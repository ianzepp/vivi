//! `vivi flow list|show`: the shipped flows and a need's run index.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vivi::VivariumError;
use vivi::cli::FlowCommand;
use vivi::mailspace::Mailspace;
use vivi::pump::config::PumpConfig;
use vivi::pump::flow::def::FlowKind;
use vivi::pump::flow::index::{RunRow, StepRow, StepState};
use vivi::pump::flow::registry::{self, FLOW_VERSION};
use vivi::pump::store::{DB_FILENAME, PumpStore};

const KINDS: [FlowKind; 4] = [
    FlowKind::Build,
    FlowKind::Research,
    FlowKind::Release,
    FlowKind::Repair,
];
const NO_DB: &str = "no pump.db here";

type Runs = Vec<(RunRow, Vec<StepRow>)>;

pub(crate) fn handle_flow_command(command: &FlowCommand) -> Result<(), VivariumError> {
    match command {
        FlowCommand::List { json } => list(*json),
        FlowCommand::Show {
            kind,
            need,
            project,
            json,
        } => show(kind, need.as_deref(), project.as_deref(), *json),
    }
}

fn list(json: bool) -> Result<(), VivariumError> {
    let names: Vec<&str> = KINDS.iter().map(|kind| kind.name()).collect();
    if json {
        return print_json(&json!(names));
    }
    for name in names {
        println!("{name}");
    }
    Ok(())
}

fn show(
    kind: &str,
    need: Option<&str>,
    project: Option<&Path>,
    json: bool,
) -> Result<(), VivariumError> {
    let kind = FlowKind::parse(kind).ok_or_else(|| {
        VivariumError::Message(format!(
            "unknown flow kind {kind:?}; expected build, research, release or repair"
        ))
    })?;
    let source = registry::shipped(kind).ok_or_else(|| {
        VivariumError::Message(format!("the {} flow is not shipped yet", kind.name()))
    })?;
    // Outer Option: `--need` given. Inner Option: a pump.db exists.
    let runs: Option<Option<Runs>> = match need {
        Some(need) => Some(run_index(need, &project_root(project))?),
        None => None,
    };
    if json {
        let mut out = json!({
            "kind": kind.name(),
            "version": FLOW_VERSION,
            "mermaid": source,
        });
        if let Some(runs) = &runs {
            out["runs"] = runs.as_deref().map_or(Value::Null, runs_json);
        }
        return print_json(&out);
    }
    print!("{source}");
    if !source.ends_with('\n') {
        println!();
    }
    if let (Some(need), Some(runs)) = (need, &runs) {
        print_runs(need, runs.as_deref());
    }
    Ok(())
}

/// The runs of `need` with their steps, or `None` when there is no pump.db.
fn run_index(need: &str, project: &Path) -> Result<Option<Runs>, VivariumError> {
    let cfg = match Mailspace::discover(Some(project)) {
        Ok(mailspace) => mailspace.config.pump.clone().unwrap_or_default(),
        Err(_) => PumpConfig::default(),
    };
    let path = cfg.home_dir(project).join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
        return Ok(None);
    };
    let mut runs = Vec::new();
    for run in store.runs_of(need)? {
        let steps = store.steps_of(need, run.run)?;
        runs.push((run, steps));
    }
    Ok(Some(runs))
}

fn print_runs(need: &str, runs: Option<&[(RunRow, Vec<StepRow>)]>) {
    println!();
    let Some(runs) = runs else {
        println!("{NO_DB}");
        return;
    };
    if runs.is_empty() {
        println!("no runs for {need}");
        return;
    }
    for (run, steps) in runs {
        println!(
            "run {} {} entry {} state {}",
            run.run, run.flow, run.entry, run.state
        );
        for step in steps {
            println!(
                "  {} {} {} {}",
                step.node,
                step.task,
                state_word(step.state),
                step.verdict.as_deref().unwrap_or("-")
            );
        }
    }
}

fn runs_json(runs: &[(RunRow, Vec<StepRow>)]) -> Value {
    Value::Array(
        runs.iter()
            .map(|(run, steps)| {
                json!({
                    "run": run.run,
                    "flow": run.flow,
                    "flow_version": run.flow_version,
                    "entry": run.entry,
                    "state": run.state,
                    "steps": steps.iter().map(|step| json!({
                        "node": step.node,
                        "task": step.task,
                        "state": state_word(step.state),
                        "verdict": step.verdict,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn state_word(state: StepState) -> &'static str {
    match state {
        StepState::Pending => "pending",
        StepState::Running => "running",
        StepState::Waiting => "waiting",
        StepState::Done => "done",
        StepState::Failed => "failed",
    }
}

fn print_json(value: &Value) -> Result<(), VivariumError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
    );
    Ok(())
}

fn project_root(project: Option<&Path>) -> PathBuf {
    project
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
