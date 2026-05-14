use std::collections::BTreeMap;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFormat,
};
use llvm_ir::{Context, Module};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{ConstProp, DeadCodeElim, Mem2Reg, PassManager};

#[derive(Clone, Copy)]
enum Stage {
    Parse,
    Optimize,
    Codegen,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Stage::Parse => "parse",
            Stage::Optimize => "optimize",
            Stage::Codegen => "codegen",
        }
    }
}

struct Config {
    input_dir: PathBuf,
    report_md: PathBuf,
    report_json: PathBuf,
    limit: Option<usize>,
}

struct Failure {
    path: PathBuf,
    stage: Stage,
    category: String,
    message: String,
}

struct Summary {
    input_dir: PathBuf,
    files_total: usize,
    parse_ok: usize,
    optimize_ok: usize,
    codegen_ok: usize,
    functions_codegened: usize,
    failures: Vec<Failure>,
}

fn usage() -> ! {
    eprintln!(
        "usage: cargo run -p llvm-in-rust-ir-parser --example llvm_test_suite_compat -- \\
         --input-dir <llvm-test-suite> [--report-md <path>] [--report-json <path>] [--limit <n>]"
    );
    std::process::exit(2);
}

fn parse_args() -> Config {
    let mut input_dir = None;
    let mut report_md = PathBuf::from("target/llvm-test-suite-compat/report/report.md");
    let mut report_json = PathBuf::from("target/llvm-test-suite-compat/report/report.json");
    let mut limit = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input-dir" => input_dir = args.next().map(PathBuf::from),
            "--report-md" => report_md = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--report-json" => {
                report_json = args.next().map(PathBuf::from).unwrap_or_else(|| usage())
            }
            "--limit" => {
                let raw = args.next().unwrap_or_else(|| usage());
                let parsed = raw
                    .parse::<usize>()
                    .unwrap_or_else(|_| panic!("invalid --limit value: {raw}"));
                if parsed > 0 {
                    limit = Some(parsed);
                }
            }
            "--help" | "-h" => usage(),
            _ => usage(),
        }
    }

    Config {
        input_dir: input_dir.unwrap_or_else(|| usage()),
        report_md,
        report_json,
        limit,
    }
}

fn collect_ll_files(root: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_ll_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("ll") {
            files.push(path);
        }
    }
    Ok(())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn catch_stage<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => Err(format!("panic: {}", panic_message(payload))),
    }
}

fn optimize(ctx: &mut Context, module: &mut Module) {
    let mut passes = PassManager::new();
    passes.add_function_pass(Mem2Reg);
    passes.add_function_pass(DeadCodeElim);
    passes.add_function_pass(ConstProp);
    passes.run(ctx, module);
}

fn codegen_x86(ctx: &Context, module: &Module) -> Result<usize, String> {
    let mut lowered = 0;

    for func in module.functions.iter().filter(|func| !func.is_declaration) {
        let mut backend = X86Backend::default();
        let mut mf = backend.lower_function(ctx, module, func);
        let intervals = compute_live_intervals(&mf);
        let mut allocation = allocate_registers(
            &intervals,
            &mf.allocatable_pregs,
            RegAllocStrategy::LinearScan,
        );
        insert_spill_reloads(&mut mf, &mut allocation, MOV_LOAD_MR, MOV_STORE_RM);
        apply_allocation(&mut mf, &allocation);

        let mut emitter = X86Emitter::new(ObjectFormat::Elf);
        let object = emit_object(&mf, &mut emitter);
        let bytes = object.to_bytes();
        if bytes.is_empty() {
            return Err(format!("empty object for function @{}", func.name));
        }
        lowered += 1;
    }

    Ok(lowered)
}

fn category(message: &str) -> String {
    let mut first = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error")
        .to_string();
    if first.len() > 120 {
        first.truncate(120);
        first.push_str("...");
    }
    first
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn run_file(path: &Path, root: &Path, summary: &mut Summary) {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            let message = format!("read failed: {err}");
            summary.failures.push(Failure {
                path: PathBuf::from(relative(path, root)),
                stage: Stage::Parse,
                category: category(&message),
                message,
            });
            return;
        }
    };

    let (mut ctx, mut module) = match catch_stage(|| parse(&source).map_err(|err| err.to_string()))
    {
        Ok(parsed) => {
            summary.parse_ok += 1;
            parsed
        }
        Err(message) => {
            summary.failures.push(Failure {
                path: PathBuf::from(relative(path, root)),
                stage: Stage::Parse,
                category: category(&message),
                message,
            });
            return;
        }
    };

    match catch_stage(|| {
        optimize(&mut ctx, &mut module);
        Ok(())
    }) {
        Ok(()) => summary.optimize_ok += 1,
        Err(message) => {
            summary.failures.push(Failure {
                path: PathBuf::from(relative(path, root)),
                stage: Stage::Optimize,
                category: category(&message),
                message,
            });
            return;
        }
    }

    match catch_stage(|| codegen_x86(&ctx, &module)) {
        Ok(functions) => {
            summary.codegen_ok += 1;
            summary.functions_codegened += functions;
        }
        Err(message) => {
            summary.failures.push(Failure {
                path: PathBuf::from(relative(path, root)),
                stage: Stage::Codegen,
                category: category(&message),
                message,
            });
        }
    }
}

fn rate(ok: usize, total: usize) -> String {
    if total == 0 {
        "n/a".to_string()
    } else {
        format!("{:.1}%", (ok as f64 / total as f64) * 100.0)
    }
}

fn top_categories(summary: &Summary) -> Vec<(String, usize, Vec<String>)> {
    let mut grouped: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    for failure in &summary.failures {
        let key = format!("{}: {}", failure.stage.as_str(), failure.category);
        let entry = grouped.entry(key).or_insert((0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < 3 {
            entry.1.push(failure.path.to_string_lossy().into_owned());
        }
    }

    let mut categories: Vec<_> = grouped
        .into_iter()
        .map(|(key, (count, examples))| (key, count, examples))
        .collect();
    categories.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    categories
}

fn write_report_md(summary: &Summary, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut report = String::new();
    report.push_str("# LLVM Test Suite IR Compatibility Report\n\n");
    report.push_str(&format!(
        "- Input: `{}`\n- Files scanned: `{}`\n- Functions lowered during codegen: `{}`\n\n",
        summary.input_dir.display(),
        summary.files_total,
        summary.functions_codegened
    ));

    report.push_str("| Stage | Passed | Total | Rate | Phase target |\n");
    report.push_str("|---|---:|---:|---:|---:|\n");
    report.push_str(&format!(
        "| Parse | {} | {} | {} | 80% now / 90% phase 2 |\n",
        summary.parse_ok,
        summary.files_total,
        rate(summary.parse_ok, summary.files_total)
    ));
    report.push_str(&format!(
        "| Optimize | {} | {} | {} | 70% phase 2 |\n",
        summary.optimize_ok,
        summary.files_total,
        rate(summary.optimize_ok, summary.files_total)
    ));
    report.push_str(&format!(
        "| x86_64 codegen | {} | {} | {} | 70% phase 3 |\n\n",
        summary.codegen_ok,
        summary.files_total,
        rate(summary.codegen_ok, summary.files_total)
    ));

    report.push_str("## Top Failure Categories\n\n");
    let categories = top_categories(summary);
    if categories.is_empty() {
        report.push_str("No failures recorded.\n\n");
    } else {
        report.push_str("| Rank | Category | Count | Examples |\n");
        report.push_str("|---:|---|---:|---|\n");
        for (idx, (category, count, examples)) in categories.iter().take(5).enumerate() {
            report.push_str(&format!(
                "| {} | `{}` | {} | {} |\n",
                idx + 1,
                category.replace('|', "\\|"),
                count,
                examples
                    .iter()
                    .map(|example| format!("`{}`", example.replace('|', "\\|")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        report.push('\n');
    }

    report.push_str("## Sample Failures\n\n");
    for failure in summary.failures.iter().take(20) {
        report.push_str(&format!(
            "- `{}` [{}]: {}\n",
            failure.path.display(),
            failure.stage.as_str(),
            failure.message.replace('\n', " ")
        ));
    }
    if summary.failures.is_empty() {
        report.push_str("No sample failures.\n");
    }

    fs::write(path, report)
}

fn json_escape(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn write_report_json(summary: &Summary, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut json = String::new();
    json.push_str("{\n");
    json.push_str(&format!(
        "  \"input_dir\": \"{}\",\n",
        json_escape(&summary.input_dir.to_string_lossy())
    ));
    json.push_str(&format!("  \"files_total\": {},\n", summary.files_total));
    json.push_str(&format!("  \"parse_ok\": {},\n", summary.parse_ok));
    json.push_str(&format!("  \"optimize_ok\": {},\n", summary.optimize_ok));
    json.push_str(&format!("  \"codegen_ok\": {},\n", summary.codegen_ok));
    json.push_str(&format!(
        "  \"functions_codegened\": {},\n",
        summary.functions_codegened
    ));
    json.push_str("  \"top_categories\": [\n");
    for (idx, (category, count, examples)) in
        top_categories(summary).into_iter().take(10).enumerate()
    {
        if idx > 0 {
            json.push_str(",\n");
        }
        json.push_str(&format!(
            "    {{\"category\": \"{}\", \"count\": {}, \"examples\": [{}]}}",
            json_escape(&category),
            count,
            examples
                .iter()
                .map(|example| format!("\"{}\"", json_escape(example)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    json.push_str("\n  ],\n");
    json.push_str("  \"failures\": [\n");
    for (idx, failure) in summary.failures.iter().enumerate() {
        if idx > 0 {
            json.push_str(",\n");
        }
        json.push_str(&format!(
            "    {{\"path\": \"{}\", \"stage\": \"{}\", \"category\": \"{}\", \"message\": \"{}\"}}",
            json_escape(&failure.path.to_string_lossy()),
            failure.stage.as_str(),
            json_escape(&failure.category),
            json_escape(&failure.message)
        ));
    }
    json.push_str("\n  ]\n");
    json.push_str("}\n");

    fs::write(path, json)
}

fn main() {
    let config = parse_args();
    let mut files = Vec::new();
    collect_ll_files(&config.input_dir, &mut files).unwrap_or_else(|err| {
        eprintln!(
            "failed to collect .ll files from '{}': {err}",
            config.input_dir.display()
        );
        std::process::exit(1);
    });
    files.sort();
    if let Some(limit) = config.limit {
        files.truncate(limit);
    }

    let mut summary = Summary {
        input_dir: config.input_dir.clone(),
        files_total: files.len(),
        parse_ok: 0,
        optimize_ok: 0,
        codegen_ok: 0,
        functions_codegened: 0,
        failures: Vec::new(),
    };

    for path in &files {
        run_file(path, &config.input_dir, &mut summary);
    }

    write_report_md(&summary, &config.report_md).unwrap_or_else(|err| {
        eprintln!("failed to write '{}': {err}", config.report_md.display());
        std::process::exit(1);
    });
    write_report_json(&summary, &config.report_json).unwrap_or_else(|err| {
        eprintln!("failed to write '{}': {err}", config.report_json.display());
        std::process::exit(1);
    });

    println!(
        "LLVM test-suite IR compatibility: parse {}/{}, optimize {}/{}, codegen {}/{}",
        summary.parse_ok,
        summary.files_total,
        summary.optimize_ok,
        summary.files_total,
        summary.codegen_ok,
        summary.files_total
    );
    println!("markdown report: {}", config.report_md.display());
    println!("json report: {}", config.report_json.display());
}
