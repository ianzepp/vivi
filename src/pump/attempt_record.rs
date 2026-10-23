//! Attempt records: the pump's account of one hand attempt, posted on the
//! need's open pump record when the attempt ends.
//!
//! Every end of a hand attempt (delivered, proof-failed, merge-conflict, agent
//! failed, timeout then split, no-defect, quarantined) leaves two things: the
//! attempt branch `vivi/unit/<uid>-a<n>`, pushed to the target remote and kept, and
//! one reply on the pump record's thread naming the unit, attempt, model,
//! pool, how it ended, the branch, the task, the commits and files on the
//! branch, the failing command with a short excerpt, and the hand's own final
//! message.
//!
//! The hand's final message comes from the task receipt (`vivi task show
//! <task> --latest`), so it works for remote pools whose hands cannot reach
//! the mailspace. Posting never blocks or fails the tick: one try per attempt
//! end, a failed send is logged and dropped.

use crate::pump::collect::{local_branch_exists, receipt_full, tail};
use crate::pump::dispatch::dispatch_base;
use crate::pump::host::{CmdOutput, Ctx, shell_quote};
use crate::pump::jobs::PoolJob;
use crate::pump::record::{reply_on, send_reply};
use crate::pump::state::State;

/// First line of an attempt-context reply on a slice task; the pool task
/// reader (`mailspace::pool_record`) picks replies up by it.
const ATTEMPT_MARKER: &str = "pump-attempt:";
/// Commits listed in a record.
const MAX_COMMITS: usize = 20;
/// Lines of `git diff --stat` listed in a record.
const MAX_STAT_LINES: usize = 40;
/// Characters of the hand's final message kept (the tail).
const FINAL_MESSAGE_CHARS: usize = 1500;
/// Characters of a failure excerpt kept (the tail).
const EXCERPT_CHARS: usize = 800;
/// The pool summary line that carries the agent's exit code.
const EXIT_MARKER: &str = "agent exit ";
/// The pool summary fragment that carries the dispatch time.
const STARTED_MARKER: &str = "started from ";
/// The receipt section titles the pool writes after its summary lines.
const SECTION_RECEIPT: &str = "receipt";
const SECTION_STDERR: &str = "stderr tail";
const SECTION_RUNNER_LOG: &str = "runner log tail";
/// Seconds allowed for every git call the record path makes: a bad network
/// must not stall a tick for minutes.
const RECORD_TIMEOUT: u64 = 30;
/// Characters of a push or listing failure kept (the tail).
const NOTE_TAIL_CHARS: usize = 200;

/// The failing command of an attempt and a short excerpt of its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failing {
    /// The command that failed, when there is one.
    pub command: Option<String>,
    /// The tail of what it printed.
    pub excerpt: String,
}

/// How one attempt ended, as far as the record is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ending {
    /// The ending in words (`delivered`, `proof-failed`, ...).
    label: String,
    /// The failing command and excerpt, for a failure.
    failing: Option<Failing>,
    /// The task's full receipt when the caller already read it.
    receipt: Option<String>,
}

impl Ending {
    /// The hand's branch reached the remote and the unit goes to the merge
    /// lanes.
    #[must_use]
    pub fn delivered(receipt: Option<&str>) -> Self {
        Self::of("delivered", None, receipt)
    }

    /// The agent ran out of its budget; the unit goes to the split planner.
    /// `text` is the receipt, or a note when the task is still running.
    #[must_use]
    pub fn split(text: &str) -> Self {
        Self::of("timeout -> split", None, Some(text))
    }

    /// The hand found nothing to fix.
    #[must_use]
    pub fn no_defect(receipt: &str) -> Self {
        Self::of("no-defect", None, Some(receipt))
    }

    /// The unit merged into a batch whose shared check failed; the batch was
    /// set aside on `vivi/quarantine/<id>` and `repair` opened.
    #[must_use]
    pub fn quarantined(id: &str, repair: &str) -> Self {
        let label = format!("quarantined (batch {id}, repair unit {repair})");
        Self::of(&label, None, None)
    }

    /// A failed attempt: `why` is the short reason (`proof-failed`,
    /// `merge-conflict`, `<state>/<detail>` of an agent that failed, ...) and
    /// `detail` the failure text.
    #[must_use]
    pub fn failed(why: &str, detail: &str, receipt: Option<&str>) -> Self {
        let merge_stage = matches!(
            why,
            "proof-failed" | "merge-conflict" | "branch-missing" | "timeout"
        );
        if merge_stage {
            return Self::of(why, Some(stage_failure(why, detail)), receipt);
        }
        let known = receipt.map_or_else(|| detail.to_owned(), str::to_owned);
        let parsed = parse_receipt(&known);
        let command = parsed.exit.map(|code| format!("agent exit {code}"));
        let failing = (command.is_some() || !parsed.stderr.is_empty()).then_some(Failing {
            command,
            excerpt: tail(&parsed.stderr, EXCERPT_CHARS),
        });
        Self::of(&format!("agent failed: {why}"), failing, receipt)
    }

    fn of(label: &str, failing: Option<Failing>, receipt: Option<&str>) -> Self {
        Self {
            label: label.to_owned(),
            failing,
            receipt: receipt.map(str::to_owned),
        }
    }
}

/// The failing command and excerpt of a merge-stage failure. A proof failure
/// carries "this command failed (exit N):\n<command>\n\n<heading>:\n<output>".
fn stage_failure(why: &str, detail: &str) -> Failing {
    if why == "proof-failed"
        && let Some((_, rest)) = detail.split_once("this command failed")
        && let Some((_, rest)) = rest.split_once('\n')
        && let Some((command, output)) = rest.split_once("\n\n")
    {
        let output = output.split_once(":\n").map_or(output, |(_, text)| text);
        return Failing {
            command: Some(command.trim().to_owned()),
            excerpt: tail(output.trim_end(), EXCERPT_CHARS),
        };
    }
    Failing {
        command: None,
        excerpt: tail(detail.trim_end(), EXCERPT_CHARS),
    }
}

/// The parts of a task receipt a record uses.
#[derive(Debug, Default, PartialEq, Eq)]
struct Parsed {
    /// The agent's exit code, from the pool's summary line.
    exit: Option<i32>,
    /// The hand's own final message.
    message: String,
    /// The stderr tail the pool collected.
    stderr: String,
}

/// The agent's exit code in a task receipt, when its summary line is there.
#[must_use]
pub fn agent_exit(receipt: &str) -> Option<i32> {
    let (_, rest) = receipt.split_once(EXIT_MARKER)?;
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    match digits.parse() {
        Ok(code) => Some(code),
        Err(_) => None,
    }
}

/// The agent's wall-clock seconds in a task receipt's summary line: the
/// number after `agent exit <n> in <m>s` or `agent exit <n> after <m>s`.
#[must_use]
pub fn agent_secs(text: &str) -> Option<u64> {
    let (_, rest) = text.split_once(EXIT_MARKER)?;
    let rest = rest
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
    let after = [" in ", " after "]
        .into_iter()
        .find_map(|m| rest.strip_prefix(m))?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse() {
        Ok(secs) => Some(secs),
        Err(_) => None,
    }
}

/// The dispatch time in a task receipt's summary line: the epoch seconds
/// after `started from `.
fn started_from(text: &str) -> Option<u64> {
    let (_, rest) = text.split_once(STARTED_MARKER)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse() {
        Ok(at) => Some(at),
        Err(_) => None,
    }
}

/// The joined text of the stderr and runner-log tail sections of a task
/// receipt: evidence scans that must not read the agent's own message
/// section read this instead of the whole receipt.
#[must_use]
pub(crate) fn sections_tail(text: &str) -> String {
    let mut tail = String::new();
    let mut sections = text.split("\n\n--- ");
    sections.next();
    for section in sections {
        let (title, body) = section.split_once('\n').unwrap_or((section, ""));
        if matches!(title.trim(), SECTION_STDERR | SECTION_RUNNER_LOG) {
            if !tail.is_empty() {
                tail.push('\n');
            }
            tail.push_str(body.trim());
        }
    }
    tail
}

/// The hand's final message from a full pool task receipt.
#[must_use]
pub fn hand_message(text: &str) -> String {
    parse_receipt(text).message
}

/// Split a task receipt: the pool's summary lines come first, then sections
/// each opened by a `--- <title>` line after a blank line. A receipt with no
/// `--- receipt` section is taken as the hand's message whole.
fn parse_receipt(text: &str) -> Parsed {
    let mut parsed = Parsed {
        exit: agent_exit(text),
        ..Parsed::default()
    };
    let mut sections = text.split("\n\n--- ");
    let head = sections.next().unwrap_or_default();
    let mut found = false;
    for section in sections {
        let (title, body) = section.split_once('\n').unwrap_or((section, ""));
        match title.trim() {
            SECTION_RECEIPT => {
                parsed.message = body.trim().to_owned();
                found = true;
            }
            SECTION_STDERR => parsed.stderr = body.trim().to_owned(),
            _ => {}
        }
    }
    if !found {
        parsed.message = head.trim().to_owned();
    }
    parsed
}

/// What the attempt branch looks like from the integration checkout.
#[derive(Debug, PartialEq, Eq)]
enum Branch {
    /// The hand left no branch: the remote answers and lists none, and the
    /// checkout has no local branch.
    Absent,
    /// The branch exists but the checkout could not read it; the tail of
    /// what the listing or fetch said.
    Unreadable(String),
    /// Commits (`git log --oneline`) and files (`git diff --stat`) on it,
    /// plus how the branch is published (`kept locally; pool runner
    /// publishes` or `pushed to the target remote, kept`).
    Found {
        commits: Vec<String>,
        files: Vec<String>,
        note: String,
    },
}

/// Everything a record says.
struct Facts {
    need: String,
    task: Option<String>,
    actor: String,
    role: String,
    kind: Option<String>,
    slot: Option<String>,
    ready_at: Option<u64>,
    start: Option<u64>,
    end: u64,
    agent_secs: Option<u64>,
    delivered_at: Option<u64>,
    uid: String,
    attempt: u32,
    model: String,
    pool: String,
    target: String,
    branch: Option<String>,
    base: String,
    shape: Branch,
    label: String,
    failing: Option<Failing>,
    message: String,
}

/// Post the record of the attempt that just ended on unit `uid`: make sure
/// its branch is on the remote, read the branch and the task receipt, and
/// reply on the need's open pump record (not the need thread). Nothing here
/// fails the caller: every error is logged and dropped (a need with no open
/// record gets no reply), and a unit whose "need" is no mailspace thread is
/// skipped: a repair unit, and a unit of a pump-synthesized fix need
/// (`<fix_prefix>-g<tip8>`, as gate fixes and their split children carry).
/// The pump never replies to a handle that is not a real thread: real
/// mailspace handles are 8 hex characters and never start with the fix
/// prefix.
pub fn post(ctx: &Ctx, st: &mut State, uid: &str, ending: &Ending) {
    let Some(unit) = st.units.get(uid) else {
        return;
    };
    if unit.repairs.is_some() || unit.need.starts_with(ctx.cfg.fix_prefix.as_str()) {
        return;
    }
    let (need, attempt) = (unit.need.clone(), unit.attempts);
    let tctx = ctx
        .for_target(unit.target_id())
        .unwrap_or_else(|| ctx.clone());
    let base = dispatch_base(&tctx, st, uid);
    let facts = gather(ctx, &tctx, st, uid, &base, ending);
    let stage = format!("attempt-{uid}-a{attempt}-{}", slug(&ending.label));
    let sent = send_reply(ctx, &need, &stage, &compose(&facts));
    if let Some(unit) = st.units.get_mut(uid) {
        unit.ready_at = None;
    }
    if sent {
        st.board.sync_pending = true;
    } else {
        ctx.log(&format!("attempt record send failed {uid}"));
    }
}

/// Post the record of one attempt as a `pump-attempt:` reply on the slice
/// task itself, from the pump identity, so the next attempt's brief carries
/// it. `facts_text` is the task receipt; the ending's own receipt wins when
/// it has one. Returns whether the reply went out; a failure is logged and
/// never blocks the caller.
pub fn post_on_task(
    ctx: &Ctx,
    task: &str,
    job: &PoolJob,
    ending: &Ending,
    facts_text: &str,
) -> bool {
    let receipt = ending.receipt.as_deref().unwrap_or(facts_text);
    let facts = Facts {
        need: task.to_owned(),
        task: Some(task.to_owned()),
        actor: ctx.actor.clone(),
        role: ctx.cfg.to.clone(),
        kind: None,
        slot: None,
        ready_at: None,
        start: started_from(receipt),
        end: ctx.now(),
        agent_secs: agent_secs(receipt),
        delivered_at: None,
        uid: task.to_owned(),
        attempt: job
            .payload
            .get("attempt")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        model: job.model.clone().unwrap_or_else(|| "None".to_owned()),
        pool: job.pool.clone().unwrap_or_else(|| "?".to_owned()),
        target: job.target.clone().unwrap_or_default(),
        branch: job.branch.clone(),
        base: String::new(),
        shape: Branch::Absent,
        label: ending.label.clone(),
        failing: ending.failing.clone(),
        message: tail(&parse_receipt(receipt).message, FINAL_MESSAGE_CHARS),
    };
    let body = format!(
        "{}\n{}",
        crate::mailspace::marked_line(ATTEMPT_MARKER, &ending.label),
        compose(&facts)
    );
    let sent = reply_on(
        ctx,
        task,
        task,
        &format!("attempt-{}", slug(&ending.label)),
        &body,
    );
    if !sent {
        ctx.log(&format!("attempt record send failed on task {task}"));
    }
    sent
}

/// Post a lifecycle event that enriches the synced attempt record.
pub(crate) fn post_status(ctx: &Ctx, st: &mut State, uid: &str, status: &str, at: u64) {
    let Some(unit) = st.units.get(uid) else {
        return;
    };
    if unit.repairs.is_some() || unit.need.starts_with(ctx.cfg.fix_prefix.as_str()) {
        return;
    }
    let need = unit.need.clone();
    let attempt = unit.attempts;
    let attempt_id = format!("{uid}-a{attempt}");
    let body = format!(
        "pump: attempt status event\n\
         attempt: {attempt_id}\n\
         need: {}\n\
         status: {status}\n\
         at: {at}",
        or_none(Some(&need))
    );
    let stage = format!("attempt-{uid}-a{attempt}-{status}");
    if send_reply(ctx, &need, &stage, &body) {
        st.board.sync_pending = true;
    } else {
        ctx.log(&format!("attempt status event send failed {uid} {status}"));
    }
}

/// Mark the first tick when a pending unit enters the dispatch-ready frontier.
pub(crate) fn mark_ready(st: &mut State, uid: &str, at: u64) {
    if let Some(unit) = st.units.get_mut(uid) {
        unit.ready_at.get_or_insert(at);
    }
}

/// Clear a readiness interval when a unit returns to a dependency hold.
pub(crate) fn clear_ready(st: &mut State, uid: &str) {
    if let Some(unit) = st.units.get_mut(uid) {
        unit.ready_at = None;
    }
}

/// Read the unit, its branch and its receipt into [`Facts`].
fn gather(ctx: &Ctx, tctx: &Ctx, st: &State, uid: &str, base: &str, ending: &Ending) -> Facts {
    let unit = st.units.get(uid).cloned().unwrap_or_default();
    let shape = unit
        .branch
        .as_deref()
        .map_or(Branch::Absent, |branch| read_branch(tctx, branch, base));
    let receipt = ending.receipt.clone().or_else(|| {
        let task = unit.task.as_deref()?;
        Some(receipt_full(ctx, task))
    });
    let secs = receipt.as_deref().and_then(agent_secs);
    let start = unit
        .since_dispatch
        .or_else(|| receipt.as_deref().and_then(started_from));
    Facts {
        need: unit.need.clone(),
        task: unit.task.clone(),
        actor: ctx.actor.clone(),
        role: ctx.cfg.to.clone(),
        kind: nonempty(ctx.cfg.kind_name(&unit.tools)),
        slot: unit.slot.clone(),
        ready_at: unit.ready_at,
        start,
        end: ctx.now(),
        agent_secs: secs,
        delivered_at: (ending.label == "delivered").then_some(unit.since),
        uid: uid.to_owned(),
        attempt: unit.attempts,
        model: unit.model.clone().unwrap_or_else(|| "None".to_owned()),
        pool: unit.pool.clone().unwrap_or_else(|| "?".to_owned()),
        target: unit.target_id().to_owned(),
        branch: unit.branch.clone(),
        base: base.to_owned(),
        shape,
        label: ending.label.clone(),
        failing: ending.failing.clone(),
        message: tail(
            &receipt
                .map(|text| parse_receipt(&text).message)
                .unwrap_or_default(),
            FINAL_MESSAGE_CHARS,
        ),
    }
}

/// Read the branch's commits and files against `base`. A local branch is
/// read straight from `refs/heads` (the pool runner publishes it); otherwise
/// the remote listing decides, and a listed branch is fetched into the
/// pump-owned `refs/pump-records/<branch>` without writing `FETCH_HEAD` or
/// any `refs/remotes/*` — the merge lanes share this checkout and those
/// refs.
fn read_branch(tctx: &Ctx, branch: &str, base: &str) -> Branch {
    let (reference, note) = if local_branch_exists(tctx, branch) {
        publish_local(tctx, branch);
        (
            format!("refs/heads/{branch}"),
            String::from("kept locally; pool runner publishes"),
        )
    } else {
        match remote_branch(tctx, branch) {
            Err(why) => return Branch::Unreadable(why),
            Ok(false) => return Branch::Absent,
            Ok(true) => {
                let out = record_fetch(tctx, branch);
                if out.code != 0 {
                    return Branch::Unreadable(tail(out.text.trim(), NOTE_TAIL_CHARS));
                }
                (
                    format!("refs/pump-records/{branch}"),
                    String::from("pushed to the target remote, kept"),
                )
            }
        }
    };
    let reference = shell_quote(&reference);
    let base = shell_quote(base);
    let commits = tctx.git(
        &format!("log --oneline {base}..{reference}"),
        RECORD_TIMEOUT,
    );
    let files = tctx.git(&format!("diff --stat {base}...{reference}"), RECORD_TIMEOUT);
    Branch::Found {
        commits: cap_commits(&lines(&commits.text)),
        files: cap_stat(&lines(&files.text)),
        note,
    }
}

/// Fetch a remote-only branch into the pump-owned record namespace: no
/// `FETCH_HEAD` write, no `refs/remotes/*` update.
fn record_fetch(tctx: &Ctx, branch: &str) -> CmdOutput {
    let b = shell_quote(branch);
    tctx.git(
        &format!(
            "fetch --no-write-fetch-head {} refs/heads/{b}:refs/pump-records/{b}",
            tctx.cfg.remote
        ),
        RECORD_TIMEOUT,
    )
}

/// The remote's answer for the branch: `Ok(true)` when listed, `Ok(false)`
/// when the remote answers but lists none, `Err(tail)` when the listing
/// itself failed (a transient network error is not "no changes").
fn remote_branch(tctx: &Ctx, branch: &str) -> Result<bool, String> {
    let out = tctx.git(
        &format!(
            "ls-remote {} refs/heads/{}",
            tctx.cfg.remote,
            shell_quote(branch)
        ),
        RECORD_TIMEOUT,
    );
    if out.code != 0 {
        return Err(tail(out.text.trim(), NOTE_TAIL_CHARS));
    }
    Ok(!out.text.trim().is_empty())
}

/// A local worktree pool already committed the attempt branch in the target
/// checkout. The privileged pool runner publishes that ref; the pump only
/// reads it for the attempt record.
fn publish_local(tctx: &Ctx, branch: &str) {
    if tctx.cfg.targets.is_empty() {
        return;
    }
    tctx.log(&format!(
        "attempt branch {branch} kept locally; pool runner publishes"
    ));
}

/// The non-empty lines of `text`.
fn lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// At most [`MAX_COMMITS`] commit lines, newest first, with a count of the rest.
fn cap_commits(commits: &[String]) -> Vec<String> {
    if commits.len() <= MAX_COMMITS {
        return commits.to_vec();
    }
    let mut kept = commits[..MAX_COMMITS].to_vec();
    kept.push(format!("... and {} more", commits.len() - MAX_COMMITS));
    kept
}

/// At most [`MAX_STAT_LINES`] lines of `diff --stat`: the first ones, then
/// the summary line that closes the stat.
fn cap_stat(files: &[String]) -> Vec<String> {
    if files.len() <= MAX_STAT_LINES {
        return files.to_vec();
    }
    let mut kept = files[..MAX_STAT_LINES - 2].to_vec();
    kept.push(format!(
        "... {} more files",
        files.len() - MAX_STAT_LINES + 1
    ));
    kept.push(files[files.len() - 1].clone());
    kept
}

/// A file-name-safe form of `label`.
fn slug(label: &str) -> String {
    let word: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    word.trim_matches('-').chars().take(24).collect()
}

/// The text of the record.
fn compose(facts: &Facts) -> String {
    let mut out = String::from("pump: attempt record\n");
    out.push_str(&header_block(facts));
    out.push_str(&format!(
        "unit: {}  attempt: {}  model: {}  pool: {}  target: {}\n",
        facts.uid, facts.attempt, facts.model, facts.pool, facts.target
    ));
    out.push_str(&format!(
        "branch: {}\n",
        branch_line(facts.branch.as_deref(), &facts.shape)
    ));
    out.push_str(&format!(
        "task: {}\nbase: {}\n",
        facts.task.as_deref().unwrap_or("none"),
        facts.base
    ));
    if let Branch::Found { commits, files, .. } = &facts.shape {
        out.push_str(&block("commits", commits));
        out.push_str(&block("files changed", files));
    }
    if let Some(failing) = &facts.failing {
        out.push_str(&failing_block(failing));
    }
    out.push_str("hand's final message:\n");
    out.push_str(&indent(&facts.message, "(none)"));
    out
}

/// The machine-readable `key: value` block that opens every record: one
/// line per fact, `none` for absent values, decimal epochs.
fn header_block(facts: &Facts) -> String {
    format!(
        "attempt: {}-a{}\n\
         need: {}\n\
         task: {}\n\
         actor: {}\n\
         role: {}\n\
         kind: {}\n\
         pool: {}\n\
         slot: {}\n\
         model: {}\n\
         ready-at: {}\n\
         start: {}\n\
         end: {}\n\
         agent-seconds: {}\n\
         delivered-at: {}\n\
         merged-at: none\n\
         outcome: {}\n",
        facts.uid,
        facts.attempt,
        or_none(Some(facts.need.as_str())),
        or_none(facts.task.as_deref()),
        or_none(Some(facts.actor.as_str())),
        or_none(Some(facts.role.as_str())),
        or_none(facts.kind.as_deref()),
        or_none(Some(facts.pool.as_str())),
        or_none(facts.slot.as_deref()),
        or_none(Some(facts.model.as_str())),
        num(facts.ready_at),
        num(facts.start),
        facts.end,
        num(facts.agent_secs),
        num(facts.delivered_at),
        &facts.label,
    )
}

/// A header value: `none` when it is absent or blank.
fn or_none(value: Option<&str>) -> &str {
    match value {
        Some(text) if !text.trim().is_empty() => text,
        _ => "none",
    }
}

/// An empty value is absent in the machine-readable attempt block.
fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_owned())
}

/// A header number: decimal, or `none` when it is absent.
fn num(value: Option<u64>) -> String {
    value.map_or_else(|| String::from("none"), |n| n.to_string())
}

/// The `branch:` line.
fn branch_line(branch: Option<&str>, shape: &Branch) -> String {
    let name = branch.unwrap_or("none");
    match shape {
        Branch::Found { note, .. } => format!("{name} ({note})"),
        Branch::Absent => format!("{name} (none: the hand left no changes)"),
        Branch::Unreadable(why) => format!("{name} (branch not readable: {why})"),
    }
}

/// `<title>:` and the indented lines, or `(none)`.
fn block(title: &str, rows: &[String]) -> String {
    format!("{title}:\n{}", indent(&rows.join("\n"), "(none)"))
}

/// The failing command and its excerpt.
fn failing_block(failing: &Failing) -> String {
    let mut out = String::from("failing command:\n");
    out.push_str(&indent(failing.command.as_deref().unwrap_or(""), "(none)"));
    if !failing.excerpt.is_empty() {
        out.push_str("excerpt:\n");
        out.push_str(&indent(&failing.excerpt, ""));
    }
    out
}

/// `text` indented two spaces per line, `empty` when it has no text; ends
/// with a newline.
fn indent(text: &str, empty: &str) -> String {
    if text.trim().is_empty() {
        return if empty.is_empty() {
            String::new()
        } else {
            format!("  {empty}\n")
        };
    }
    text.lines().map(|line| format!("  {line}\n")).collect()
}
