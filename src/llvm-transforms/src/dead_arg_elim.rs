//! Inter-procedural dead argument elimination.

use crate::pass::ModulePass;
use llvm_ir::{
    Context, FunctionId, GlobalId, InstrKind, Module, TypeData, ValueRef,
};

/// Removes trailing unused function parameters and rewrites direct callsites.
///
/// This intentionally targets only trailing dead args to avoid argument
/// reindexing across function bodies.
pub struct DeadArgElim;

impl ModulePass for DeadArgElim {
    fn name(&self) -> &'static str {
        "dead-arg-elim"
    }

    fn run_on_module(&mut self, ctx: &mut Context, module: &mut Module) -> bool {
        let mut changed = false;
        let mut updates: Vec<(FunctionId, usize)> = Vec::new();

        for (fi, f) in module.functions.iter().enumerate() {
            if f.is_declaration || f.args.is_empty() {
                continue;
            }
            let mut max_used: Option<usize> = None;
            for instr in &f.instructions {
                for op in instr.kind.operands() {
                    if let ValueRef::Argument(aid) = op {
                        let idx = aid.0 as usize;
                        max_used = Some(max_used.map_or(idx, |m| m.max(idx)));
                    }
                }
            }
            let keep_len = max_used.map_or(0, |m| m + 1).min(f.args.len());
            if keep_len < f.args.len() {
                updates.push((FunctionId(fi as u32), keep_len));
            }
        }

        for (fid, keep_len) in updates {
            let f = &mut module.functions[fid.0 as usize];
            let old_len = f.args.len();
            if keep_len >= old_len {
                continue;
            }
            f.args.truncate(keep_len);
            f.arg_names.retain(|_, aid| (aid.0 as usize) < keep_len);

            if let TypeData::Function(ft) = ctx.get_type(f.ty).clone() {
                let mut params = ft.params;
                params.truncate(keep_len);
                f.ty = ctx.mk_fn_type(ft.ret, params, ft.variadic);
            }
            let new_ty = f.ty;

            for caller in &mut module.functions {
                for instr in &mut caller.instructions {
                    let InstrKind::Call {
                        callee,
                        callee_ty,
                        args,
                        ..
                    } = &mut instr.kind
                    else {
                        continue;
                    };
                    if *callee == ValueRef::Global(GlobalId(fid.0)) {
                        args.truncate(keep_len);
                        *callee_ty = new_ty;
                    }
                }
            }
            changed = true;
        }

        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Linkage};

    #[test]
    fn dead_arg_elim_removes_trailing_unused_arg_and_rewrites_calls() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);

        b.add_function(
            "f",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["a".into(), "b".into(), "dead".into()],
            false,
            Linkage::External,
        );
        let f_entry = b.add_block("f.entry");
        b.position_at_end(f_entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let s = b.build_add("s", a, bv);
        b.build_ret(s);

        b.add_function(
            "caller",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let c_entry = b.add_block("caller.entry");
        b.position_at_end(c_entry);
        let x = b.get_arg(0);
        let c1 = b.const_int(b.ctx.i64_ty, 1);
        let c2 = b.const_int(b.ctx.i64_ty, 2);
        let call_ty = b
            .ctx
            .mk_fn_type(b.ctx.i64_ty, vec![b.ctx.i64_ty, b.ctx.i64_ty, b.ctx.i64_ty], false);
        let r = b.build_call(
            "r",
            b.ctx.i64_ty,
            call_ty,
            ValueRef::Global(GlobalId(0)),
            vec![x, c1, c2],
        );
        b.build_ret(r);

        let mut pass = DeadArgElim;
        let changed = pass.run_on_module(&mut ctx, &mut module);
        assert!(changed);
        assert_eq!(module.functions[0].args.len(), 2);
        let call = module.functions[1]
            .instructions
            .iter()
            .find(|i| matches!(i.kind, InstrKind::Call { .. }))
            .expect("call exists");
        if let InstrKind::Call { args, .. } = &call.kind {
            assert_eq!(args.len(), 2);
        } else {
            panic!("expected call");
        }
    }
}
