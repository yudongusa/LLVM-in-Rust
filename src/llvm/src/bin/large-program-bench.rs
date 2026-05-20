//! `large-program-bench` — compile real-world IR fixtures and report metrics.
//!
//! This binary compiles each `.ll` fixture in `bench/large-programs/fixtures/`
//! through the full compile pipeline and reports:
//!
//! - Fixture name
//! - Input IR size (bytes)
//! - Output text section size (bytes)
//! - Estimated instruction count (text_bytes / 4)
//! - Compile wall-clock time (ms)
//! - Peak RSS estimate (2× input IR rule check)
//!
//! Results are compared against `bench/large-programs/baselines.json`.
//! Pass `--update-baselines` to rewrite the baselines file with current values.
//!
//! # Usage
//! ```
//! cargo run -p llvm-in-rust --bin large-program-bench
//! cargo run -p llvm-in-rust --bin large-program-bench -- --update-baselines
//! cargo run -p llvm-in-rust --bin large-program-bench -- --fixture-dir path/to/fixtures
//! ```

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use llvm::compile::{compile_ir_to_object, host_object_format};
use llvm_codegen::ObjectFormat;
use llvm_transforms::OptLevel;

// ---------------------------------------------------------------------------
// Baseline JSON helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct BaselineEntry {
    text_bytes: usize,
    #[allow(dead_code)]
    compile_ms: u64,
}

fn load_baselines(path: &Path) -> std::collections::HashMap<String, BaselineEntry> {
    let mut map = std::collections::HashMap::new();
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return map,
    };
    // Minimal hand-rolled JSON parsing for our fixed schema.
    // Schema: { "name": { "text_bytes": N, "compile_ms": T }, ... }
    for line in text.lines() {
        let line = line.trim();
        if let Some(colon) = line.find(':') {
            let key_raw = line[..colon].trim().trim_matches('"');
            let rest = line[colon + 1..].trim();
            // We're looking for lines like:  "text_bytes": 123,
            if key_raw == "text_bytes" {
                // We'll just collect via the structured approach below.
                let _ = rest;
            }
        }
    }
    // Re-parse with a state machine for our schema.
    let mut current_name: Option<String> = None;
    let mut text_bytes: Option<usize> = None;
    let mut compile_ms: Option<u64> = None;
    for line in text.lines() {
        let line = line.trim();
        // Opening entry: "fixture_name": {
        if line.ends_with(": {") || line.ends_with(":{") {
            let name = line
                .trim_end_matches(":{")
                .trim_end_matches(": {")
                .trim()
                .trim_matches('"');
            if !name.is_empty() && name != "{" {
                current_name = Some(name.to_string());
                text_bytes = None;
                compile_ms = None;
            }
        }
        // Field: "text_bytes": N,  or "compile_ms": T
        if let Some(ref _name) = current_name {
            if let Some(v) = extract_u64_field(line, "text_bytes") {
                text_bytes = Some(v as usize);
            }
            if let Some(v) = extract_u64_field(line, "compile_ms") {
                compile_ms = Some(v);
            }
        }
        // Closing brace — commit entry
        if (line == "}" || line == "},") && current_name.is_some() {
            if let (Some(name), Some(tb), Some(cm)) =
                (current_name.take(), text_bytes.take(), compile_ms.take())
            {
                map.insert(name, BaselineEntry { text_bytes: tb, compile_ms: cm });
            }
        }
    }
    map
}

fn extract_u64_field(line: &str, field: &str) -> Option<u64> {
    // Match:  "field": 123,  or  "field": 123
    let needle = format!("\"{field}\":");
    let pos = line.find(&needle)?;
    let rest = line[pos + needle.len()..].trim().trim_end_matches(',');
    rest.parse::<u64>().ok()
}

fn save_baselines(
    path: &Path,
    entries: &[(String, BenchResult)],
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create dir {}: {e}", parent.display()))?;
    }
    let mut json = String::from("{\n");
    for (i, (name, r)) in entries.iter().enumerate() {
        let comma = if i + 1 < entries.len() { "," } else { "" };
        json.push_str(&format!(
            "  \"{name}\": {{ \"text_bytes\": {}, \"compile_ms\": {} }}{comma}\n",
            r.text_bytes, r.compile_ms
        ));
    }
    json.push_str("}\n");
    fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Benchmark result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct BenchResult {
    fixture:          String,
    input_bytes:      usize,
    text_bytes:       usize,
    /// text_bytes / 4 — rough instruction count (each x86 instr encoded ≥4 B on average)
    instr_estimate:   usize,
    compile_ms:       u64,
    /// Compiler-visible RSS estimate: 2× input bytes rule satisfied?
    rss_ok:           bool,
}

// ---------------------------------------------------------------------------
// Core benchmark
// ---------------------------------------------------------------------------

fn bench_fixture(path: &Path, fmt: ObjectFormat) -> Result<BenchResult, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let input_bytes = src.len();

    let t0 = Instant::now();
    let obj_bytes = compile_ir_to_object(&src, OptLevel::O2, fmt)?;
    let compile_ms = t0.elapsed().as_millis() as u64;

    // Find text section size by scanning the ELF/Mach-O/COFF structure.
    // As a portable approximation, count the actual output object size
    // and derive text bytes from the raw output (entire object minus headers).
    // We use the whole object size as an upper bound; the real text section
    // is extracted separately via a simple scan for the .text content length.
    let text_bytes = estimate_text_bytes(&obj_bytes, fmt);
    let instr_estimate = if text_bytes >= 4 { text_bytes / 4 } else { text_bytes };

    // RSS rule: object output must be <= 2× input IR size (memory efficiency check).
    // We approximate peak RSS as the output object size (worst-case allocator).
    let rss_ok = obj_bytes.len() <= 2 * input_bytes + 65536; // +64 KB for fixed overhead

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid filename: {}", path.display()))?;

    Ok(BenchResult {
        fixture:        stem.to_string(),
        input_bytes,
        text_bytes,
        instr_estimate,
        compile_ms,
        rss_ok,
    })
}

/// Extract text section byte count from an object file.
/// For ELF: scans section headers for .text; for Mach-O: scans for __text.
/// Falls back to (total_bytes / 2) if not found.
fn estimate_text_bytes(obj: &[u8], fmt: ObjectFormat) -> usize {
    match fmt {
        ObjectFormat::Elf => elf_text_size(obj).unwrap_or(obj.len() / 2),
        ObjectFormat::MachO => macho_text_size(obj).unwrap_or(obj.len() / 2),
        _ => obj.len() / 2,
    }
}

/// Read ELF-64 section header table and find .text section size.
fn elf_text_size(obj: &[u8]) -> Option<usize> {
    if obj.len() < 64 { return None; }
    if obj[0..4] != [0x7f, b'E', b'L', b'F'] { return None; }
    // ELF64 header offsets (all little-endian)
    let e_shoff     = u64::from_le_bytes(obj.get(40..48)?.try_into().ok()?) as usize;
    let e_shentsize = u16::from_le_bytes(obj.get(58..60)?.try_into().ok()?) as usize;
    let e_shnum     = u16::from_le_bytes(obj.get(60..62)?.try_into().ok()?) as usize;
    let e_shstrndx  = u16::from_le_bytes(obj.get(62..64)?.try_into().ok()?) as usize;

    if e_shoff == 0 || e_shentsize < 64 || e_shnum == 0 { return None; }

    // Section name string table
    let shstrtab_start = e_shoff + e_shstrndx * e_shentsize;
    if shstrtab_start + e_shentsize > obj.len() { return None; }
    let shstr_off  = u64::from_le_bytes(
        obj.get(shstrtab_start + 24..shstrtab_start + 32)?.try_into().ok()?,
    ) as usize;

    // Walk section headers looking for ".text"
    for i in 0..e_shnum {
        let sh = e_shoff + i * e_shentsize;
        if sh + e_shentsize > obj.len() { break; }
        let sh_name  = u32::from_le_bytes(obj.get(sh..sh + 4)?.try_into().ok()?) as usize;
        let sh_size  = u64::from_le_bytes(obj.get(sh + 32..sh + 40)?.try_into().ok()?) as usize;
        let name_off = shstr_off + sh_name;
        if obj.get(name_off..)?.starts_with(b".text\0") {
            return Some(sh_size);
        }
    }
    None
}

/// Read Mach-O 64-bit section table and find __text section size.
fn macho_text_size(obj: &[u8]) -> Option<usize> {
    if obj.len() < 32 { return None; }
    // magic: 0xFEEDFACF (LE) = CF FA ED FE
    if obj[0..4] != [0xCF, 0xFA, 0xED, 0xFE] { return None; }
    let ncmds    = u32::from_le_bytes(obj.get(16..20)?.try_into().ok()?) as usize;
    let mut off = 32usize; // mach_header_64 is 32 bytes
    for _ in 0..ncmds {
        if off + 8 > obj.len() { break; }
        let cmd     = u32::from_le_bytes(obj.get(off..off + 4)?.try_into().ok()?);
        let cmdsize = u32::from_le_bytes(obj.get(off + 4..off + 8)?.try_into().ok()?) as usize;
        if cmd == 0x19 {
            // LC_SEGMENT_64: segname at +8 (16 bytes), nsects at +64
            let nsects = u32::from_le_bytes(
                obj.get(off + 64..off + 68)?.try_into().ok()?,
            ) as usize;
            let sec_base = off + 72; // first section_64
            for s in 0..nsects {
                let sec = sec_base + s * 80; // sizeof(section_64) = 80
                if sec + 80 > obj.len() { break; }
                // sectname at sec+0 (16 bytes), segname at sec+16 (16 bytes)
                let sectname = &obj[sec..sec + 16];
                let segname  = &obj[sec + 16..sec + 32];
                if segname.starts_with(b"__TEXT\0")
                    && sectname.starts_with(b"__text\0")
                {
                    let size = u64::from_le_bytes(
                        obj.get(sec + 40..sec + 48)?.try_into().ok()?,
                    ) as usize;
                    return Some(size);
                }
            }
        }
        if cmdsize == 0 { break; }
        off += cmdsize;
    }
    None
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let update_baselines = args.iter().any(|a| a == "--update-baselines");
    let fixture_dir = args
        .iter()
        .position(|a| a == "--fixture-dir")
        .and_then(|i| args.get(i + 1))
        .map(|s| PathBuf::from(s))
        .unwrap_or_else(|| PathBuf::from("bench/large-programs/fixtures"));
    let baselines_path = PathBuf::from("bench/large-programs/baselines.json");
    // Regression threshold: instruction count must not be >20% above baseline.
    let threshold = 0.20f64;

    // --- collect fixture files ---
    let mut paths: Vec<PathBuf> = match fs::read_dir(&fixture_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("ll"))
            .collect(),
        Err(e) => {
            eprintln!("error: cannot read fixture dir {}: {e}", fixture_dir.display());
            return ExitCode::FAILURE;
        }
    };
    paths.sort();

    if paths.is_empty() {
        eprintln!("error: no .ll fixtures found in {}", fixture_dir.display());
        return ExitCode::FAILURE;
    }

    let fmt = host_object_format();
    let baselines = load_baselines(&baselines_path);

    // --- run benchmarks ---
    let mut results: Vec<BenchResult> = Vec::new();
    let mut any_failure = false;

    for path in &paths {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        eprint!("  compiling {name} ... ");
        match bench_fixture(path, fmt) {
            Ok(r) => {
                eprintln!(
                    "ok  ({} ms, {} text bytes, ~{} instrs)",
                    r.compile_ms, r.text_bytes, r.instr_estimate
                );
                results.push(r);
            }
            Err(e) => {
                eprintln!("FAILED: {e}");
                any_failure = true;
            }
        }
    }

    if any_failure {
        eprintln!("error: one or more fixtures failed to compile");
        return ExitCode::FAILURE;
    }

    // --- print results table ---
    println!();
    println!(
        "{:<22}  {:>10}  {:>10}  {:>10}  {:>10}  {}",
        "fixture", "in (B)", "text (B)", "~instrs", "time (ms)", "rss"
    );
    println!("{}", "-".repeat(80));
    for r in &results {
        println!(
            "{:<22}  {:>10}  {:>10}  {:>10}  {:>10}  {}",
            r.fixture,
            r.input_bytes,
            r.text_bytes,
            r.instr_estimate,
            r.compile_ms,
            if r.rss_ok { "ok" } else { "WARN: >2x input" },
        );
    }
    println!();

    // --- compare to baselines ---
    let mut regression = false;
    if !update_baselines && !baselines.is_empty() {
        println!("Baseline comparison (threshold: ±{:.0}%):", threshold * 100.0);
        println!("{}", "-".repeat(80));
        for r in &results {
            if let Some(bl) = baselines.get(&r.fixture) {
                let delta = (r.text_bytes as f64 - bl.text_bytes as f64) / bl.text_bytes.max(1) as f64;
                let flag = if delta > threshold {
                    regression = true;
                    "  !! REGRESSION"
                } else if delta < -threshold {
                    "  ** improvement"
                } else {
                    ""
                };
                println!(
                    "  {:<22}  text_bytes: {:>6} vs baseline {:>6}  ({:+.1}%){flag}",
                    r.fixture, r.text_bytes, bl.text_bytes, delta * 100.0
                );
            } else {
                println!("  {:<22}  (no baseline — run with --update-baselines)", r.fixture);
            }
        }
        println!();
    }

    // --- update baselines ---
    if update_baselines {
        let entries: Vec<(String, BenchResult)> = results
            .iter()
            .map(|r| (r.fixture.clone(), r.clone()))
            .collect();
        match save_baselines(&baselines_path, &entries) {
            Ok(()) => println!("baselines updated: {}", baselines_path.display()),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    // --- RSS check ---
    let rss_failures: Vec<&str> = results
        .iter()
        .filter(|r| !r.rss_ok)
        .map(|r| r.fixture.as_str())
        .collect();
    if !rss_failures.is_empty() {
        eprintln!(
            "warning: RSS budget exceeded for: {}",
            rss_failures.join(", ")
        );
    }

    if regression {
        eprintln!("FAIL: instruction count regression detected (>{:.0}% above baseline)", threshold * 100.0);
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
