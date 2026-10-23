//! `vivi pump adopt`: open a pump record for every need the pump already has
//! in flight.
//!
//! Before vivi 19 the pump planned any need that carried the approval mark.
//! From 19 it plans exactly the needs that have an open pump record, and it
//! replies progress on that record. Work that was in flight at the upgrade has
//! no record, so its replies would be dropped. This command reads the pump's
//! own state and opens one record for each need the pump has planned (or is
//! planning) and has not finished, unless it already has one.
//!
//! It is idempotent: a need that has an open record is left alone, so a
//! second run opens nothing. It never touches a need the pump finished
//! (complete, withdrawn or closed), and a need that is not a need in the
//! mailspace (the synthetic `pumpfix-*` repair needs of Vivi 15) is reported
//! as skipped, not as an error. Nothing is printed by [`adopt`]; [`run`]
//! prints its report.

use std::collections::BTreeSet;
use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpAdoptCommand;
use crate::mailspace::Mailspace;
use crate::pump::control::project_home;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// What one adopt pass did, need by need.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AdoptReport {
    /// Needs that got a record: `(need, record handle)`. The handle is empty
    /// in a dry run, which opens nothing.
    pub opened: Vec<(String, String)>,
    /// Needs that already had an open record: `(need, record handle)`.
    pub kept: Vec<(String, String)>,
    /// Needs that could not be adopted: `(need, why)`.
    pub skipped: Vec<(String, String)>,
}

/// The needs the pump has planned or is planning and has not finished,
/// sorted. Finished means complete, withdrawn, or in `closed_needs`.
#[must_use]
pub fn in_flight_needs(st: &State) -> Vec<String> {
    let mut needs: BTreeSet<&String> = st.needs.keys().collect();
    needs.extend(st.board.revision_planning.keys());
    needs
        .into_iter()
        .filter(|need| !st.closed_needs.contains(*need))
        .filter(|need| !st.needs.get(*need).is_some_and(|record| record.withdrawn))
        .filter(|need| !crate::pump::report::need_complete(st, need))
        .cloned()
        .collect()
}

/// Open a pump record for every in-flight need of `st` that has none. With
/// `dry_run` nothing is opened and the report says what would be.
///
/// # Errors
/// Returns an error only when the mailspace cannot be read at all; a need
/// that cannot be adopted is reported in the result.
pub fn adopt(
    mailspace: &Mailspace,
    st: &State,
    dry_run: bool,
) -> Result<AdoptReport, VivariumError> {
    let mut report = AdoptReport::default();
    for need in in_flight_needs(st) {
        let existing = match mailspace.pump_open_record(&need) {
            Ok(existing) => existing,
            Err(error) => {
                report.skipped.push((need, error.to_string()));
                continue;
            }
        };
        if let Some(record) = existing {
            report.kept.push((need, record.handle));
            continue;
        }
        if dry_run {
            report.opened.push((need, String::new()));
            continue;
        }
        let body = format!(
            "Adopted need {need}: the pump had it in flight before pump records existed (vivi pump adopt)."
        );
        match mailspace.pump_send(None, &need, Some(&body)) {
            Ok(record) => report.opened.push((need, record.handle)),
            Err(error) => report.skipped.push((need, error.to_string())),
        }
    }
    Ok(report)
}

/// `vivi pump adopt [--dry-run]`.
///
/// # Errors
/// Returns an error when the mailspace has no pump database or the pump's
/// state cannot be read.
pub fn run(project: &Path, cmd: &PumpAdoptCommand) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    let path = home.join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    };
    let st = store.load()?;
    let mailspace = Mailspace::discover(Some(project))?;
    let report = adopt(&mailspace, &st, cmd.dry_run)?;
    print_report(&report, cmd.dry_run);
    Ok(())
}

fn print_report(report: &AdoptReport, dry_run: bool) {
    for (need, record) in &report.opened {
        if dry_run {
            println!("would adopt need {need}");
        } else {
            println!("adopted need {need}: pump record {record}");
        }
    }
    for (need, record) in &report.kept {
        println!("need {need} already has open pump record {record}");
    }
    for (need, why) in &report.skipped {
        println!("skipped need {need}: {why}");
    }
    let verb = if dry_run { "would adopt" } else { "adopted" };
    println!(
        "{verb} {} need(s); {} already had a record; {} skipped",
        report.opened.len(),
        report.kept.len(),
        report.skipped.len()
    );
}

#[cfg(test)]
#[path = "adopt_test.rs"]
mod tests;
