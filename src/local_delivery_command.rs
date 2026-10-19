use std::process::Command as ProcessCommand;

use serde::{Deserialize, Serialize};
use vivi::VivariumError;
use vivi::cli::DeliveryCommand;
use vivi::mailspace::{Mailspace, SendRequest};

#[derive(Debug, Serialize, Deserialize)]
struct DeliveryNeed {
    schema: u8,
    branch: String,
    local_branch: String,
    tip: String,
    base: String,
    remote: String,
    push: PushReceipt,
    gate: GateReceipt,
    closes: Vec<String>,
    blocked: Vec<BlockedNeed>,
    filed: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PushReceipt {
    status: String,
    remote_ref: String,
    detail: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GateReceipt {
    command: String,
    result: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BlockedNeed {
    handle: String,
    reason: String,
}

struct DeliverySend<'a> {
    from: &'a str,
    to: &'a str,
    branch: &'a str,
    tip: &'a str,
    base: &'a str,
    remote: &'a str,
    gate: &'a str,
    gate_result: &'a str,
    closes: &'a [String],
    blocked: &'a [String],
    filed: &'a [String],
    project: Option<&'a std::path::Path>,
}

pub(crate) fn handle(command: &DeliveryCommand) -> Result<(), VivariumError> {
    match command {
        DeliveryCommand::Send {
            from_identity,
            to,
            branch,
            tip,
            base,
            remote,
            gate,
            gate_result,
            closes,
            blocked,
            filed,
            project,
        } => send_delivery(DeliverySend {
            from: from_identity,
            to,
            branch,
            tip,
            base,
            remote,
            gate,
            gate_result,
            closes,
            blocked,
            filed,
            project: project.as_deref(),
        }),
        DeliveryCommand::Close {
            handle,
            for_identity,
            merge_commit,
            project,
        } => close_delivery(handle, for_identity, merge_commit, project.as_deref()),
    }
}

fn send_delivery(request: DeliverySend<'_>) -> Result<(), VivariumError> {
    let DeliverySend {
        from,
        to,
        branch,
        tip,
        base,
        remote,
        gate,
        gate_result,
        closes,
        blocked,
        filed,
        project,
    } = request;
    let mailspace = Mailspace::discover(project)?;
    validate_delivery_args(branch, tip, base, remote, gate, gate_result)?;
    let remote_branch = under_mind_prefix(branch);
    let blocked = parse_blocked(blocked)?;
    let push = push_branch(&mailspace.root, branch, &remote_branch, tip, remote);
    let delivery = DeliveryNeed {
        schema: 1,
        branch: remote_branch.clone(),
        local_branch: branch.into(),
        tip: tip.into(),
        base: base.into(),
        remote: remote.into(),
        push,
        gate: GateReceipt {
            command: gate.into(),
            result: gate_result.into(),
        },
        closes: closes.to_vec(),
        blocked,
        filed: filed.to_vec(),
    };
    file_delivery(&mailspace, from, to, delivery)
}

fn validate_delivery_args(
    branch: &str,
    tip: &str,
    base: &str,
    remote: &str,
    gate: &str,
    gate_result: &str,
) -> Result<(), VivariumError> {
    for (label, value) in [
        ("branch", branch),
        ("tip", tip),
        ("base", base),
        ("remote", remote),
        ("gate command", gate),
        ("gate result", gate_result),
    ] {
        if value.trim().is_empty() {
            return Err(VivariumError::Message(format!(
                "delivery {label} cannot be empty"
            )));
        }
    }
    if (branch.contains('\n') || branch.contains('\r')) || branch.trim() != branch {
        return Err(VivariumError::Message(
            "delivery branch must be a single trimmed git branch name".into(),
        ));
    }
    if !commit_sha(tip) || !commit_sha(base) {
        return Err(VivariumError::Message(
            "delivery tip and base must be commit SHA prefixes of 7 to 64 hex digits".into(),
        ));
    }
    Ok(())
}

fn commit_sha(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn under_mind_prefix(branch: &str) -> String {
    if branch.starts_with("mind/") {
        branch.to_string()
    } else {
        format!("mind/{branch}")
    }
}

fn parse_blocked(values: &[String]) -> Result<Vec<BlockedNeed>, VivariumError> {
    values
        .iter()
        .map(|value| {
            let Some((handle, reason)) = value.split_once('=') else {
                return Err(VivariumError::Message(format!(
                    "blocked delivery entry '{value}' must be HANDLE=REASON"
                )));
            };
            if handle.trim().is_empty() || reason.trim().is_empty() {
                return Err(VivariumError::Message(format!(
                    "blocked delivery entry '{value}' must have a handle and reason"
                )));
            }
            Ok(BlockedNeed {
                handle: handle.trim().into(),
                reason: reason.trim().into(),
            })
        })
        .collect()
}

fn push_branch(
    root: &std::path::Path,
    local_branch: &str,
    remote_branch: &str,
    declared_tip: &str,
    remote: &str,
) -> PushReceipt {
    let remote_ref = format!("refs/heads/{remote_branch}");
    for branch in [local_branch, remote_branch] {
        let valid = git(root, ["check-ref-format", "--branch", branch])
            .is_ok_and(|output| output.status.success());
        if !valid {
            return failed_push(remote_ref, "branch name is not a valid git branch");
        }
    }
    let local_ref = format!("refs/heads/{local_branch}");
    let tip = git(
        root,
        ["rev-parse", "--verify", &format!("{local_ref}^{{commit}}")],
    );
    let Ok(tip) = tip else {
        return failed_push(
            remote_ref,
            "local branch is unavailable in the mailspace checkout",
        );
    };
    if !tip.status.success() {
        return failed_push(
            remote_ref,
            "local branch is unavailable in the mailspace checkout",
        );
    }
    let actual_tip = String::from_utf8_lossy(&tip.stdout).trim().to_string();
    if !actual_tip.starts_with(declared_tip) {
        return failed_push(
            remote_ref,
            "local branch tip does not match the declared delivery tip",
        );
    }
    match git(
        root,
        ["push", "--", remote, &format!("{local_ref}:{remote_ref}")],
    ) {
        Ok(output) if output.status.success() => PushReceipt {
            status: "pushed".into(),
            remote_ref,
            detail: None,
        },
        Ok(output) => failed_push(
            remote_ref,
            &format!(
                "git push exited with status {}",
                output
                    .status
                    .code()
                    .map_or("unknown".into(), |code| code.to_string())
            ),
        ),
        Err(_) => failed_push(remote_ref, "git push could not be started"),
    }
}

fn git<const N: usize>(
    root: &std::path::Path,
    args: [&str; N],
) -> Result<std::process::Output, std::io::Error> {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
}

fn failed_push(remote_ref: String, detail: &str) -> PushReceipt {
    PushReceipt {
        status: "not_pushed".into(),
        remote_ref,
        detail: Some(detail.into()),
    }
}

fn file_delivery(
    mailspace: &Mailspace,
    from: &str,
    to: &str,
    delivery: DeliveryNeed,
) -> Result<(), VivariumError> {
    let body = serde_json::to_string_pretty(&delivery)
        .map_err(|error| VivariumError::Other(format!("failed to encode delivery: {error}")))?;
    let result = mailspace.send(SendRequest {
        from: from.into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: format!("Delivery: {}", delivery.branch),
        body,
        role: "needs".into(),
        kind: Some("need".into()),
        reply_to: None,
        depends_on: Vec::new(),
    })?;
    let handle = result
        .delivered
        .first()
        .map_or(result.sent.clone(), |message| message.handle.clone());
    mailspace.assign_need(from, &handle, to).map_err(|error| {
        VivariumError::Message(format!(
            "delivery need {handle} was sent, but assignment to '{to}' failed: {error}"
        ))
    })?;
    println!("delivery {handle}");
    println!("branch {} {}", delivery.branch, delivery.push.status);
    mailspace.announce_remote(&result.remote);
    Ok(())
}

fn close_delivery(
    handle: &str,
    for_identity: &str,
    merge_commit: &str,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    if !commit_sha(merge_commit) {
        return Err(VivariumError::Message(
            "merge commit must be a SHA prefix of 7 to 64 hex digits".into(),
        ));
    }
    let mailspace = Mailspace::discover(project)?;
    require_delivery(&mailspace, handle)?;
    let metadata = std::collections::BTreeMap::from([
        ("branch".into(), "main".into()),
        ("commit".into(), merge_commit.into()),
    ]);
    let moved = mailspace.move_item_outcome(
        for_identity,
        handle,
        "done",
        Some(&format!("merged as {merge_commit}")),
        "need done",
        Some(&metadata),
    )?;
    println!("delivery {} merged as {merge_commit}", moved.handle);
    Ok(())
}

fn require_delivery(mailspace: &Mailspace, handle: &str) -> Result<(), VivariumError> {
    let storage = mailspace.storage()?;
    let message_id = storage.resolve_message_token(handle)?;
    let bytes = storage.read_message(&message_id)?;
    let parsed = mail_parser::MessageParser::default()
        .parse(&bytes)
        .ok_or_else(|| VivariumError::Message("delivery item is not a valid letter".into()))?;
    let body = parsed
        .body_text(0)
        .ok_or_else(|| VivariumError::Message("delivery item has no structured body".into()))?;
    let delivery: DeliveryNeed = serde_json::from_str(&body).map_err(|_| {
        VivariumError::Message(format!("{handle} is not a structured delivery need"))
    })?;
    if delivery.schema != 1 {
        return Err(VivariumError::Message(format!(
            "unsupported delivery schema {}",
            delivery.schema
        )));
    }
    Ok(())
}
