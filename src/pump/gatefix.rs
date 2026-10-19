//! Gate-fix planning: the fix need id, the planner prompt, and the re-plan of
//! residual failures (owned names still failing after their owner settled).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::pump::collect::spawn_planner;
use crate::pump::gate::is_settled;
use crate::pump::host::{Ctx, exit_receipt_path, shell_quote};
use crate::pump::state::{State, Status};

/// The `state.json` key holding how many times each failing name has been
/// handed to a fresh gate-fix planner after its owner settled.
const REPLANS_KEY: &str = "replans";

/// How many times `name` has been re-planned.
fn replan_count(st: &State, name: &str) -> u64 {
    st.extra
        .get(REPLANS_KEY)
        .and_then(|counts| counts.get(name))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// `true` when `name` may be handed to a fresh planner again: a baseline
/// exists and `[pump.gate] max_replans` is not used up for the name.
pub(crate) fn replans_left(ctx: &Ctx, st: &State, name: &str) -> bool {
    st.baseline.is_some() && replan_count(st, name) < u64::from(ctx.cfg.gate.max_replans)
}

/// `true` when the owner has units and every one of them is settled.
fn owner_settled(st: &State, owner: &str) -> bool {
    let statuses: Vec<Status> = st
        .units
        .values()
        .filter(|unit| unit.need == owner)
        .map(|unit| unit.status)
        .collect();
    !statuses.is_empty() && statuses.iter().all(|&s| is_settled(s))
}

/// Owned names that still fail while every unit of their owner has settled
/// and re-plans are left for them, as `(name, owner)` sorted by name.
pub(crate) fn residual_failures(ctx: &Ctx, st: &State, names: &[String]) -> Vec<(String, String)> {
    let failing: BTreeSet<&String> = names.iter().collect();
    st.red
        .iter()
        .filter(|(name, owner)| {
            failing.contains(name) && replans_left(ctx, st, name) && owner_settled(st, owner)
        })
        .map(|(name, owner)| (name.clone(), owner.clone()))
        .collect()
}

/// Hand the residual names to `need`: count the re-plan, own the name by the
/// new need (so promotion stays held) and log one line per old owner.
pub(crate) fn take_residuals(ctx: &Ctx, st: &mut State, need: &str, residual: &[(String, String)]) {
    let mut by_owner: BTreeMap<&str, usize> = BTreeMap::new();
    let mut counts = match st.extra.get(REPLANS_KEY) {
        Some(serde_json::Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    };
    for (name, owner) in residual {
        st.red.insert(name.clone(), need.to_owned());
        let next = replan_count(st, name) + 1;
        counts.insert(name.clone(), serde_json::json!(next));
        *by_owner.entry(owner.as_str()).or_insert(0) += 1;
    }
    if !residual.is_empty() {
        st.extra
            .insert(String::from(REPLANS_KEY), serde_json::Value::Object(counts));
    }
    for (owner, count) in by_owner {
        ctx.log(&format!(
            "replan: {count} residual failures of {owner} \u{2192} {need}"
        ));
    }
}

/// The gate-fix need id for a tip: `<fix_prefix>-g<tip8>`.
pub(crate) fn fix_need(ctx: &Ctx, tip: &str) -> String {
    let short: String = tip.chars().take(8).collect();
    format!("{}-g{short}", ctx.cfg.fix_prefix)
}

/// The need id a gate-fix plan goes under: `<fix_prefix>-g<tip8>`. With
/// residual names the base id may already be taken by an earlier plan or
/// planner, so the first free `<base>-r<k>` is used.
pub(crate) fn plan_need(ctx: &Ctx, tip: &str, residual: bool) -> String {
    let base = fix_need(ctx, tip);
    if !residual || !need_taken(ctx, &base) {
        return base;
    }
    for k in 1..=u32::MAX {
        let need = format!("{base}-r{k}");
        if !need_taken(ctx, &need) {
            return need;
        }
    }
    base
}

/// `true` when a plan file or a planner prompt already exists for `need`.
fn need_taken(ctx: &Ctx, need: &str) -> bool {
    ctx.home.join("plans").join(format!("{need}.json")).exists()
        || ctx
            .home
            .join("gatefix")
            .join(format!("{need}.prompt"))
            .exists()
}

/// Send the real backing need for a gate fix and return its handle: the
/// mailspace thread fix units' attempt records reply on. Returns `None`
/// (with the reason logged) when the send fails or no handle can be parsed.
pub(crate) fn send_fix_need(
    ctx: &Ctx,
    tip: &str,
    new: &[String],
    residual: &[(String, String)],
) -> Option<String> {
    let dir = ctx.home.join("gatefix");
    drop(fs::create_dir_all(&dir));
    let path = dir.join(format!("{}-need.txt", fix_need(ctx, tip)));
    let mut body = format!("The integration branch went red at {tip}.\nNew failures:\n");
    for name in new {
        body.push_str(&format!("- {name}\n"));
    }
    body.push_str("Residual failures still failing after their owner settled:\n");
    for (name, _) in residual {
        body.push_str(&format!("- {name}\n"));
    }
    body.push_str(
        "The pump starts a fix planner for these; attempt records for its units reply on this thread.\n",
    );
    drop(fs::write(&path, body));
    let short: String = tip.chars().take(8).collect();
    let subject = format!(
        "pump: gate fix for g{short} ({} new, {} residual)",
        new.len(),
        residual.len()
    );
    let args = format!(
        "need send --from {} --to {} --subject {} --body-file {}",
        ctx.cfg.reply_from,
        ctx.cfg.reply_to(),
        shell_quote(&subject),
        shell_quote(&path.display().to_string()),
    );
    let out = ctx.vivi(&args, 60);
    let handle = if out.code == 0 {
        parse_need_handle(&out.text)
    } else {
        None
    };
    match handle {
        Some(handle) => Some(handle),
        None => {
            ctx.log(&format!(
                "gatefix need send failed: {}",
                last_line(&out.text)
            ));
            None
        }
    }
}

/// The need handle from `need send` output: the third whitespace token of
/// the first `created ` line, else the second token of the first `sent ` line.
fn parse_need_handle(text: &str) -> Option<String> {
    let created = text
        .lines()
        .find(|line| line.starts_with("created "))
        .and_then(|line| line.split_whitespace().nth(2));
    let token = created.or_else(|| {
        text.lines()
            .find(|line| line.starts_with("sent "))
            .and_then(|line| line.split_whitespace().nth(1))
    });
    token
        .map(str::to_owned)
        .filter(|handle| !handle.is_empty() && !handle.contains(char::is_whitespace))
}

/// The last non-blank line of `text`, cut to 80 chars.
fn last_line(text: &str) -> String {
    let line = text.trim().lines().last().unwrap_or("");
    line.chars().take(80).collect()
}

/// Start the gate-fix planner for a red gate under `need`, skipped when its
/// plan file already exists.
pub(crate) fn gatefix(
    ctx: &Ctx,
    tip: &str,
    clean: &str,
    need: &str,
    new: &[String],
    residual: &[(String, String)],
) {
    if ctx.home.join("plans").join(format!("{need}.json")).exists() {
        return;
    }
    let prompt = gatefix_prompt(ctx, tip, clean, need, new, residual);
    spawn_planner(ctx, "gatefix", need, &prompt);
    let more = if residual.is_empty() {
        String::new()
    } else {
        format!(", {} residual", residual.len())
    };
    ctx.log(&format!(
        "gatefix planner started for {need} ({} new{more})",
        new.len()
    ));
}

/// The `state.json` key holding how many times each gate-fix planner has
/// exited without writing its plan.
const FAILS_KEY: &str = "gatefix_fails";

/// The `state.json` key holding when each gate-fix planner is next retried.
const RETRY_KEY: &str = "gatefix_retry_at";

/// How many planless gate-fix planner exits are allowed before the owned
/// names are reported and left alone.
const MAX_GATEFIX_FAILS: u64 = 3;

/// Expire the gate-fix planner state of every owner need in `st.red`: clear
/// the bookkeeping once a plan landed, retry a planner that exited without
/// writing one, and raise `gatefix-no-plan` when its retries are used up.
pub(crate) fn expire_gatefix(ctx: &Ctx, st: &mut State) {
    let needs: BTreeSet<String> = st.red.values().cloned().collect();
    for need in needs {
        expire_need(ctx, st, &need);
    }
}

/// The gate-fix expiry for one owner need: (a) a plan clears the
/// bookkeeping; (b) an exit receipt counts a fail and arms the backoff retry
/// or raises the exception at the cap; (c) a due retry respawns the planner
/// from its prompt.
fn expire_need(ctx: &Ctx, st: &mut State, need: &str) {
    if ctx.home.join("plans").join(format!("{need}.json")).exists() {
        extra_remove(st, FAILS_KEY, need);
        extra_remove(st, RETRY_KEY, need);
        return;
    }
    let out = ctx.home.join("gatefix").join(format!("{need}.out"));
    let receipt = exit_receipt_path(&out);
    if receipt.exists() {
        let code = fs::read_to_string(&receipt).unwrap_or_default();
        let code = code.trim();
        drop(fs::remove_file(&receipt));
        let fails = extra_u64(st, FAILS_KEY, need).unwrap_or(0) + 1;
        extra_set(st, FAILS_KEY, need, fails);
        if fails >= MAX_GATEFIX_FAILS {
            ctx.exception(
                "gatefix-no-plan",
                need,
                "the gate-fix planner exited 3 times without writing a plan; its names stay owned red",
            );
            extra_remove(st, RETRY_KEY, need);
        } else {
            let delay = (60u64 << (fails - 1)).min(900);
            extra_set(st, RETRY_KEY, need, ctx.now() + delay);
            ctx.log(&format!(
                "gatefix planner for {need} exited rc={code} without a plan; retry {fails}/3 in {delay}s"
            ));
        }
        return;
    }
    if extra_u64(st, RETRY_KEY, need).is_some_and(|at| at <= ctx.now()) {
        let prompt = fs::read_to_string(ctx.home.join("gatefix").join(format!("{need}.prompt")))
            .unwrap_or_default();
        drop(spawn_planner(ctx, "gatefix", need, &prompt));
        extra_remove(st, RETRY_KEY, need);
        ctx.log(&format!("gatefix retry {need}"));
    }
}

/// The u64 value of `key -> field` in the extra state maps, else `None`.
fn extra_u64(st: &State, key: &str, field: &str) -> Option<u64> {
    st.extra
        .get(key)
        .and_then(|map| map.get(field))
        .and_then(serde_json::Value::as_u64)
}

/// Set `key -> field` to `value` in the extra state maps.
fn extra_set(st: &mut State, key: &str, field: &str, value: u64) {
    let entry = st
        .extra
        .entry(key.to_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(object) = entry.as_object_mut() {
        object.insert(field.to_owned(), serde_json::json!(value));
    }
}

/// Remove `key -> field` from the extra state maps, dropping an emptied map.
fn extra_remove(st: &mut State, key: &str, field: &str) {
    let emptied = if let Some(map) = st.extra.get_mut(key)
        && let Some(object) = map.as_object_mut()
    {
        object.remove(field);
        object.is_empty()
    } else {
        return;
    };
    if emptied {
        st.extra.remove(key);
    }
}

/// The gate-fix planner prompt, with the repo, home and plan
/// paths taken from the context.
fn gatefix_prompt(
    ctx: &Ctx,
    tip: &str,
    clean: &str,
    need: &str,
    new: &[String],
    residual: &[(String, String)],
) -> String {
    let repo = ctx.repo.display();
    let home = ctx.home.display();
    let remote = &ctx.cfg.remote;
    let new_section = if new.is_empty() {
        String::new()
    } else {
        let names = new
            .iter()
            .map(|name| format!("- {name}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "A full test run at integration-branch commit {tip} shows these failing tests that were NOT failing at the last clean run (commit {clean}):
{names}

The commits merged between the two are `git log --oneline {clean}..{tip}` in {repo} (run `git -C {repo} fetch -q {remote}` first; each merge subject names its unit and need). Read the failing tests and the merged diffs and find, for each failure, the merge that most likely caused it.

"
        )
    };
    let sections = format!("{new_section}{}", residual_sections(ctx, tip, residual));
    let fix = fix_need(ctx, tip);
    format!(
        "The integration branch went red. Plan the fix units, as your system prompt describes, with these rules.

{sections}Write {home}/plans/{need}.json (write .tmp, then mv): {{\"need\": \"{need}\", \"verdict\": \"units\", \"reason\": \"...\", \"units\": [...]}}. Unit ids \"{fix}-1\", \"{fix}-2\", ... . Group failures with one likely cause into one unit; one cause per unit. Each brief names the failing tests, the suspected merge commit and why, and says: run the tests first; fix the CAUSE forward; change a test only if the suspected unit implemented a ruling that deliberately changed the pinned behaviour, and name the ruling; never delete, ignore or weaken a test; name the culprit commit in the reply. Proof: the failing tests by name (one `cargo test` filter per command). Keep write scopes as narrow as you can.
Your final message is one line.
"
    )
}

/// One prompt section per earlier owner of residual names.
fn residual_sections(ctx: &Ctx, tip: &str, residual: &[(String, String)]) -> String {
    let mut by_owner: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, owner) in residual {
        by_owner.entry(owner).or_default().push(name);
    }
    by_owner
        .into_iter()
        .map(|(owner, names)| residual_section(ctx, tip, owner, &names))
        .collect()
}

/// The prompt section for the residual `names` earlier owned by `owner`.
fn residual_section(ctx: &Ctx, tip: &str, owner: &str, names: &[&str]) -> String {
    let repo = ctx.repo.display();
    let remote = &ctx.cfg.remote;
    let list = names
        .iter()
        .map(|name| format!("- {name}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "These tests were owned by fix plan {owner}. Every unit of that plan has settled, and they still fail at commit {tip}, so the earlier fix was incomplete:
{list}

Read what that plan's units changed (`git -C {repo} fetch -q {remote}`, then `git -C {repo} log --grep {owner}` in the repo), fix the cause forward, and group the failures by cause.

"
    )
}
