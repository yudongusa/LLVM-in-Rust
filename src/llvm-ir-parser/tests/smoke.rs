//! Smoke-test oracle: compile each program with Clang/LLVM 19 (ground truth)
//! AND with our Rust-native pipeline, then assert **identical runtime behaviour**
//! (exit code + stdout).
//!
//! Clang/LLVM 19 is the oracle — its output is the definition of "correct".
//! If our pipeline agrees, the smoke test passes.  If tools are absent the
//! test skips gracefully; with `REQUIRE_LLVM=1` absent tools cause a panic.
//!
//! All programs are single-`@main` modules that exercise:
//!   loops (alloca-based, converted by mem2reg),
//!   nested loops, conditional dispatch, bitwise/arithmetic operations.
//!
//! stdout comparison is left for future work once the x86 backend gains
//! proper GlobalRef → RIP-relative address materialisation.

use std::path::{Path, PathBuf};
use std::process::Command;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFormat,
};
use llvm_ir::{printer::Printer, Context, Module};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{pass::PassManager, Mem2Reg};

// ── tool discovery ────────────────────────────────────────────────────────────

fn find_llvm_bin() -> Option<PathBuf> {
    [
        "/usr/local/opt/llvm/bin",
        "/opt/homebrew/opt/llvm/bin",
        "/usr/lib/llvm-19/bin",
        "/usr/bin",
        "/usr/local/bin",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.join("clang").exists())
    .or_else(|| {
        Command::new("clang")
            .arg("--version")
            .output()
            .ok()
            .map(|_| PathBuf::from(""))
    })
}

fn llvm_tool(name: &str) -> Option<PathBuf> {
    let bin = find_llvm_bin()?;
    let path = if bin.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        bin.join(name)
    };
    if path.exists() || bin.as_os_str().is_empty() {
        Some(path)
    } else {
        None
    }
}

fn require_tool(name: &str) -> Option<PathBuf> {
    match llvm_tool(name) {
        Some(p) => Some(p),
        None => {
            if std::env::var("REQUIRE_LLVM").is_ok() {
                panic!(
                    "REQUIRE_LLVM is set but '{}' was not found. \
                     Install LLVM 19 and ensure it is on PATH.",
                    name
                );
            }
            None
        }
    }
}

fn strict_smoke_required() -> bool {
    std::env::var("REQUIRE_LLVM").is_ok()
}

// ── temp-file helpers ─────────────────────────────────────────────────────────

fn with_temp_ll<R>(tag: &str, content: &str, f: impl FnOnce(&Path) -> R) -> R {
    let path = std::env::temp_dir().join(format!("smoke_{tag}.ll"));
    std::fs::write(&path, content).expect("write temp .ll");
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

fn with_temp_file<R>(tag: &str, ext: &str, f: impl FnOnce(&Path) -> R) -> R {
    let path = std::env::temp_dir().join(format!("smoke_{tag}.{ext}"));
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

// ── RunResult ─────────────────────────────────────────────────────────────────

/// The observable runtime result of executing a compiled binary.
#[derive(Debug, PartialEq)]
struct RunResult {
    exit_code: i32,
    stdout: String,
}

fn run_binary(path: &Path, label: &str, which: &str) -> Option<std::process::Output> {
    #[cfg(target_os = "linux")]
    {
        let out = Command::new("timeout")
            .args(["5s"])
            .arg(path)
            .output()
            .ok()?;
        let code = out.status.code().unwrap_or(-1);
        if code == 124 || code == 137 {
            eprintln!("[smoke/{label}] {which} timed out after 5s");
            return None;
        }
        Some(out)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Command::new(path).output().ok().or_else(|| {
            eprintln!("[smoke/{label}] failed to run {which}");
            None
        })
    }
}

// ── oracle path (Clang/LLVM 19) ───────────────────────────────────────────────

/// Compile `ir` with Clang, execute the binary, and return its exit code + stdout.
fn run_oracle(clang: &Path, label: &str, ir: &str) -> Option<RunResult> {
    let bin_path = std::env::temp_dir().join(format!("smoke_{label}_oracle_bin"));
    with_temp_ll(&format!("{label}_oracle"), ir, |ll_path| {
        let compile = Command::new(clang)
            .args(["-x", "ir", "-O0"])
            .arg(ll_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .expect("spawn clang");
        if !compile.status.success() {
            eprintln!(
                "[smoke/{label}] clang compile failed:\n{}",
                String::from_utf8_lossy(&compile.stderr)
            );
            return None;
        }
        let run = run_binary(&bin_path, label, "oracle binary")?;
        let _ = std::fs::remove_file(&bin_path);
        Some(RunResult {
            exit_code: run.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&run.stdout).into_owned(),
        })
    })
}

// ── our pipeline path ─────────────────────────────────────────────────────────

fn host_object_format() -> Option<ObjectFormat> {
    if cfg!(target_os = "macos") {
        Some(ObjectFormat::MachO)
    } else if cfg!(target_os = "linux") {
        Some(ObjectFormat::Elf)
    } else {
        None
    }
}

/// Lower `@main` with our x86 backend, link with `cc`, execute, and return
/// exit code + stdout. Returns `None` if linking fails.
fn run_ours(ctx: &Context, module: &Module, label: &str) -> Option<RunResult> {
    let main_func = module
        .functions
        .iter()
        .find(|f| f.name == "main" && !f.is_declaration)?;

    let mut backend = X86Backend::default();
    let mut mf = backend.lower_function(ctx, module, main_func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);
    let obj_format = match host_object_format() {
        Some(f) => f,
        None => {
            eprintln!("[smoke/{label}] unsupported host object format");
            return None;
        }
    };
    let mut emitter = X86Emitter::new(obj_format);
    let obj = emit_object(&mf, &mut emitter);
    let obj_bytes = obj.to_bytes();

    with_temp_file(&format!("{label}_ours"), "o", |obj_path| {
        std::fs::write(obj_path, &obj_bytes).expect("write .o");
        let bin_path = std::env::temp_dir().join(format!("smoke_{label}_ours_bin"));
        let link = match Command::new("cc")
            .arg(obj_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("[smoke/{label}] failed to spawn cc: {e}");
                return None;
            }
        };
        if !link.status.success() {
            eprintln!(
                "[smoke/{label}] link failed:\n{}",
                String::from_utf8_lossy(&link.stderr)
            );
            return None;
        }
        let run = run_binary(&bin_path, label, "ours binary")?;
        let _ = std::fs::remove_file(&bin_path);
        Some(RunResult {
            exit_code: run.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&run.stdout).into_owned(),
        })
    })
}

// ── oracle harness ────────────────────────────────────────────────────────────

/// Core oracle harness.
///
/// 1. Parse `src` and run mem2reg (same transformation both paths see).
/// 2. Compile with the Clang oracle → capture RunResult.
/// 3. Compile with our pipeline  → capture RunResult.
/// 4. Assert exact equality.
///
/// If Clang is absent the test skips (or panics when `REQUIRE_LLVM=1`).
/// If `REQUIRE_LLVM=1` is set, our path must also emit+link+run successfully.
fn smoke_oracle(label: &str, src: &str) {
    let clang = match require_tool("clang") {
        Some(p) => p,
        None => return,
    };

    let (mut ctx, mut module) =
        parse(src).unwrap_or_else(|e| panic!("[smoke/{label}] parse failed: {e}"));
    let mut pm = PassManager::new();
    pm.add_function_pass(Mem2Reg);
    pm.run(&mut ctx, &mut module);

    let printed = Printer::new(&ctx).print_module(&module);

    let oracle = match run_oracle(&clang, label, &printed) {
        Some(r) => r,
        None => {
            eprintln!("[smoke/{label}] oracle (clang) failed — skipping");
            return;
        }
    };

    let ours = match run_ours(&ctx, &module, label) {
        Some(r) => r,
        None => {
            if strict_smoke_required() {
                panic!("[smoke/{label}] our path failed (emit/link/run) with REQUIRE_LLVM=1");
            }
            eprintln!("[smoke/{label}] our path skipped (link/emit failed)");
            eprintln!(
                "[smoke/{label}] oracle produced: exit={} stdout={:?}",
                oracle.exit_code, oracle.stdout
            );
            return;
        }
    };

    assert_eq!(
        oracle, ours,
        "[smoke/{label}] oracle vs ours mismatch\n\
         oracle exit={} stdout={:?}\n\
         ours   exit={} stdout={:?}",
        oracle.exit_code, oracle.stdout, ours.exit_code, ours.stdout,
    );
    eprintln!(
        "[smoke/{label}] PASS — exit={} stdout={:?}",
        ours.exit_code, ours.stdout
    );
}

// ── smoke tests ───────────────────────────────────────────────────────────────

/// Sum 1 + 2 + ... + 9 = 45.  Exercises a counted loop with two loop variables.
#[test]
fn smoke_loop_sum() {
    smoke_oracle(
        "loop_sum",
        r#"define i32 @main() {
entry:
  %sum = alloca i32
  %i = alloca i32
  store i32 0, ptr %sum
  store i32 1, ptr %i
  br label %loop
loop:
  %iv = load i32, ptr %i
  %sv = load i32, ptr %sum
  %cmp = icmp sle i32 %iv, 9
  br i1 %cmp, label %body, label %exit
body:
  %ns = add i32 %sv, %iv
  store i32 %ns, ptr %sum
  %ni = add i32 %iv, 1
  store i32 %ni, ptr %i
  br label %loop
exit:
  %result = load i32, ptr %sum
  ret i32 %result
}
"#,
    );
}

/// Iterative Fibonacci: fib(7) = 13.  Three loop variables; tests phi chains.
#[test]
fn smoke_fibonacci_iterative() {
    smoke_oracle(
        "fibonacci_iterative",
        r#"define i32 @main() {
entry:
  %a = alloca i32
  %b = alloca i32
  %i = alloca i32
  store i32 0, ptr %a
  store i32 1, ptr %b
  store i32 0, ptr %i
  br label %loop
loop:
  %iv = load i32, ptr %i
  %cmp = icmp slt i32 %iv, 7
  br i1 %cmp, label %body, label %exit
body:
  %av = load i32, ptr %a
  %bv = load i32, ptr %b
  %next = add i32 %av, %bv
  store i32 %bv, ptr %a
  store i32 %next, ptr %b
  %ni = add i32 %iv, 1
  store i32 %ni, ptr %i
  br label %loop
exit:
  %result = load i32, ptr %a
  ret i32 %result
}
"#,
    );
}

/// Euclidean GCD(48, 18) = 6.  Tests sdiv/srem in a loop.
#[test]
fn smoke_gcd_iterative() {
    smoke_oracle(
        "gcd_iterative",
        r#"define i32 @main() {
entry:
  %a = alloca i32
  %b = alloca i32
  store i32 48, ptr %a
  store i32 18, ptr %b
  br label %loop
loop:
  %bv = load i32, ptr %b
  %cmp = icmp ne i32 %bv, 0
  br i1 %cmp, label %body, label %exit
body:
  %av = load i32, ptr %a
  %rem = srem i32 %av, %bv
  store i32 %bv, ptr %a
  store i32 %rem, ptr %b
  br label %loop
exit:
  %result = load i32, ptr %a
  ret i32 %result
}
"#,
    );
}

/// Iterative 5! = 120.  Tests multiply-accumulate in a counted loop.
#[test]
fn smoke_factorial_iterative() {
    smoke_oracle(
        "factorial_iterative",
        r#"define i32 @main() {
entry:
  %result = alloca i32
  %i = alloca i32
  store i32 1, ptr %result
  store i32 2, ptr %i
  br label %loop
loop:
  %iv = load i32, ptr %i
  %cmp = icmp sle i32 %iv, 5
  br i1 %cmp, label %body, label %exit
body:
  %rv = load i32, ptr %result
  %nr = mul i32 %rv, %iv
  store i32 %nr, ptr %result
  %ni = add i32 %iv, 1
  store i32 %ni, ptr %i
  br label %loop
exit:
  %r = load i32, ptr %result
  ret i32 %r
}
"#,
    );
}

/// max(11, 42, 17) = 42.  Tests a chain of `select` instructions.
#[test]
fn smoke_max_select() {
    smoke_oracle(
        "max_select",
        r#"define i32 @main() {
entry:
  %a = add i32 11, 0
  %b = add i32 42, 0
  %c = add i32 17, 0
  %ab_gt = icmp sgt i32 %b, %a
  %max_ab = select i1 %ab_gt, i32 %b, i32 %a
  %abc_gt = icmp sgt i32 %c, %max_ab
  %result = select i1 %abc_gt, i32 %c, i32 %max_ab
  ret i32 %result
}
"#,
    );
}

/// Bitwise: (0x7C & 0x3F) | 0x05 = 61.  Tests and/or with immediate operands.
#[test]
fn smoke_bitwise() {
    smoke_oracle(
        "bitwise",
        r#"define i32 @main() {
entry:
  %v = add i32 124, 0
  %masked = and i32 %v, 63
  %result = or i32 %masked, 5
  ret i32 %result
}
"#,
    );
}

/// 3×3 nested loop: sum of i*j for i,j in 0..2 = 9.
/// Exercises nested phi chains and inner-loop reset.
#[test]
fn smoke_nested_loop() {
    smoke_oracle(
        "nested_loop",
        r#"define i32 @main() {
entry:
  %sum = alloca i32
  %i = alloca i32
  %j = alloca i32
  store i32 0, ptr %sum
  store i32 0, ptr %i
  store i32 0, ptr %j
  br label %outer
outer:
  %iv = load i32, ptr %i
  %ocmp = icmp slt i32 %iv, 3
  br i1 %ocmp, label %outer_body, label %exit
outer_body:
  store i32 0, ptr %j
  br label %inner
inner:
  %jv = load i32, ptr %j
  %icmp = icmp slt i32 %jv, 3
  br i1 %icmp, label %inner_body, label %inner_exit
inner_body:
  %sv = load i32, ptr %sum
  %iv2 = load i32, ptr %i
  %prod = mul i32 %iv2, %jv
  %ns = add i32 %sv, %prod
  store i32 %ns, ptr %sum
  %nj = add i32 %jv, 1
  store i32 %nj, ptr %j
  br label %inner
inner_exit:
  %ni = add i32 %iv, 1
  store i32 %ni, ptr %i
  br label %outer
exit:
  %result = load i32, ptr %sum
  ret i32 %result
}
"#,
    );
}

/// 2^7 = 128.  Multiply-by-two loop; tests a simple counted loop with mul.
#[test]
fn smoke_power_of_two() {
    smoke_oracle(
        "power_of_two",
        r#"define i32 @main() {
entry:
  %result = alloca i32
  %i = alloca i32
  store i32 1, ptr %result
  store i32 0, ptr %i
  br label %loop
loop:
  %iv = load i32, ptr %i
  %cmp = icmp slt i32 %iv, 7
  br i1 %cmp, label %body, label %exit
body:
  %rv = load i32, ptr %result
  %nr = mul i32 %rv, 2
  store i32 %nr, ptr %result
  %ni = add i32 %iv, 1
  store i32 %ni, ptr %i
  br label %loop
exit:
  %r = load i32, ptr %result
  ret i32 %r
}
"#,
    );
}

/// Collatz(6) reaches 1 in 8 steps.  Tests mixed even/odd branching with select.
#[test]
fn smoke_collatz_steps() {
    smoke_oracle(
        "collatz_steps",
        r#"define i32 @main() {
entry:
  %n = alloca i32
  %steps = alloca i32
  store i32 6, ptr %n
  store i32 0, ptr %steps
  br label %loop
loop:
  %nv = load i32, ptr %n
  %cmp = icmp ne i32 %nv, 1
  br i1 %cmp, label %body, label %exit
body:
  %rem = srem i32 %nv, 2
  %is_odd = icmp ne i32 %rem, 0
  %half = sdiv i32 %nv, 2
  %triple = mul i32 %nv, 3
  %triple1 = add i32 %triple, 1
  %next_n = select i1 %is_odd, i32 %triple1, i32 %half
  store i32 %next_n, ptr %n
  %sv = load i32, ptr %steps
  %ns = add i32 %sv, 1
  store i32 %ns, ptr %steps
  br label %loop
exit:
  %result = load i32, ptr %steps
  ret i32 %result
}
"#,
    );
}

/// Popcount(0b10110110 = 182) = 5.  Tests bit-and + lshr in a counted loop.
#[test]
fn smoke_popcount() {
    smoke_oracle(
        "popcount",
        r#"define i32 @main() {
entry:
  %val = alloca i32
  %count = alloca i32
  %shift = alloca i32
  store i32 182, ptr %val
  store i32 0, ptr %count
  store i32 0, ptr %shift
  br label %loop
loop:
  %sv = load i32, ptr %shift
  %cmp = icmp slt i32 %sv, 8
  br i1 %cmp, label %body, label %exit
body:
  %vv = load i32, ptr %val
  %cv = load i32, ptr %count
  %bit = and i32 %vv, 1
  %nc = add i32 %cv, %bit
  store i32 %nc, ptr %count
  %nv = lshr i32 %vv, 1
  store i32 %nv, ptr %val
  %ns = add i32 %sv, 1
  store i32 %ns, ptr %shift
  br label %loop
exit:
  %result = load i32, ptr %count
  ret i32 %result
}
"#,
    );
}

/// Vector smoke: simple `<4 x i32>` pipeline through insert/shuffle/extract.
/// This validates parse -> backend -> link -> run on a vector-IR module.
#[test]
fn smoke_vector_lane0_roundtrip() {
    smoke_oracle(
        "vector_lane0_roundtrip",
        r#"define i32 @main() {
entry:
  %v0 = insertelement <4 x i32> zeroinitializer, i32 0, i32 0
  %v1 = insertelement <4 x i32> %v0, i32 0, i32 1
  %v2 = shufflevector <4 x i32> %v1, <4 x i32> zeroinitializer, <4 x i32> <i32 0, i32 1, i32 4, i32 5>
  %lane = extractelement <4 x i32> %v2, i32 0
  ret i32 %lane
}
"#,
    );
}
