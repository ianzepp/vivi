//! `vivi-sim`: run every scenario under `scenarios/` (or those whose directory
//! name contains a filter) against the real pump on a virtual clock.
//!
//! Usage: `cargo run --release -- [--verbose] [FILTER...]`
//! Exit status: 0 when every scenario passed, 1 when any failed, 2 on a
//! scenario that could not be read or set up.

mod host;
mod project;
mod run;
mod scenario;

use std::path::PathBuf;
use std::process::ExitCode;

use run::Outcome;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let scenarios = match scenario::discover(&root) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("vivi-sim: {e}");
            return ExitCode::from(2);
        }
    };
    let chosen: Vec<_> = scenarios
        .iter()
        .filter(|s| filters.is_empty() || filters.iter().any(|f| dir_name(s).contains(f.as_str())))
        .collect();
    let mut failed = 0;
    for scenario in &chosen {
        match run::run(scenario) {
            Ok(outcome) => {
                report(&dir_name(scenario), &outcome, verbose);
                failed += usize::from(!outcome.failures.is_empty());
            }
            Err(e) => {
                eprintln!("vivi-sim: {}: setup failed: {e}", dir_name(scenario));
                return ExitCode::from(2);
            }
        }
    }
    println!("\n{} scenario(s), {} failed", chosen.len(), failed);
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn dir_name(scenario: &scenario::Scenario) -> String {
    scenario
        .dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

fn report(dir: &str, outcome: &Outcome, verbose: bool) {
    let verdict = if outcome.failures.is_empty() {
        "PASS"
    } else {
        "FAIL"
    };
    println!(
        "{verdict} {dir} ({}): {} ticks, {:.1} virtual h in {:.2}s wall; {} commands, {} spawned, {} exceptions",
        outcome.name,
        outcome.ticks,
        outcome.virtual_secs as f64 / 3600.0,
        outcome.wall_secs,
        outcome.record.commands,
        outcome.record.spawned,
        outcome.record.exceptions.len(),
    );
    for failure in &outcome.failures {
        println!("  fail: {failure}");
    }
    for error in outcome.tick_errors.iter().take(5) {
        println!("  tick error: {error}");
    }
    for exception in outcome.record.exceptions.iter().take(5) {
        println!("  exception: {exception}");
    }
    if verbose {
        println!("  about: {}", outcome.description.trim());
        for line in outcome
            .record
            .logs
            .iter()
            .filter(|l| l.contains("skipped") || l.contains("refused") || l.contains("failed"))
            .take(5)
        {
            println!("  log: {line}");
        }
        for (shape, count) in &outcome.record.unscripted {
            println!("  unscripted: {count:>6}  {shape}");
        }
    }
}
