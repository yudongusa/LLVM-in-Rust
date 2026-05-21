//! Opt-pipeline example: run O2 on dead-code IR and observe the size reduction.

use llvm_ir::{Builder, Context, Linkage, Module};
use llvm_transforms::{build_pipeline, OptLevel};

fn main() {
    let mut ctx = Context::new();
    let mut module = Module::new("opt_demo");
    let mut b = Builder::new(&mut ctx, &mut module);
    let i32_ty = b.ctx.i32_ty;
    b.add_function("f", i32_ty, vec![], vec![], false, Linkage::External);
    let entry = b.add_block("entry");
    b.position_at_end(entry);
    let c1 = b.const_int(i32_ty, 3);
    let c2 = b.const_int(i32_ty, 4);
    // This add is dead: its result is never used.
    let _dead = b.build_add("dead", c1, c2);
    let ret_val = b.const_int(i32_ty, 42);
    b.build_ret(ret_val);
    drop(b);

    let before = module.functions[0].blocks[0].body.len();
    let mut pm = build_pipeline(OptLevel::O2);
    pm.run_until_fixed_point(&mut ctx, &mut module, 8);
    let after = module.functions[0].blocks[0].body.len();
    println!("Body size: {} -> {} (dead code eliminated)", before, after);
}
