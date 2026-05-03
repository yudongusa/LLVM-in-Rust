//! Golden codegen gate for issue #178.
//!
//! This locks a small, representative LLVM IR corpus to deterministic x86_64
//! ELF object bytes. Intentional baseline changes must be regenerated with the
//! explicit blessed-update path documented in `docs/golden_codegen_gate.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFormat,
};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{pass::PassManager, Mem2Reg};

const BASELINE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/golden/codegen_x86_64_elf.json"
);

#[derive(Clone, Copy)]
struct GoldenCase {
    label: &'static str,
    category: &'static str,
    run_mem2reg: bool,
    src: &'static str,
}

const GOLDEN_CASES: &[GoldenCase] = &[
    GoldenCase {
        label: "parser_return_constant",
        category: "parser/backend",
        run_mem2reg: false,
        src: r#"target triple = "x86_64-unknown-linux-gnu"

define i32 @main() {
entry:
  ret i32 7
}
"#,
    },
    GoldenCase {
        label: "opt_mem2reg_stack_slot",
        category: "optimization/backend",
        run_mem2reg: true,
        src: r#"target triple = "x86_64-unknown-linux-gnu"

define i32 @main() {
entry:
  %slot = alloca i32, align 4
  store i32 40, ptr %slot, align 4
  %x = load i32, ptr %slot, align 4
  %y = add i32 %x, 2
  ret i32 %y
}
"#,
    },
    GoldenCase {
        label: "backend_arithmetic_chain",
        category: "backend",
        run_mem2reg: false,
        src: r#"target triple = "x86_64-unknown-linux-gnu"

define i32 @main() {
entry:
  %a = add i32 9, 5
  %b = sub i32 %a, 3
  %c = mul i32 %b, 4
  ret i32 %c
}
"#,
    },
    GoldenCase {
        label: "link_external_call_relocation",
        category: "link/relocation",
        run_mem2reg: false,
        src: r#"target triple = "x86_64-unknown-linux-gnu"

declare i32 @callee(i32)

define i32 @main() {
entry:
  %v = call i32 @callee(i32 41)
  ret i32 %v
}
"#,
    },
    GoldenCase {
        label: "branch_phi_diamond",
        category: "backend/control-flow",
        run_mem2reg: false,
        src: r#"target triple = "x86_64-unknown-linux-gnu"

define i32 @main() {
entry:
  br i1 true, label %then, label %else
then:
  br label %join
else:
  br label %join
join:
  %v = phi i32 [ 11, %then ], [ 22, %else ]
  ret i32 %v
}
"#,
    },
    GoldenCase {
        label: "debug_metadata_sections",
        category: "debug/unwind",
        run_mem2reg: false,
        src: r#"source_filename = "golden_debug.c"
target triple = "x86_64-unknown-linux-gnu"

define i32 @main() {
entry:
  ret i32 0, !dbg !12
}
!12 = !DILocation(line: 42, column: 7, scope: !1)
"#,
    },

];

fn stable_hash_hex(data: &[u8]) -> String {
    // Dependency-free FNV-1a fingerprint. This is not used for security; it is
    // a deterministic codegen drift checksum that stays stable across Rust
    // toolchain versions.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn baseline_path() -> PathBuf {
    PathBuf::from(BASELINE_PATH)
}

fn emit_case(case: GoldenCase) -> Vec<u8> {
    // Deterministic mode is opt-in for callers/CI and documented as part of
    // the gate. Today the pipeline is deterministic by construction; setting
    // this env var reserves the explicit contract for future randomized passes.
    std::env::set_var("LLVM_IN_RUST_DETERMINISTIC", "1");

    let (mut ctx, mut module) = parse(case.src)
        .unwrap_or_else(|e| panic!("golden case '{}' did not parse: {e}", case.label));

    if case.run_mem2reg {
        let mut pm = PassManager::new();
        pm.add_function_pass(Mem2Reg);
        pm.run(&mut ctx, &mut module);
    }

    let func = module
        .functions
        .iter()
        .find(|f| f.name == "main" && !f.is_declaration)
        .unwrap_or_else(|| panic!("golden case '{}' has no @main definition", case.label));

    let mut backend = X86Backend::default();
    let mut mf = backend.lower_function(&ctx, &module, func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);

    let mut emitter = X86Emitter::new(ObjectFormat::Elf);
    emit_object(&mf, &mut emitter).to_bytes()
}

fn compute_hashes() -> BTreeMap<String, serde_json::Value> {
    let mut cases = BTreeMap::new();
    for case in GOLDEN_CASES {
        let first = emit_case(*case);
        let second = emit_case(*case);
        assert_eq!(
            first, second,
            "golden case '{}' emitted different object bytes on repeated runs",
            case.label
        );

        cases.insert(
            case.label.to_owned(),
            serde_json::json!({
                "category": case.category,
                "target": "x86_64-unknown-linux-gnu",
                "object_format": "elf",
                "passes": if case.run_mem2reg { vec!["mem2reg"] } else { Vec::<&str>::new() },
                "object_hash": stable_hash_hex(&first),
            }),
        );
    }
    cases
}

fn load_baseline() -> serde_json::Value {
    let path = baseline_path();
    let data = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read golden baseline {}: {e}", path.display()));
    serde_json::from_str(&data)
        .unwrap_or_else(|e| panic!("invalid golden baseline {}: {e}", path.display()))
}

#[test]
fn golden_codegen_objects_match_locked_baseline() {
    let actual = serde_json::json!({
        "version": 1,
        "deterministic_mode": "LLVM_IN_RUST_DETERMINISTIC=1",
        "cases": compute_hashes(),
    });

    if std::env::var("UPDATE_GOLDEN_CODEGEN").is_ok() {
        assert_eq!(
            std::env::var("BLESS_GOLDEN_CODEGEN").as_deref(),
            Ok("1"),
            "refusing to update golden codegen baselines without BLESS_GOLDEN_CODEGEN=1"
        );
        std::fs::write(
            baseline_path(),
            format!("{}\n", serde_json::to_string_pretty(&actual).unwrap()),
        )
        .expect("write golden codegen baseline");
        return;
    }

    let expected = load_baseline();
    assert_eq!(
        actual, expected,
        "Golden codegen drift detected. If intentional, run scripts/update_golden_codegen.sh --bless and get maintainer approval on the PR."
    );
}
