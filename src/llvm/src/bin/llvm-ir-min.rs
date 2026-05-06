use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

struct Config {
    input: PathBuf,
    predicate: String,
    output: PathBuf,
    evidence_dir: Option<PathBuf>,
    log: Option<PathBuf>,
    bucket_signature: Option<String>,
}

struct ReductionStats {
    attempts: usize,
    removals: usize,
}

fn usage() {
    eprintln!(
        "Usage: llvm-ir-min --input <file.ll> --predicate <cmd with {{input}}> \
         [--output <min.ll>] [--evidence-dir <dir>] [--log <reducer.log>] \
         [--bucket-signature <text>]"
    );
}

fn parse_args() -> Result<Config, String> {
    let mut input: Option<PathBuf> = None;
    let mut predicate: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut evidence_dir: Option<PathBuf> = None;
    let mut log: Option<PathBuf> = None;
    let mut bucket_signature: Option<String> = None;

    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--input" => input = it.next().map(PathBuf::from),
            "--predicate" => predicate = it.next(),
            "--output" => output = it.next().map(PathBuf::from),
            "--evidence-dir" => evidence_dir = it.next().map(PathBuf::from),
            "--log" => log = it.next().map(PathBuf::from),
            "--bucket-signature" => bucket_signature = it.next(),
            "-h" | "--help" => {
                usage();
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    let input = input.ok_or("missing --input".to_string())?;
    let predicate = predicate.ok_or("missing --predicate".to_string())?;
    let output = output.unwrap_or_else(|| {
        evidence_dir
            .as_ref()
            .map(|dir| dir.join("minimized.ll"))
            .unwrap_or_else(|| PathBuf::from("minimized.ll"))
    });
    let log = log.or_else(|| evidence_dir.as_ref().map(|dir| dir.join("reducer.log")));

    Ok(Config {
        input,
        predicate,
        output,
        evidence_dir,
        log,
        bucket_signature,
    })
}

fn run_predicate(cmd_tpl: &str, input_path: &Path) -> Result<bool, String> {
    let input = input_path
        .to_str()
        .ok_or("input path is not valid UTF-8".to_string())?;
    let cmd = cmd_tpl.replace("{{input}}", input);
    let status = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status()
        .map_err(|e| format!("failed to run predicate: {e}"))?;
    Ok(!status.success())
}

fn has_ir_shape(text: &str) -> bool {
    text.lines().any(|l| l.trim_start().starts_with("define "))
}

fn minimize_lines(
    original: &str,
    predicate: &str,
    work_file: &Path,
    log: &mut Vec<String>,
) -> Result<(String, ReductionStats), String> {
    let mut lines: Vec<String> = original.lines().map(|s| s.to_string()).collect();
    let mut changed = true;
    let mut stats = ReductionStats {
        attempts: 0,
        removals: 0,
    };

    while changed {
        changed = false;
        let mut i = 0usize;
        while i < lines.len() {
            let mut trial = lines.clone();
            let removed = trial.remove(i);
            let trial_text = format!("{}\n", trial.join("\n"));
            if !has_ir_shape(&trial_text) {
                log.push(format!("skip line {i}: removing it loses IR shape"));
                i += 1;
                continue;
            }
            stats.attempts += 1;
            fs::write(work_file, &trial_text).map_err(|e| format!("write trial file failed: {e}"))?;
            if run_predicate(predicate, work_file)? {
                log.push(format!("remove line {i}: {removed}"));
                lines = trial;
                changed = true;
                stats.removals += 1;
            } else {
                log.push(format!("keep line {i}: predicate no longer fails"));
                i += 1;
            }
        }
    }

    Ok((format!("{}\n", lines.join("\n")), stats))
}

fn write_evidence_package(config: &Config, original: &str, minimized: &str, log_text: &str) -> Result<(), String> {
    let Some(dir) = &config.evidence_dir else {
        return Ok(());
    };

    fs::create_dir_all(dir).map_err(|e| format!("failed to create evidence dir {}: {e}", dir.display()))?;
    fs::write(dir.join("original.ll"), original)
        .map_err(|e| format!("failed to write original.ll: {e}"))?;
    fs::write(dir.join("minimized.ll"), minimized)
        .map_err(|e| format!("failed to write minimized.ll: {e}"))?;
    fs::write(dir.join("reducer.log"), log_text)
        .map_err(|e| format!("failed to write reducer.log: {e}"))?;
    fs::write(
        dir.join("repro.sh"),
        format!("#!/usr/bin/env sh\nset -eu\n{}\n", config.predicate.replace("{{input}}", "${1:-minimized.ll}")),
    )
    .map_err(|e| format!("failed to write repro.sh: {e}"))?;
    let _ = Command::new("chmod").arg("+x").arg(dir.join("repro.sh")).status();

    let manifest = format!(
        "input={}\noutput={}\nbucket_signature={}\noriginal_bytes={}\nminimized_bytes={}\n",
        config.input.display(),
        config.output.display(),
        config.bucket_signature.as_deref().unwrap_or("unbucketed"),
        original.len(),
        minimized.len()
    );
    fs::write(dir.join("manifest.txt"), manifest)
        .map_err(|e| format!("failed to write manifest.txt: {e}"))?;
    Ok(())
}

fn main() -> ExitCode {
    let config = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            usage();
            return ExitCode::from(2);
        }
    };

    let original = match fs::read_to_string(&config.input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", config.input.display());
            return ExitCode::from(1);
        }
    };

    if !has_ir_shape(&original) {
        eprintln!("error: input does not look like LLVM IR (no `define` found)");
        return ExitCode::from(1);
    }

    if let Err(e) = run_predicate(&config.predicate, &config.input).and_then(|failing| {
        if failing {
            Ok(())
        } else {
            Err("predicate does not fail on original input".to_string())
        }
    }) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }

    let tmp = config.output.with_extension("tmp.ll");
    let mut log = vec![format!("input: {}", config.input.display())];
    if let Some(signature) = &config.bucket_signature {
        log.push(format!("bucket_signature: {signature}"));
    }
    let (minimized, stats) = match minimize_lines(&original, &config.predicate, &tmp, &mut log) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: minimization failed: {e}");
            let _ = fs::remove_file(&tmp);
            return ExitCode::from(1);
        }
    };
    let _ = fs::remove_file(&tmp);

    if let Some(parent) = config.output.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!("error: failed to create {}: {e}", parent.display());
                return ExitCode::from(1);
            }
        }
    }
    if let Err(e) = fs::write(&config.output, &minimized) {
        eprintln!("error: failed to write {}: {e}", config.output.display());
        return ExitCode::from(1);
    }

    log.push(format!("attempts: {}", stats.attempts));
    log.push(format!("removals: {}", stats.removals));
    log.push(format!("original_bytes: {}", original.len()));
    log.push(format!("minimized_bytes: {}", minimized.len()));
    let log_text = format!("{}\n", log.join("\n"));

    if let Some(log_path) = &config.log {
        if let Some(parent) = log_path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent) {
                    eprintln!("error: failed to create {}: {e}", parent.display());
                    return ExitCode::from(1);
                }
            }
        }
        if let Err(e) = fs::write(log_path, &log_text) {
            eprintln!("error: failed to write {}: {e}", log_path.display());
            return ExitCode::from(1);
        }
    }

    if let Err(e) = write_evidence_package(&config, &original, &minimized, &log_text) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }

    println!(
        "minimized {} -> {} ({} -> {} bytes, {} removals)",
        config.input.display(),
        config.output.display(),
        original.len(),
        minimized.len(),
        stats.removals
    );

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_define_shape() {
        assert!(has_ir_shape("define i32 @main() {\nret i32 0\n}"));
        assert!(!has_ir_shape("; comment only"));
    }
}
