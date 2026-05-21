//! Hello-world example: build a trivial IR function, run O1 optimizations, print instruction count.

use llvm_ir::{Builder, Context, Linkage, Module};
use llvm_transforms::{build_pipeline, OptLevel};

fn main() {
    let mut ctx = Context::new();
    let mut module = Module::new("hello");
    let mut b = Builder::new(&mut ctx, &mut module);
    let i32_ty = b.ctx.i32_ty;
    b.add_function("main", i32_ty, vec![], vec![], false, Linkage::External);
    let entry = b.add_block("entry");
    b.position_at_end(entry);
    let c = b.const_int(i32_ty, 0);
    b.build_ret(c);
    drop(b);

    let mut pm = build_pipeline(OptLevel::O1);
    pm.run_until_fixed_point(&mut ctx, &mut module, 4);
    println!(
        "main: {} instructions",
        module.functions[0].instructions.len()
    );
}
