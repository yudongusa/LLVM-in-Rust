use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn usage() {
    eprintln!(
        "Usage: llvm-ir-min --input <file.ll> --predicate <cmd with {{input}}> [--output <min.ll>]"
    );
}

fn parse_args() -> Result<(PathBuf, String, PathBuf), String> {
    let mut input: Option<PathBuf> = None;
    let mut predicate: Option<String> = None;
    let mut output: Option<PathBuf> = None;

    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--input" => input = it.next().map(PathBuf::from),
            "--predicate" => predicate = it.next(),
            "--output" => output = it.next().map(PathBuf::from),
            "-h" | "--help" => {
                usage();
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    let input = input.ok_or("missing --input".to_string())?;
    let predicate = predicate.ok_or("missing --predicate".to_string())?;
    let output = output.unwrap_or_else(|| PathBuf::from("minimized.ll"));
    Ok((input, predicate, output))
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

fn minimize_lines(original: &str, predicate: &str, work_file: &Path) -> Result<String, String> {
    let mut lines: Vec<String> = original.lines().map(|s| s.to_string()).collect();
    let mut changed = true;

    while changed {
        changed = false;
        let mut i = 0usize;
        while i < lines.len() {
            let mut trial = lines.clone();
            trial.remove(i);
            let trial_text = format!("{}\n", trial.join("\n"));
            if !has_ir_shape(&trial_text) {
                i += 1;
                continue;
            }
            fs::write(work_file, &trial_text).map_err(|e| format!("write trial file failed: {e}"))?;
            if run_predicate(predicate, work_file)? {
                lines = trial;
                changed = true;
            } else {
                i += 1;
            }
        }
    }

    Ok(format!("{}\n", lines.join("\n")))
}

fn main() -> ExitCode {
    let (input, predicate, output) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            usage();
            return ExitCode::from(2);
        }
    };

    let original = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", input.display());
            return ExitCode::from(1);
        }
    };

    if !has_ir_shape(&original) {
        eprintln!("error: input does not look like LLVM IR (no `define` found)");
        return ExitCode::from(1);
    }

    if let Err(e) = run_predicate(&predicate, &input).and_then(|failing| {
        if failing {
            Ok(())
        } else {
            Err("predicate does not fail on original input".to_string())
        }
    }) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }

    let tmp = output.with_extension("tmp.ll");
    let minimized = match minimize_lines(&original, &predicate, &tmp) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: minimization failed: {e}");
            let _ = fs::remove_file(&tmp);
            return ExitCode::from(1);
        }
    };
    let _ = fs::remove_file(&tmp);

    if let Err(e) = fs::write(&output, &minimized) {
        eprintln!("error: failed to write {}: {e}", output.display());
        return ExitCode::from(1);
    }

    println!(
        "minimized {} -> {} ({} -> {} bytes)",
        input.display(),
        output.display(),
        original.len(),
        minimized.len()
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
