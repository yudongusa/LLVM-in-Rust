//! Informational LLVM test-suite IR compatibility runner.
//!
//! Scans a directory for `.ll` files, then records parser, optimizer, and x86
//! instruction-selection outcomes without panicking the harness on individual
//! failures.

use llvm_codegen::isel::IselBackend;
use llvm_ir_parser::parser::parse;
use llvm_target_x86::X86Backend;
use llvm_transforms::{ConstProp, DeadCodeElim, Mem2Reg, PassManager};
use std::env;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Stats {
    total: usize,
    parsed: usize,
    optimized: usize,
    codegen: usize,
    failures: Vec<(String, String, String)>,
}

fn main() {
    let mut suite_dir = PathBuf::from("target/llvm-test-suite");
    let mut report = PathBuf::from("target/llvm-test-suite-compat.md");
    let mut limit: Option<usize> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--suite-dir" => suite_dir = PathBuf::from(args.next().expect("--suite-dir value")),
            "--report" => report = PathBuf::from(args.next().expect("--report value")),
            "--limit" => {
                limit = Some(
                    args.next()
                        .expect("--limit value")
                        .parse()
                        .expect("numeric --limit"),
                )
            }
            "--help" | "-h" => {
                eprintln!("usage: llvm-test-suite-compat [--suite-dir DIR] [--report PATH] [--limit N]");
                return;
            }
            other => panic!("unknown argument: {other}"),
        }
    }

    let mut files = Vec::new();
    collect_ll_files(&suite_dir, &mut files).expect("scan suite dir");
    files.sort();
    if let Some(n) = limit {
        files.truncate(n);
    }

    let mut stats = Stats::default();
    for path in files {
        stats.total += 1;
        let rel = path
            .strip_prefix(&suite_dir)
            .unwrap_or(&path)
            .display()
            .to_string();
        let src = match fs::read_to_string(&path) {
            Ok(src) => src,
            Err(err) => {
                stats.failures.push((rel, "read".into(), err.to_string()));
                continue;
            }
        };

        let parsed = catch_unwind(AssertUnwindSafe(|| parse(&src)));
        let (mut ctx, mut module) = match parsed {
            Ok(Ok(parsed)) => {
                stats.parsed += 1;
                parsed
            }
            Ok(Err(err)) => {
                stats.failures.push((rel, "parse".into(), err.to_string()));
                continue;
            }
            Err(_) => {
                stats.failures.push((rel, "parse-panic".into(), "panic".into()));
                continue;
            }
        };

        let optimized = catch_unwind(AssertUnwindSafe(|| {
            let mut pm = PassManager::new();
            pm.add_function_pass(Mem2Reg);
            pm.add_function_pass(DeadCodeElim);
            pm.add_function_pass(ConstProp);
            pm.run(&mut ctx, &mut module);
        }));
        if optimized.is_err() {
            stats.failures.push((rel, "optimize-panic".into(), "panic".into()));
            continue;
        }
        stats.optimized += 1;

        let codegen = catch_unwind(AssertUnwindSafe(|| {
            let mut be = X86Backend::default();
            for f in &module.functions {
                if !f.is_declaration {
                    let _ = be.lower_function(&ctx, &module, f);
                }
            }
        }));
        if codegen.is_err() {
            stats.failures.push((rel, "codegen-panic".into(), "panic".into()));
            continue;
        }
        stats.codegen += 1;
    }

    write_report(&report, &suite_dir, &stats).expect("write report");
    println!("wrote {}", report.display());
    println!(
        "total={} parsed={} optimized={} codegen={}",
        stats.total, stats.parsed, stats.optimized, stats.codegen
    );
}

fn collect_ll_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_ll_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "ll") {
            out.push(path);
        }
    }
    Ok(())
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        (n as f64) * 100.0 / (d as f64)
    }
}

fn write_report(path: &Path, suite_dir: &Path, stats: &Stats) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    body.push_str("# LLVM test-suite IR compatibility report\n\n");
    body.push_str(&format!("Suite dir: `{}`\n\n", suite_dir.display()));
    body.push_str("| Stage | Count | Rate |\n|---|---:|---:|\n");
    body.push_str(&format!("| Total `.ll` files | {} | 100% |\n", stats.total));
    body.push_str(&format!("| Parsed | {} | {:.1}% |\n", stats.parsed, pct(stats.parsed, stats.total)));
    body.push_str(&format!("| Optimized | {} | {:.1}% |\n", stats.optimized, pct(stats.optimized, stats.total)));
    body.push_str(&format!("| x86 codegen lowered | {} | {:.1}% |\n\n", stats.codegen, pct(stats.codegen, stats.total)));

    body.push_str("## First failures by file\n\n");
    for (file, stage, err) in stats.failures.iter().take(50) {
        let first_line = err.lines().next().unwrap_or(err);
        body.push_str(&format!("- `{stage}` `{file}` — `{first_line}`\n"));
    }
    if stats.failures.is_empty() {
        body.push_str("No failures recorded.\n");
    }
    fs::write(path, body)
}
