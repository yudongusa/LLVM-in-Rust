use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{allocate_registers, apply_allocation, compute_live_intervals, RegAllocStrategy},
    ObjectFormat,
};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};

const DBG_LL: &str = r#"
source_filename = "cv_dbg_test.c"
define i32 @main() {
entry:
  ret i32 0, !dbg !12
}
!12 = !DILocation(line: 42, column: 7, scope: !1)
"#;

#[test]
fn emits_codeview_debug_s_for_coff_when_dbg_metadata_present() {
    let (ctx, module) = parse(DBG_LL).expect("parse test ir");
    let func = module
        .functions
        .iter()
        .find(|f| !f.is_declaration)
        .expect("one definition must exist");

    let mut backend = X86Backend::default();
    let mut mf = backend.lower_function(&ctx, &module, func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    llvm_codegen::regalloc::insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);

    let mut emitter = X86Emitter::new(ObjectFormat::Coff);
    let obj = emit_object(&mf, &mut emitter);

    let cv = obj
        .sections
        .iter()
        .find(|s| s.name == ".debug$S")
        .expect("COFF object must include .debug$S when debug metadata exists");
    assert!(cv.data.len() >= 12, "codeview payload too small");
    assert_eq!(&cv.data[0..4], &[4, 0, 0, 0], "CV_SIGNATURE_C13");
    assert_eq!(
        u32::from_le_bytes([cv.data[4], cv.data[5], cv.data[6], cv.data[7]]),
        0xF1,
        "expected DEBUG_S_SYMBOLS subsection"
    );
    assert!(
        cv.data
            .windows("cv_dbg_test.c".len())
            .any(|w| w == b"cv_dbg_test.c"),
        "expected source filename in .debug$S payload"
    );

    let bytes = obj.to_bytes();
    assert_eq!(&bytes[0..2], &[0x64, 0x86], "COFF AMD64 machine");
    assert!(bytes.windows(8).any(|w| w == b".debug$S"));
}
