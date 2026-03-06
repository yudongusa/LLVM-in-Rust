//! Inter-procedural constant propagation (IPCP).

use crate::{const_prop::ConstProp, pass::ModulePass, value_rewrite::rewrite_values_in_kind};
use llvm_ir::{
    ArgId, Context, Function, FunctionId, GlobalId, InstrKind, Module, ValueRef,
};

/// Simple IPCP pass:
/// - detect a direct callee argument that is constant across all direct callsites
/// - clone callee and substitute that argument with the constant
/// - redirect matching callsites to the specialized clone
pub struct Ipcp;

impl ModulePass for Ipcp {
    fn name(&self) -> &'static str {
        "ipcp"
    }

    fn run_on_module(&mut self, ctx: &mut Context, module: &mut Module) -> bool {
        let mut changed = false;

        for callee_idx in 0..module.functions.len() {
            let callee_id = FunctionId(callee_idx as u32);
            if module.functions[callee_idx].is_declaration || module.functions[callee_idx].args.is_empty() {
                continue;
            }

            let callsites = collect_direct_calls(module, callee_id);
            if callsites.is_empty() {
                continue;
            }
            let Some((arg_idx, const_val)) = find_constant_arg(&callsites) else {
                continue;
            };

            let spec_name = format!(
                "{}.ipcp.a{}.c{}",
                module.functions[callee_idx].name, arg_idx, const_val.0
            );
            let spec_id = if let Some(fid) = module.get_function_id(&spec_name) {
                fid
            } else {
                let mut spec = clone_specialized_function(
                    &module.functions[callee_idx],
                    &spec_name,
                    ArgId(arg_idx as u32),
                    ValueRef::Constant(const_val),
                );
                let mut cp = ConstProp;
                let _ = crate::pass::FunctionPass::run_on_function(&mut cp, ctx, &mut spec);
                module.add_function(spec)
            };

            let spec_ty = module.functions[spec_id.0 as usize].ty;
            for cs in callsites {
                if cs.const_args.get(arg_idx).copied() == Some(Some(const_val)) {
                    let instr = &mut module.functions[cs.caller.0 as usize].instructions[cs.iid.0 as usize];
                    if let InstrKind::Call { callee, callee_ty, .. } = &mut instr.kind {
                        *callee = ValueRef::Global(GlobalId(spec_id.0));
                        *callee_ty = spec_ty;
                        changed = true;
                    }
                }
            }
        }

        changed
    }
}

#[derive(Clone)]
struct DirectCallSite {
    caller: FunctionId,
    iid: llvm_ir::InstrId,
    const_args: Vec<Option<llvm_ir::ConstId>>,
}

fn collect_direct_calls(module: &Module, callee_id: FunctionId) -> Vec<DirectCallSite> {
    let mut out = Vec::new();
    for (caller_idx, f) in module.functions.iter().enumerate() {
        if f.is_declaration {
            continue;
        }
        for (iid_idx, instr) in f.instructions.iter().enumerate() {
            let InstrKind::Call { callee, args, .. } = &instr.kind else {
                continue;
            };
            if *callee != ValueRef::Global(GlobalId(callee_id.0)) {
                continue;
            }
            let const_args = args
                .iter()
                .map(|a| match a {
                    ValueRef::Constant(c) => Some(*c),
                    _ => None,
                })
                .collect();
            out.push(DirectCallSite {
                caller: FunctionId(caller_idx as u32),
                iid: llvm_ir::InstrId(iid_idx as u32),
                const_args,
            });
        }
    }
    out
}

fn find_constant_arg(callsites: &[DirectCallSite]) -> Option<(usize, llvm_ir::ConstId)> {
    let argc = callsites.first()?.const_args.len();
    for ai in 0..argc {
        let mut cst: Option<llvm_ir::ConstId> = None;
        let mut ok = true;
        for cs in callsites {
            match cs.const_args.get(ai).copied().flatten() {
                Some(c) => {
                    if let Some(prev) = cst {
                        if prev != c {
                            ok = false;
                            break;
                        }
                    } else {
                        cst = Some(c);
                    }
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return cst.map(|c| (ai, c));
        }
    }
    None
}

fn clone_specialized_function(
    src: &Function,
    new_name: &str,
    arg_id: ArgId,
    const_val: ValueRef,
) -> Function {
    let mut dst = Function::new(new_name.to_string(), src.ty, src.args.clone(), src.linkage);
    dst.blocks = src.blocks.clone();
    dst.instructions = src.instructions.clone();
    dst.value_names = src.value_names.clone();
    dst.arg_names = src.arg_names.clone();
    dst.is_declaration = false;

    for instr in &mut dst.instructions {
        let old = instr.kind.clone();
        instr.kind = rewrite_values_in_kind(old, |v| {
            if v == ValueRef::Argument(arg_id) {
                const_val
            } else {
                v
            }
        });
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Linkage};

    #[test]
    fn ipcp_specializes_constant_argument_and_rewrites_calls() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);

        b.add_function(
            "addk",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["x".into(), "k".into()],
            false,
            Linkage::External,
        );
        let addk_entry = b.add_block("addk.entry");
        b.position_at_end(addk_entry);
        let x = b.get_arg(0);
        let k = b.get_arg(1);
        let s = b.build_add("s", x, k);
        b.build_ret(s);

        b.add_function(
            "caller",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let caller_entry = b.add_block("caller.entry");
        b.position_at_end(caller_entry);
        let x0 = b.get_arg(0);
        let c7 = b.const_int(b.ctx.i64_ty, 7);
        let c7b = b.const_int(b.ctx.i64_ty, 7);
        let call_ty = b.ctx.mk_fn_type(b.ctx.i64_ty, vec![b.ctx.i64_ty, b.ctx.i64_ty], false);
        let t1 = b.build_call(
            "t1",
            b.ctx.i64_ty,
            call_ty,
            ValueRef::Global(GlobalId(0)),
            vec![x0, c7],
        );
        let t2 = b.build_call(
            "t2",
            b.ctx.i64_ty,
            call_ty,
            ValueRef::Global(GlobalId(0)),
            vec![t1, c7],
        );
        let _t3 = b.build_call(
            "t3",
            b.ctx.i64_ty,
            call_ty,
            ValueRef::Global(GlobalId(0)),
            vec![t2, c7b],
        );
        b.build_ret(t2);

        let mut pass = Ipcp;
        let changed = pass.run_on_module(&mut ctx, &mut module);
        assert!(changed);
        assert!(
            module.functions.iter().any(|f| f.name.starts_with("addk.ipcp")),
            "expected specialized clone"
        );
    }
}
