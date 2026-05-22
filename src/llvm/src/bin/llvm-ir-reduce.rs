//! `llvm-ir-reduce` — delta-debugging IR minimizer.
//!
//! Reduces a failing LLVM IR program to the smallest version that still
//! triggers a user-supplied predicate.
//!
//! USAGE:
//!   llvm-ir-reduce <input.ll> --predicate <substring>
//!
//! OPTIONS:
//!   <input.ll>              Path to the input LLVM IR file
//!   --predicate <text>      Substring that must appear in the reduced IR
//!   --help, -h              Print this help message
//!
//! EXAMPLES:
//!   llvm-ir-reduce buggy.ll --predicate "add i32"
//!   llvm-ir-reduce crash.ll --predicate "@problematic_fn"
//!
//! Prints the reduced IR to stdout.

use std::{env, fs, process::ExitCode};

use llvm_ir::reduce::{ContainsPredicate, Predicate, Reducer};
use llvm_ir::Printer;
use llvm_ir_parser::parser::parse;

fn usage() {
    eprintln!(concat!(
        "USAGE: llvm-ir-reduce <input.ll> --predicate <substring>\n",
        "\n",
        "Reduces input.ll to the smallest IR that still contains <substring>.\n",
        "Prints the reduced IR to stdout.\n",
        "\n",
        "OPTIONS:\n",
        "  <input.ll>              Input LLVM IR file\n",
        "  --predicate <text>      Substring to preserve\n",
        "  --help, -h              Print this help message\n",
        "\n",
        "EXAMPLES:\n",
        "  llvm-ir-reduce buggy.ll --predicate \"add i32\"\n",
        "  llvm-ir-reduce crash.ll --predicate \"@problematic_fn\"",
    ));
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1).peekable();

    let mut input: Option<String> = None;
    let mut predicate: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                usage();
                return ExitCode::SUCCESS;
            }
            "--predicate" => {
                predicate = Some(
                    args.next().unwrap_or_else(|| die("--predicate requires an argument")),
                );
            }
            s if s.starts_with("--predicate=") => {
                predicate = Some(s["--predicate=".len()..].to_owned());
            }
            s if s.starts_with('-') => {
                eprintln!("error: unknown option: {s}");
                usage();
                return ExitCode::from(2);
            }
            s => {
                if input.is_some() {
                    die("only one input file is supported");
                }
                input = Some(s.to_owned());
            }
        }
    }

    let input = match input {
        Some(p) => p,
        None => {
            eprintln!("error: no input file specified");
            usage();
            return ExitCode::from(2);
        }
    };
    let substring = match predicate {
        Some(p) => p,
        None => {
            eprintln!("error: --predicate is required");
            usage();
            return ExitCode::from(2);
        }
    };

    let src = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => die(&format!("cannot read {input}: {e}")),
    };

    let (ctx, module) = match parse(&src) {
        Ok(pair) => pair,
        Err(e) => die(&format!("parse error: {e}")),
    };

    let pred = ContainsPredicate { substring };

    // Verify the predicate holds on the original input.
    if !pred.test(&ctx, &module) {
        eprintln!(
            "error: predicate does not hold on the original input -- \
             substring {:?} not found in printed IR",
            pred.substring
        );
        return ExitCode::from(1);
    }

    let reducer = Reducer::new();
    let (ctx2, module2) = reducer.reduce(ctx, module, &pred);

    let out = Printer::new(&ctx2).print_module(&module2);
    print!("{}", out);

    ExitCode::SUCCESS
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}
