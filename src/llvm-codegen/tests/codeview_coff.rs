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

fn build_coff_cv_obj() -> (Vec<u8>, Vec<u8>) {
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

    let cv_data = obj
        .sections
        .iter()
        .find(|s| s.name == ".debug$S")
        .expect(".debug$S must exist")
        .data
        .clone();
    let raw_bytes = obj.to_bytes();
    (cv_data, raw_bytes)
}

#[test]
fn debug_s_contains_s_gproc32_record() {
    // S_GPROC32 (rectype = 0x1110) must appear in the DEBUG_S_SYMBOLS subsection
    // so WinDbg can map instruction addresses to the function name.
    let (cv, _) = build_coff_cv_obj();

    // Scan for S_GPROC32 record type bytes [0x10, 0x11] (little-endian 0x1110).
    let has_gproc32 = cv.windows(2).any(|w| w == [0x10, 0x11]);
    assert!(has_gproc32, ".debug$S must contain S_GPROC32 (0x1110) record");
}

#[test]
fn debug_s_contains_s_end_record() {
    // S_END (0x0006) closes the S_GPROC32 lexical block.
    let (cv, _) = build_coff_cv_obj();
    let has_end = cv.windows(2).any(|w| w == [0x06, 0x00]);
    assert!(has_end, ".debug$S must contain S_END (0x0006) record");
}

#[test]
fn debug_s_contains_function_name() {
    // The function name must appear verbatim in the S_GPROC32 name field.
    let (cv, _) = build_coff_cv_obj();
    assert!(
        cv.windows(4).any(|w| w == b"main"),
        ".debug$S must contain function name 'main'"
    );
}

#[test]
fn debug_s_contains_debug_s_lines_subsection() {
    // DEBUG_S_LINES (0xF2) subsection provides the code-offset → line mapping
    // required for source-line breakpoints in WinDbg.
    let (cv, _) = build_coff_cv_obj();
    let has_lines = cv
        .windows(4)
        .any(|w| u32::from_le_bytes(w.try_into().unwrap()) == 0xF2);
    assert!(has_lines, ".debug$S must contain DEBUG_S_LINES (0xF2) subsection");
}

#[test]
fn debug_s_contains_filechksms_subsection() {
    // DEBUG_S_FILECHKSMS (0xF4) is required for the LINES→filename cross-reference.
    let (cv, _) = build_coff_cv_obj();
    let has_chksm = cv
        .windows(4)
        .any(|w| u32::from_le_bytes(w.try_into().unwrap()) == 0xF4);
    assert!(has_chksm, ".debug$S must contain DEBUG_S_FILECHKSMS (0xF4) subsection");
}

#[test]
fn debug_s_contains_stringtable_subsection() {
    // DEBUG_S_STRINGTABLE (0xF3) holds the source file path referenced by FILECHKSMS.
    let (cv, _) = build_coff_cv_obj();
    let has_strtab = cv
        .windows(4)
        .any(|w| u32::from_le_bytes(w.try_into().unwrap()) == 0xF3);
    assert!(has_strtab, ".debug$S must contain DEBUG_S_STRINGTABLE (0xF3) subsection");
}
