//! `vivi flow list|show`: the shipped flows and a need's run index.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vivi::VivariumError;
use vivi::cli::FlowCommand;
use vivi::pump::flow::def::FlowKind;
use vivi::pump::flow::index::{RunRow, StepRow, StepState};
use vivi::pump::flow::registry::FLOW_VERSION;
use vivi::pump::flow::view;

const KINDS: [FlowKind; 4] = [
    FlowKind::Build,
    FlowKind::Research,
    FlowKind::Release,
    FlowKind::Repair,
];
const NO_DB: &str = "no pump.db here";

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
    let flow = view(&project_root(project), kind, need)?;
    if json {
        let mut out = json!({
            "kind": flow.kind.name(),
            "version": FLOW_VERSION,
            "mermaid": flow.mermaid,
        });
        if let Some(runs) = &flow.runs {
            out["runs"] = runs.as_deref().map_or(Value::Null, runs_json);
        }
        return print_json(&out);
    }
    print!("{}", flow.mermaid);
    if !flow.mermaid.ends_with('\n') {
        println!();
    }
    if let (Some(need), Some(runs)) = (need, &flow.runs) {
        print_runs(need, runs.as_deref());
    }
    Ok(())
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
