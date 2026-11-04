use std::path::Path;
use std::process;

use vivi_core::cli::DoctorCommand;
use vivi_doctor::doctor::{DoctorReport, Finding, Project, Verdict};
use vivi_error::VivariumError;

/// Gather and print the project-wide read-only doctor report.
pub(crate) fn run(
    command: &DoctorCommand,
    global_project: Option<&Path>,
) -> Result<(), VivariumError> {
    let project_path = command.project.as_deref().or(global_project);
    let report = report(project_path);
    if command.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| VivariumError::Other(
                format!("failed to encode doctor JSON: {error}")
            ))?
        );
    } else {
        print_text(&report, command.fix_hints);
    }
    process::exit(exit_code(report.verdict));
}

fn report(project_path: Option<&Path>) -> DoctorReport {
    let release_result =
        vivi_update::update::compare_latest().map_err(|_| "release lookup unavailable".to_string());
    let release = Some(match &release_result {
        Ok(outcome) => Ok(outcome),
        Err(reason) => Err(reason.clone()),
    });
    let targets: Vec<_> = vivi_skills::skills::recorded_targets()
        .into_iter()
        .map(|path| {
            let status = vivi_skills::skills::copy_status(&path);
            (path, status)
        })
        .collect();
    let mut findings = vivi_doctor::doctor::install_section::findings(
        env!("CARGO_PKG_VERSION"),
        release,
        &targets,
    );

    let project = Project::discover(project_path);
    match project.as_ref() {
        Ok(project) => gather_project_sections(project, &mut findings),
        Err(_) => gather_unavailable_project_sections(project_path, &mut findings),
    }
    findings.extend(vivi_doctor::doctor::proxy_section::findings(None));
    if let Ok(project) = project {
        findings.extend(vivi_doctor::doctor::disk_section::findings(&project));
    } else {
        findings.push(skipped("disk", "project", "mailspace config unavailable"));
    }
    DoctorReport::from_findings(findings)
}

fn gather_project_sections(project: &Project, findings: &mut Vec<Finding>) {
    findings.extend(vivi_doctor::doctor::mailspace_section::findings(Some(
        &project.root,
    )));
    findings.extend(vivi_doctor::doctor::roles_section::findings(project));
    let reachable = vivi_doctor::doctor::sync_section::probe_server(project);
    findings.extend(vivi_doctor::doctor::sync_section::findings(
        project,
        chrono::Utc::now(),
        reachable,
    ));
    findings.extend(vivi_doctor::doctor::pools_section::findings(Some(
        &project.root,
    )));
    findings.extend(vivi_doctor::doctor::pump_section::findings(project));
}

fn gather_unavailable_project_sections(project_path: Option<&Path>, findings: &mut Vec<Finding>) {
    findings.extend(vivi_doctor::doctor::mailspace_section::findings(
        project_path,
    ));
    findings.push(skipped("roles", "roster", "mailspace config unavailable"));
    findings.push(skipped("sync", "config", "mailspace config unavailable"));
    findings.push(skipped("pools", "config", "mailspace config unavailable"));
    findings.push(skipped("pump", "pump home", "mailspace config unavailable"));
}

fn skipped(section: &str, name: &str, reason: &str) -> Finding {
    Finding::skipped(section, name, reason)
}

fn print_text(report: &DoctorReport, fix_hints: bool) {
    println!("verdict {}", report.verdict.word());
    for finding in &report.findings {
        println!(
            "[{}] {}/{}: {}",
            finding.status.word(),
            finding.section,
            finding.name,
            finding.detail
        );
        if fix_hints && let Some(fix) = finding.fix.as_deref() {
            println!("  fix: {fix}");
        }
    }
}

const fn exit_code(verdict: Verdict) -> i32 {
    match verdict {
        Verdict::Ok => 0,
        Verdict::Warn => 1,
        Verdict::Fail => 2,
    }
}
