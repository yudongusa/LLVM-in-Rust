//! Scalar replacement of aggregates (SROA).
//!
//! This pass implements a deliberately small, conservative SROA slice: entry
//! block struct/array allocas whose address is used only through constant-index
//! GEPs, and whose GEP results are used only by non-volatile loads/stores.
//! Replacing those aggregate slots with per-field/per-element scalar allocas
//! lets the existing mem2reg pass promote the new scalars to SSA registers.

use crate::pass::FunctionPass;
use llvm_ir::{Context, Function, InstrId, InstrKind, Instruction, TypeData, TypeId, ValueRef};
use std::collections::{HashMap, HashSet};

/// Conservative scalar replacement of aggregate allocas.
pub struct Sroa;

impl FunctionPass for Sroa {
    fn name(&self) -> &'static str {
        "sroa"
    }

    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool {
        let candidates = find_candidates(ctx, func);
        if candidates.is_empty() {
            return false;
        }

        let mut gep_replacements = HashMap::new();
        let mut remove = HashSet::new();
        let mut insertions = Vec::new();

        for candidate in candidates {
            let mut scalar_allocas = Vec::with_capacity(candidate.component_tys.len());
            for ty in candidate.component_tys {
                let name = func.fresh_name();
                let iid = func.alloc_instr(Instruction {
                    name: Some(name),
                    ty: ctx.ptr_ty,
                    kind: InstrKind::Alloca {
                        alloc_ty: ty,
                        num_elements: None,
                        align: candidate.align,
                    },
                });
                scalar_allocas.push(iid);
            }

            for (gep, index) in candidate.geps {
                gep_replacements.insert(gep, scalar_allocas[index]);
                remove.insert(gep);
            }
            remove.insert(candidate.alloca);
            insertions.push((candidate.alloca, scalar_allocas));
        }

        rewrite_load_store_pointers(func, &gep_replacements);
        rewrite_entry_block(func, &remove, insertions);
        remove_from_non_entry_blocks(func, &remove);
        true
    }
}

struct Candidate {
    alloca: InstrId,
    component_tys: Vec<TypeId>,
    align: Option<u32>,
    geps: Vec<(InstrId, usize)>,
}

fn find_candidates(ctx: &Context, func: &Function) -> Vec<Candidate> {
    let Some(entry) = func.blocks.first() else {
        return Vec::new();
    };

    entry
        .body
        .iter()
        .copied()
        .filter_map(|iid| candidate_for_alloca(ctx, func, iid))
        .collect()
}

fn candidate_for_alloca(ctx: &Context, func: &Function, alloca: InstrId) -> Option<Candidate> {
    let (alloc_ty, align) = match func.instr(alloca).kind {
        InstrKind::Alloca {
            alloc_ty,
            num_elements: None,
            align,
        } => (alloc_ty, align),
        _ => return None,
    };

    let component_tys = aggregate_components(ctx, alloc_ty)?;
    if component_tys.is_empty() {
        return None;
    }

    let alloca_ref = ValueRef::Instruction(alloca);
    let mut geps = Vec::new();

    for (idx, instr) in func.instructions.iter().enumerate() {
        let iid = InstrId(idx as u32);
        match &instr.kind {
            InstrKind::GetElementPtr {
                base_ty,
                ptr,
                indices,
                ..
            } if *ptr == alloca_ref => {
                if indices.contains(&alloca_ref) {
                    return None;
                }
                let index = constant_component_index(ctx, alloc_ty, *base_ty, indices)?;
                if index >= component_tys.len() {
                    return None;
                }
                geps.push((iid, index));
            }
            kind if kind.operands().contains(&alloca_ref) => return None,
            _ => {}
        }
    }

    if geps.is_empty() {
        return None;
    }

    for (gep, _) in &geps {
        if !gep_uses_are_load_store_only(func, *gep) {
            return None;
        }
    }

    Some(Candidate {
        alloca,
        component_tys,
        align,
        geps,
    })
}

fn aggregate_components(ctx: &Context, ty: TypeId) -> Option<Vec<TypeId>> {
    match ctx.get_type(ty) {
        TypeData::Struct(st) => Some(st.fields.clone()),
        TypeData::Array { element, len } => {
            if *len > 64 {
                return None;
            }
            Some(vec![*element; *len as usize])
        }
        _ => None,
    }
}

fn constant_component_index(
    ctx: &Context,
    alloc_ty: TypeId,
    base_ty: TypeId,
    indices: &[ValueRef],
) -> Option<usize> {
    if base_ty != alloc_ty || indices.len() != 2 {
        return None;
    }
    if const_int(ctx, indices[0])? != 0 {
        return None;
    }
    usize::try_from(const_int(ctx, indices[1])?).ok()
}

fn const_int(ctx: &Context, value: ValueRef) -> Option<u64> {
    let ValueRef::Constant(cid) = value else {
        return None;
    };
    match ctx.get_const(cid) {
        llvm_ir::ConstantData::Int { val, .. } => Some(*val),
        _ => None,
    }
}

fn gep_uses_are_load_store_only(func: &Function, gep: InstrId) -> bool {
    let gep_ref = ValueRef::Instruction(gep);
    for instr in &func.instructions {
        match &instr.kind {
            InstrKind::Load {
                ptr,
                volatile: false,
                ..
            } if *ptr == gep_ref => {}
            InstrKind::Store {
                val,
                ptr,
                volatile: false,
                ..
            } if *ptr == gep_ref && *val != gep_ref => {}
            kind if kind.operands().contains(&gep_ref) => return false,
            _ => {}
        }
    }
    true
}

fn rewrite_load_store_pointers(func: &mut Function, gep_replacements: &HashMap<InstrId, InstrId>) {
    for instr in &mut func.instructions {
        match &mut instr.kind {
            InstrKind::Load { ptr, .. } | InstrKind::Store { ptr, .. } => {
                if let ValueRef::Instruction(gep) = *ptr {
                    if let Some(&alloca) = gep_replacements.get(&gep) {
                        *ptr = ValueRef::Instruction(alloca);
                    }
                }
            }
            _ => {}
        }
    }
}

fn rewrite_entry_block(
    func: &mut Function,
    remove: &HashSet<InstrId>,
    insertions: Vec<(InstrId, Vec<InstrId>)>,
) {
    let insertion_map: HashMap<InstrId, Vec<InstrId>> = insertions.into_iter().collect();
    let entry = &mut func.blocks[0];
    let mut new_body = Vec::with_capacity(entry.body.len());
    for &iid in &entry.body {
        if let Some(scalars) = insertion_map.get(&iid) {
            new_body.extend(scalars.iter().copied());
        }
        if !remove.contains(&iid) {
            new_body.push(iid);
        }
    }
    entry.body = new_body;
}

fn remove_from_non_entry_blocks(func: &mut Function, remove: &HashSet<InstrId>) {
    for block in func.blocks.iter_mut().skip(1) {
        block.body.retain(|iid| !remove.contains(iid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mem2Reg;
    use llvm_ir::{Builder, Linkage, Module};

    fn make_pair_function() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("sroa");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "main",
            b.ctx.i32_ty,
            vec![b.ctx.i32_ty, b.ctx.i32_ty],
            vec!["a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let pair_ty = b
            .ctx
            .mk_struct_anon(vec![b.ctx.i32_ty, b.ctx.i32_ty], false);
        let zero = b.const_int(b.ctx.i32_ty, 0);
        let one = b.const_int(b.ctx.i32_ty, 1);
        let a = b.get_arg(0);
        let b_arg = b.get_arg(1);
        let pair = b.build_alloca("pair", pair_ty);
        let f0 = b.build_gep("f0", pair_ty, pair, vec![zero, zero]);
        let f1 = b.build_gep("f1", pair_ty, pair, vec![zero, one]);
        b.build_store(a, f0);
        b.build_store(b_arg, f1);
        let v0 = b.build_load("v0", b.ctx.i32_ty, f0);
        let v1 = b.build_load("v1", b.ctx.i32_ty, f1);
        let sum = b.build_add("sum", v0, v1);
        b.build_ret(sum);

        (ctx, module)
    }

    #[test]
    fn sroa_splits_pair_alloca() {
        let (mut ctx, mut module) = make_pair_function();
        let func = &mut module.functions[0];
        assert!(Sroa.run_on_function(&mut ctx, func));

        let body = &func.blocks[0].body;
        let allocas = body
            .iter()
            .filter(|&&iid| matches!(func.instr(iid).kind, InstrKind::Alloca { .. }))
            .count();
        let geps = body
            .iter()
            .filter(|&&iid| matches!(func.instr(iid).kind, InstrKind::GetElementPtr { .. }))
            .count();

        assert_eq!(
            allocas, 2,
            "aggregate alloca should become two scalar allocas"
        );
        assert_eq!(geps, 0, "constant field GEPs should be removed");
        assert!(func.instructions.iter().any(|instr| matches!(
            instr.kind,
            InstrKind::Load {
                ptr: ValueRef::Instruction(_),
                ..
            }
        )));
    }

    #[test]
    fn sroa_then_mem2reg_promotes_fields() {
        let (mut ctx, mut module) = make_pair_function();
        let func = &mut module.functions[0];
        assert!(Sroa.run_on_function(&mut ctx, func));
        assert!(Mem2Reg.run_on_function(&mut ctx, func));

        for &iid in &func.blocks[0].body {
            assert!(
                !matches!(
                    func.instr(iid).kind,
                    InstrKind::Alloca { .. } | InstrKind::Load { .. } | InstrKind::Store { .. }
                ),
                "SROA-created scalar memory should be promotable by mem2reg"
            );
        }
    }

    #[test]
    fn dynamic_gep_index_is_unchanged() {
        let mut ctx = Context::new();
        let mut module = Module::new("sroa");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "main",
            b.ctx.i32_ty,
            vec![b.ctx.i32_ty],
            vec!["idx".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let arr_ty = b.ctx.mk_array(b.ctx.i32_ty, 4);
        let zero = b.const_int(b.ctx.i32_ty, 0);
        let idx = b.get_arg(0);
        let arr = b.build_alloca("arr", arr_ty);
        let elem = b.build_gep("elem", arr_ty, arr, vec![zero, idx]);
        let val = b.build_load("val", b.ctx.i32_ty, elem);
        b.build_ret(val);

        let func = &mut module.functions[0];
        assert!(!Sroa.run_on_function(&mut ctx, func));
        assert!(func.blocks[0]
            .body
            .iter()
            .any(|&iid| matches!(func.instr(iid).kind, InstrKind::GetElementPtr { .. })));
    }

    #[test]
    fn volatile_load_is_unchanged() {
        let (mut ctx, mut module) = make_pair_function();
        let func = &mut module.functions[0];
        for instr in &mut func.instructions {
            if let InstrKind::Load { volatile, .. } = &mut instr.kind {
                *volatile = true;
                break;
            }
        }
        assert!(!Sroa.run_on_function(&mut ctx, func));
    }
}
