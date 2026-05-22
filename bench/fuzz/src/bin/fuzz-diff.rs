//! CLI binary for the differential fuzzing harness.
//!
//! Usage:
//!   fuzz-diff [count] [base_seed]
//!
//! Defaults: count=100, base_seed=42
//!
//! Exits with code 1 if any mismatches are found.

use fuzz_diff::run_campaign_from;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let count: u32 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let seed: u64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);

    println!(
        "Running {} differential fuzz programs (base_seed={})...",
        count, seed
    );

    let mismatches = run_campaign_from(seed, count);

    println!("Ran {} programs, {} mismatches", count, mismatches.len());

    for m in &mismatches {
        println!(
            "MISMATCH seed={}: expected={}, actual={:?}",
            m.seed, m.expected, m.actual
        );
        println!("{}", m.ir_text);
    }

    if !mismatches.is_empty() {
        std::process::exit(1);
    }
}
