//! Alias analysis: TBAA type-hierarchy queries and BasicAA pointer
//! disambiguation.
//!
//! # Overview
//!
//! This module provides two cooperating analyses:
//!
//! 1. **TBAA** ([`TbaaHierarchy`]) — models the C/C++ type-based alias
//!    analysis metadata (`!tbaa`).  Given two [`TbaaAccessTag`]s, it answers
//!    whether the underlying type rules *allow* the two accesses to alias.
//!
//! 2. **BasicAA** ([`BasicAliasAnalysis`]) — performs lightweight pointer
//!    disambiguation by chasing the definition chain to an *underlying object*
//!    (`alloca`, global, or argument).  Two pointers that provably point to
//!    different underlying objects cannot alias.
//!
//! Both analyses implement the [`AliasAnalysis`] trait, which returns an
//! [`AliasResult`].  [`CombinedAliasAnalysis`] runs BasicAA first and returns
//! its answer (TBAA can be layered on later if `!tbaa` metadata is parsed).

use llvm_ir::{ArgId, Function, GlobalId, InstrId, Module, ValueRef};
use llvm_ir::instruction::InstrKind;

// ---------------------------------------------------------------------------
// AliasResult
// ---------------------------------------------------------------------------

/// The three-valued result of an alias query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasResult {
    /// The two memory locations definitely do not overlap.
    NoAlias,
    /// The two memory locations may or may not overlap (conservative).
    MayAlias,
    /// The two memory locations definitely refer to the exact same bytes.
    MustAlias,
}

// ---------------------------------------------------------------------------
// MemAccess
// ---------------------------------------------------------------------------

/// A memory access used for alias queries.
pub struct MemAccess {
    /// The pointer operand.
    pub ptr: ValueRef,
    /// Access size in bytes; `0` means unknown / unbounded.
    pub size_bytes: u64,
}

// ---------------------------------------------------------------------------
// AliasAnalysis trait
// ---------------------------------------------------------------------------

/// Common interface implemented by every alias analysis.
pub trait AliasAnalysis {
    /// Returns whether memory accesses `a` and `b` may alias.
    fn may_alias(&self, a: &MemAccess, b: &MemAccess) -> AliasResult;
}

// ---------------------------------------------------------------------------
// TBAA
// ---------------------------------------------------------------------------

/// A single node in the TBAA type hierarchy.
///
/// The hierarchy is a forest of nodes rooted at one or more "root" nodes
/// (nodes with no parent).  Scalar types form simple chains; struct-path types
/// have a base node plus an offset into that struct.
#[derive(Debug, Clone)]
pub struct TbaaNode {
    /// Unique id within the [`TbaaHierarchy`].
    pub id: u32,
    /// Human-readable name (matches the LLVM metadata string).
    pub name: String,
    /// Id of the immediate parent node, or `None` for root nodes.
    pub parent: Option<u32>,
}

/// An access tag that describes the TBAA type metadata attached to a single
/// load or store instruction.
#[derive(Debug, Clone)]
pub struct TbaaAccessTag {
    /// Id of the "base type" node — the struct or aggregate being accessed.
    pub base_type: u32,
    /// Id of the "access type" node — the scalar type of the access itself.
    pub access_type: u32,
    /// Byte offset of the access within the base type.
    pub offset: i64,
}

/// The complete TBAA type hierarchy for a compilation unit.
///
/// Constructed once per module from the `!tbaa` metadata nodes; queried for
/// every alias query that has TBAA tags on both accesses.
pub struct TbaaHierarchy {
    /// All TBAA nodes, indexed by their `id` field.
    pub nodes: Vec<TbaaNode>,
}

impl TbaaHierarchy {
    /// Create an empty hierarchy.
    pub fn new() -> Self {
        TbaaHierarchy { nodes: Vec::new() }
    }

    /// Returns `true` if `ancestor_id` is an ancestor of (or equal to)
    /// `descendant_id` in the type hierarchy.
    ///
    /// The search walks up through `parent` links.  If a node id is not found
    /// in `self.nodes` the walk stops (treats as root).
    pub fn is_ancestor(&self, ancestor_id: u32, descendant_id: u32) -> bool {
        let mut current = descendant_id;
        loop {
            if current == ancestor_id {
                return true;
            }
            // Walk up to the parent.
            match self.nodes.iter().find(|n| n.id == current) {
                Some(node) => match node.parent {
                    Some(p) => current = p,
                    None => return false, // reached a root without finding ancestor
                },
                None => return false, // unknown node id
            }
        }
    }

    /// Determine whether two access tags may alias.
    ///
    /// Rules (applied in order):
    /// 1. If either access type is a "char" / "omnipotent char" type (per C
    ///    rules, `char *` can alias anything) → [`AliasResult::MayAlias`].
    /// 2. If one base type is an ancestor of the other → [`AliasResult::MayAlias`]
    ///    (they share a type lineage so aliasing is possible).
    /// 3. Otherwise → [`AliasResult::NoAlias`].
    pub fn may_alias(&self, a: &TbaaAccessTag, b: &TbaaAccessTag) -> AliasResult {
        // Rule 1: char / omnipotent-char aliases everything.
        if self.is_char_type(a.access_type) || self.is_char_type(b.access_type) {
            return AliasResult::MayAlias;
        }

        // Rule 2: shared type lineage.
        if self.is_ancestor(a.base_type, b.base_type)
            || self.is_ancestor(b.base_type, a.base_type)
        {
            return AliasResult::MayAlias;
        }

        AliasResult::NoAlias
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Returns `true` if `id` names a "char" or "omnipotent char" type.
    fn is_char_type(&self, id: u32) -> bool {
        match self.nodes.iter().find(|n| n.id == id) {
            Some(node) => {
                let name_lower = node.name.to_lowercase();
                name_lower.contains("char") || name_lower.contains("omnipotent")
            }
            None => false,
        }
    }
}

impl Default for TbaaHierarchy {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// UnderlyingObject
// ---------------------------------------------------------------------------

/// The provenance origin of a pointer value.
///
/// Used by [`BasicAliasAnalysis`] to prove non-aliasing between pointers
/// that derive from structurally distinct memory objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnderlyingObject {
    /// The pointer comes from an `alloca` instruction with the given id.
    Alloca(InstrId),
    /// The pointer is a reference to a global variable.
    Global(GlobalId),
    /// The pointer came in as a function argument.
    Arg(ArgId),
    /// The origin could not be determined statically.
    Unknown,
}

/// Walk the SSA def-chain of `val` to find its underlying memory object.
///
/// Only two-address chain steps are followed:
/// - `GetElementPtr { ptr, .. }` — peel through GEP and recurse on `ptr`.
/// - `Alloca` — the chain terminates with [`UnderlyingObject::Alloca`].
/// - `Load` — can't track through a loaded pointer; return `Unknown`.
/// - `ValueRef::Global(gid)` — return [`UnderlyingObject::Global`].
/// - `ValueRef::Argument(aid)` — return [`UnderlyingObject::Arg`].
/// - Everything else — return `Unknown`.
///
/// The recursion is bounded implicitly because SSA is acyclic (no infinite
/// chains in well-formed IR).
pub fn underlying_object(
    val: ValueRef,
    func: &Function,
    _module: &Module,
) -> UnderlyingObject {
    match val {
        ValueRef::Global(gid) => UnderlyingObject::Global(gid),
        ValueRef::Argument(aid) => UnderlyingObject::Arg(aid),
        ValueRef::Constant(_) => UnderlyingObject::Unknown,
        ValueRef::Instruction(iid) => {
            let instr = &func.instructions[iid.0 as usize];
            match &instr.kind {
                InstrKind::Alloca { .. } => UnderlyingObject::Alloca(iid),
                InstrKind::GetElementPtr { ptr, .. } => {
                    // Recursively strip GEPs.
                    underlying_object(*ptr, func, _module)
                }
                InstrKind::Load { .. } => {
                    // Loaded pointer — provenance lost.
                    UnderlyingObject::Unknown
                }
                // Phi, Select, BitCast, and all others — conservative.
                _ => UnderlyingObject::Unknown,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// BasicAliasAnalysis
// ---------------------------------------------------------------------------

/// Lightweight alias analysis that disambiguates pointers by comparing their
/// underlying memory objects.
///
/// Rules:
/// 1. Two distinct `Alloca` origins → [`AliasResult::NoAlias`].
/// 2. `Alloca` vs `Global` → [`AliasResult::NoAlias`].
/// 3. Two distinct `Global` origins → [`AliasResult::NoAlias`].
/// 4. Same underlying object (Alloca == Alloca, Global == Global, Arg == Arg) →
///    [`AliasResult::MayAlias`] (could be the same bytes with different offsets).
/// 5. Anything `Unknown` → [`AliasResult::MayAlias`].
pub struct BasicAliasAnalysis<'a> {
    /// The function whose pointers are being analysed.
    pub func: &'a Function,
    /// The enclosing module (for global variable information).
    pub module: &'a Module,
}

impl<'a> BasicAliasAnalysis<'a> {
    /// Construct a new `BasicAliasAnalysis` for `func` within `module`.
    pub fn new(func: &'a Function, module: &'a Module) -> Self {
        BasicAliasAnalysis { func, module }
    }
}

impl<'a> AliasAnalysis for BasicAliasAnalysis<'a> {
    fn may_alias(&self, a: &MemAccess, b: &MemAccess) -> AliasResult {
        let oa = underlying_object(a.ptr, self.func, self.module);
        let ob = underlying_object(b.ptr, self.func, self.module);

        match (&oa, &ob) {
            // Two *different* allocas — definitely distinct stack slots.
            (UnderlyingObject::Alloca(ia), UnderlyingObject::Alloca(ib)) if ia != ib => {
                AliasResult::NoAlias
            }
            // Alloca vs global — stack vs static, never overlap.
            (UnderlyingObject::Alloca(_), UnderlyingObject::Global(_))
            | (UnderlyingObject::Global(_), UnderlyingObject::Alloca(_)) => AliasResult::NoAlias,
            // Two *different* globals — distinct objects.
            (UnderlyingObject::Global(ga), UnderlyingObject::Global(gb)) if ga != gb => {
                AliasResult::NoAlias
            }
            // Arg vs Alloca — argument pointer vs local stack slot.
            (UnderlyingObject::Arg(_), UnderlyingObject::Alloca(_))
            | (UnderlyingObject::Alloca(_), UnderlyingObject::Arg(_)) => AliasResult::NoAlias,
            // All other combinations: same object or unknown → conservative.
            _ => AliasResult::MayAlias,
        }
    }
}

// ---------------------------------------------------------------------------
// CombinedAliasAnalysis
// ---------------------------------------------------------------------------

/// Runs multiple alias analyses in sequence, returning the strongest result.
///
/// Currently wraps [`BasicAliasAnalysis`].  TBAA can be layered on top in a
/// future PR once the `!tbaa` metadata is parsed from the IR text.
pub struct CombinedAliasAnalysis<'a> {
    /// The underlying BasicAA pass.
    pub basic: BasicAliasAnalysis<'a>,
}

impl<'a> CombinedAliasAnalysis<'a> {
    /// Create a new `CombinedAliasAnalysis` for `func` within `module`.
    pub fn new(func: &'a Function, module: &'a Module) -> Self {
        CombinedAliasAnalysis {
            basic: BasicAliasAnalysis::new(func, module),
        }
    }
}

impl<'a> AliasAnalysis for CombinedAliasAnalysis<'a> {
    fn may_alias(&self, a: &MemAccess, b: &MemAccess) -> AliasResult {
        // BasicAA first: if it proves NoAlias, we're done.
        let r = self.basic.may_alias(a, b);
        if r == AliasResult::NoAlias {
            return AliasResult::NoAlias;
        }
        // Future: run TBAA here when metadata is available.
        AliasResult::MayAlias
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{
        BasicBlock, Context, Function, Instruction, InstrKind, Linkage, Module, ValueRef,
    };
    use llvm_ir::value::GlobalVariable;

    // -----------------------------------------------------------------------
    // Helper: build a minimal 1-block function with the provided instructions
    // -----------------------------------------------------------------------

    fn make_func_and_module(
        ctx: &mut Context,
        build: impl FnOnce(&mut Context, &mut Function),
    ) -> (Function, Module) {
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let mut func = Function::new("test", fn_ty, vec![], Linkage::External);
        func.add_block(BasicBlock::new("entry"));
        build(ctx, &mut func);
        let module = Module::new("test_module");
        (func, module)
    }

    // -----------------------------------------------------------------------
    // TBAA tests (4 tests)
    // -----------------------------------------------------------------------

    fn make_hierarchy() -> TbaaHierarchy {
        // root (id=0) -> int (id=1), float (id=2)
        TbaaHierarchy {
            nodes: vec![
                TbaaNode { id: 0, name: "TBAA root".to_string(), parent: None },
                TbaaNode { id: 1, name: "int".to_string(), parent: Some(0) },
                TbaaNode { id: 2, name: "float".to_string(), parent: Some(0) },
            ],
        }
    }

    #[test]
    fn tbaa_ancestor_of_self() {
        let h = make_hierarchy();
        // Every node is an ancestor of itself.
        assert!(h.is_ancestor(0, 0));
        assert!(h.is_ancestor(1, 1));
        assert!(h.is_ancestor(2, 2));
    }

    #[test]
    fn tbaa_parent_is_ancestor() {
        let h = make_hierarchy();
        // root (0) is an ancestor of int (1) and float (2).
        assert!(h.is_ancestor(0, 1));
        assert!(h.is_ancestor(0, 2));
        // int is NOT an ancestor of float.
        assert!(!h.is_ancestor(1, 2));
        assert!(!h.is_ancestor(2, 1));
    }

    #[test]
    fn tbaa_no_alias_different_types() {
        let h = make_hierarchy();
        // int (1) and float (2) share only the root ancestor, but neither is
        // an ancestor of the other, so they do not alias.
        let a = TbaaAccessTag { base_type: 1, access_type: 1, offset: 0 };
        let b = TbaaAccessTag { base_type: 2, access_type: 2, offset: 0 };
        assert_eq!(h.may_alias(&a, &b), AliasResult::NoAlias);
    }

    #[test]
    fn tbaa_char_aliases_anything() {
        // A node named "omnipotent char" must alias with everything.
        let h = TbaaHierarchy {
            nodes: vec![
                TbaaNode { id: 0, name: "TBAA root".to_string(), parent: None },
                TbaaNode { id: 1, name: "int".to_string(), parent: Some(0) },
                TbaaNode { id: 2, name: "omnipotent char".to_string(), parent: Some(0) },
            ],
        };
        // char (2) vs int (1) — char wins.
        let a = TbaaAccessTag { base_type: 2, access_type: 2, offset: 0 };
        let b = TbaaAccessTag { base_type: 1, access_type: 1, offset: 0 };
        assert_eq!(h.may_alias(&a, &b), AliasResult::MayAlias);
        // Also works in reverse.
        assert_eq!(h.may_alias(&b, &a), AliasResult::MayAlias);
    }

    // -----------------------------------------------------------------------
    // BasicAA / underlying_object tests (8 tests)
    // -----------------------------------------------------------------------

    #[test]
    fn underlying_object_alloca() {
        let mut ctx = Context::new();
        let (func, module) = make_func_and_module(&mut ctx, |ctx, func| {
            let alloca_instr = Instruction {
                name: Some("a".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Alloca {
                    alloc_ty: ctx.i32_ty,
                    num_elements: None,
                    align: None,
                },
            };
            let iid = func.alloc_instr(alloca_instr);
            func.blocks[0].body.push(iid);
        });

        let iid = func.value_names["a"];
        let val = ValueRef::Instruction(iid);
        assert_eq!(underlying_object(val, &func, &module), UnderlyingObject::Alloca(iid));
    }

    #[test]
    fn underlying_object_global() {
        let mut ctx = Context::new();
        let mut module = Module::new("test_module");
        let gv = GlobalVariable {
            name: "g".to_string(),
            ty: ctx.i32_ty,
            initializer: None,
            is_constant: false,
            linkage: Linkage::External,
        };
        let gid = module.add_global(gv);
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let func = Function::new("f", fn_ty, vec![], Linkage::External);

        let val = ValueRef::Global(gid);
        assert_eq!(underlying_object(val, &func, &module), UnderlyingObject::Global(gid));
    }

    #[test]
    fn underlying_object_gep_from_alloca() {
        // GEP whose base is an alloca should resolve to Alloca.
        let mut ctx = Context::new();
        let (func, module) = make_func_and_module(&mut ctx, |ctx, func| {
            // %a = alloca i32
            let alloca_instr = Instruction {
                name: Some("a".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Alloca {
                    alloc_ty: ctx.i32_ty,
                    num_elements: None,
                    align: None,
                },
            };
            let alloca_iid = func.alloc_instr(alloca_instr);
            func.blocks[0].body.push(alloca_iid);

            // %g = getelementptr i32, ptr %a, i64 0
            let zero = ValueRef::Constant(ctx.const_int(ctx.i64_ty, 0));
            let gep_instr = Instruction {
                name: Some("g".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::GetElementPtr {
                    inbounds: true,
                    base_ty: ctx.i32_ty,
                    ptr: ValueRef::Instruction(alloca_iid),
                    indices: vec![zero],
                },
            };
            let gep_iid = func.alloc_instr(gep_instr);
            func.blocks[0].body.push(gep_iid);
        });

        let alloca_iid = func.value_names["a"];
        let gep_iid = func.value_names["g"];
        let gep_ref = ValueRef::Instruction(gep_iid);
        // GEP should resolve through to the underlying alloca.
        assert_eq!(
            underlying_object(gep_ref, &func, &module),
            UnderlyingObject::Alloca(alloca_iid)
        );
    }

    #[test]
    fn basicaa_two_different_allocas_no_alias() {
        let mut ctx = Context::new();
        let (func, module) = make_func_and_module(&mut ctx, |ctx, func| {
            for name in ["a", "b"] {
                let instr = Instruction {
                    name: Some(name.to_string()),
                    ty: ctx.ptr_ty,
                    kind: InstrKind::Alloca {
                        alloc_ty: ctx.i32_ty,
                        num_elements: None,
                        align: None,
                    },
                };
                let iid = func.alloc_instr(instr);
                func.blocks[0].body.push(iid);
            }
        });
        let aa = BasicAliasAnalysis::new(&func, &module);
        let ma = MemAccess {
            ptr: ValueRef::Instruction(func.value_names["a"]),
            size_bytes: 4,
        };
        let mb = MemAccess {
            ptr: ValueRef::Instruction(func.value_names["b"]),
            size_bytes: 4,
        };
        assert_eq!(aa.may_alias(&ma, &mb), AliasResult::NoAlias);
    }

    #[test]
    fn basicaa_same_alloca_may_alias() {
        let mut ctx = Context::new();
        let (func, module) = make_func_and_module(&mut ctx, |ctx, func| {
            let instr = Instruction {
                name: Some("a".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Alloca {
                    alloc_ty: ctx.i32_ty,
                    num_elements: None,
                    align: None,
                },
            };
            let iid = func.alloc_instr(instr);
            func.blocks[0].body.push(iid);
        });
        let aa = BasicAliasAnalysis::new(&func, &module);
        let aref = ValueRef::Instruction(func.value_names["a"]);
        let ma = MemAccess { ptr: aref, size_bytes: 4 };
        let mb = MemAccess { ptr: aref, size_bytes: 4 };
        // Same alloca — could be same bytes, so MayAlias.
        assert_eq!(aa.may_alias(&ma, &mb), AliasResult::MayAlias);
    }

    #[test]
    fn basicaa_alloca_vs_global_no_alias() {
        let mut ctx = Context::new();
        let mut module = Module::new("test_module");
        let gv = GlobalVariable {
            name: "g".to_string(),
            ty: ctx.i32_ty,
            initializer: None,
            is_constant: false,
            linkage: Linkage::External,
        };
        let gid = module.add_global(gv);
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let mut func = Function::new("f", fn_ty, vec![], Linkage::External);
        func.add_block(BasicBlock::new("entry"));
        let alloca_instr = Instruction {
            name: Some("a".to_string()),
            ty: ctx.ptr_ty,
            kind: InstrKind::Alloca {
                alloc_ty: ctx.i32_ty,
                num_elements: None,
                align: None,
            },
        };
        let iid = func.alloc_instr(alloca_instr);
        func.blocks[0].body.push(iid);

        let aa = BasicAliasAnalysis::new(&func, &module);
        let ma = MemAccess {
            ptr: ValueRef::Instruction(func.value_names["a"]),
            size_bytes: 4,
        };
        let mb = MemAccess { ptr: ValueRef::Global(gid), size_bytes: 4 };
        assert_eq!(aa.may_alias(&ma, &mb), AliasResult::NoAlias);
    }

    #[test]
    fn basicaa_gep_from_alloca_no_alias_vs_other_alloca() {
        // GEP(alloca A, 0) vs alloca B → NoAlias (A ≠ B).
        let mut ctx = Context::new();
        let (func, module) = make_func_and_module(&mut ctx, |ctx, func| {
            // %a = alloca i32
            let a = Instruction {
                name: Some("a".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Alloca {
                    alloc_ty: ctx.i32_ty,
                    num_elements: None,
                    align: None,
                },
            };
            let a_iid = func.alloc_instr(a);
            func.blocks[0].body.push(a_iid);

            // %b = alloca i32
            let b = Instruction {
                name: Some("b".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Alloca {
                    alloc_ty: ctx.i32_ty,
                    num_elements: None,
                    align: None,
                },
            };
            let b_iid = func.alloc_instr(b);
            func.blocks[0].body.push(b_iid);

            // %gep = getelementptr i32, ptr %a, i64 0
            let zero = ValueRef::Constant(ctx.const_int(ctx.i64_ty, 0));
            let gep = Instruction {
                name: Some("gep".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::GetElementPtr {
                    inbounds: true,
                    base_ty: ctx.i32_ty,
                    ptr: ValueRef::Instruction(a_iid),
                    indices: vec![zero],
                },
            };
            let gep_iid = func.alloc_instr(gep);
            func.blocks[0].body.push(gep_iid);
        });

        let aa = BasicAliasAnalysis::new(&func, &module);
        let ma = MemAccess {
            ptr: ValueRef::Instruction(func.value_names["gep"]),
            size_bytes: 4,
        };
        let mb = MemAccess {
            ptr: ValueRef::Instruction(func.value_names["b"]),
            size_bytes: 4,
        };
        // gep derives from alloca A; mb derives from alloca B → NoAlias.
        assert_eq!(aa.may_alias(&ma, &mb), AliasResult::NoAlias);
    }

    #[test]
    fn basicaa_unknown_vs_unknown_may_alias() {
        // Two loaded pointers — underlying object unknown for both → MayAlias.
        let mut ctx = Context::new();
        let (func, module) = make_func_and_module(&mut ctx, |ctx, func| {
            // %ptr_slot = alloca ptr
            let slot = Instruction {
                name: Some("slot".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Alloca {
                    alloc_ty: ctx.ptr_ty,
                    num_elements: None,
                    align: None,
                },
            };
            let slot_iid = func.alloc_instr(slot);
            func.blocks[0].body.push(slot_iid);

            // %p = load ptr, ptr %slot    (loaded pointer — Unknown)
            let p = Instruction {
                name: Some("p".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Load {
                    ty: ctx.ptr_ty,
                    ptr: ValueRef::Instruction(slot_iid),
                    align: None,
                    volatile: false,
                },
            };
            let p_iid = func.alloc_instr(p);
            func.blocks[0].body.push(p_iid);

            // %q = load ptr, ptr %slot    (another loaded pointer — Unknown)
            let q = Instruction {
                name: Some("q".to_string()),
                ty: ctx.ptr_ty,
                kind: InstrKind::Load {
                    ty: ctx.ptr_ty,
                    ptr: ValueRef::Instruction(slot_iid),
                    align: None,
                    volatile: false,
                },
            };
            let q_iid = func.alloc_instr(q);
            func.blocks[0].body.push(q_iid);
        });

        let aa = BasicAliasAnalysis::new(&func, &module);
        let ma = MemAccess {
            ptr: ValueRef::Instruction(func.value_names["p"]),
            size_bytes: 4,
        };
        let mb = MemAccess {
            ptr: ValueRef::Instruction(func.value_names["q"]),
            size_bytes: 4,
        };
        assert_eq!(aa.may_alias(&ma, &mb), AliasResult::MayAlias);
    }
}
