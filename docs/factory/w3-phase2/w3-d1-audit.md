# W3-D1 audit — vivi library surface (wave 3 phase 2)

- **Auditor**: independent, read-only; did not implement any of W3-D1.
- **Need branch**: `need/w3-d1` at `.claude/worktrees/need-w3-d1`, tip `fcd0c900`, base `470225f4` (v21.0.0).
- **Commits audited**: `f656044a` (config show/validate/migrate), `c2340f20` (pool list/show views), `265c8d10` (flow view), `91972705` (pump actions value entry points incl. unlock), `40ffa7a1` (logview view), plus their merge commits.
- **Authority**: `docs/factory/wave3-completion-goal.md` §3.3 (target + done_when), §6.2 W3-D1 probe row, §5.6 verdict scale.
- **Scope of this audit**: the diff `470225f4..fcd0c900` only; targeted tests run in the need worktree; no file in any repo edited.

## 1. What landed (verified against the diff)

33 files, all under `src/` (+1976/−449). `git diff 470225f4..fcd0c900 -- tests/` is **empty** — no test file was touched at all, which is stronger than the probe's "no expected-output edit".

| §3.3 table row | Library entry point (plain args, returns values) | Unit tests |
| --- | --- | --- |
| config show | `vivi::pump::config::show(project, resolved) -> ShowReport` (`src/pump/config/show.rs:34-41`) | `src/pump/config/show_test.rs` (3) |
| config validate + issues_json | `vivi::pump::config::validate(project) -> ValidateReport` (`src/pump/config/check.rs:33-41`), `issues_json(&[Issue])` (`check.rs:44-62`) | `src/pump/config/check_test.rs` (5) |
| config migrate | `vivi::pump::config::migrate(project, write) -> MigrateReport` (`src/pump/config/migrate.rs:66-83`) | `src/pump/config/migrate_test.rs` (3) |
| pool list / show | `vivi::pool::config::{list_view, show_view}` re-exported from `src/pool/config_view.rs:62-88` (`PoolRow`, `BandRow`, `PoolShow`) | `src/pool/config_view_test.rs` (5) |
| flow show | `vivi::pump::flow::view(project, kind, need) -> FlowShow` (`src/pump/flow.rs:81-93`) | `src/pump/flow_test.rs` (3) |
| pump actions | `requeue(project, &RequeueTarget, dry_run)` (`src/pump/requeue.rs:81-90`), `withdraw(project, needs, reason)` (`src/pump/withdraw.rs:73-93`), `replan(project, need)` (`src/pump/replan.rs:41-65`), `flow::answer::answer(project, step, word, note)` (`src/pump/flow/answer.rs:101-115`), `unlock(project, need, entry, note)` (`src/pump/unlock.rs:48-62`) | extended/new companions (see §4) |
| pump log tail | `vivi::pump::logview::view(project, file, lines, filter) -> Vec<String>` (`src/pump/logview.rs:227-240`), count applied after filtering | `src/pump/logview_test.rs` (+3) |

CLIs became printers: `src/local_config_command.rs`, `src/local_pool_config_command.rs`, `src/local_flow_command.rs`, `src/local_pump_command.rs` now call the library functions and print the returned values. `post_unlock` is gone from bin glue; its single implementation is `pump::unlock::unlock` (base `src/local_pump_command.rs:20-33` moved verbatim; print line preserved via `UnlockResult::Display`).

## 2. Correctness of the lifts (compared against 470225f4)

I read every removed block in the bin glue and compared it to the new library body.

- **config show**: same discovery, same `resolved` free-slot probe (`pool status --porcelain` of `current_exe`, `count_free` line format identical). The base's `live_free(...).transpose()?` had no real `Err` branch (the base body returned `Ok` on probe failure); dropping it is value-preserving. JSON porcelain/origins insertion identical (`src/local_config_command.rs:78-108`).
- **config validate**: issue construction is byte-identical — `Issue::not_ready(missing.name, missing.detail)`, `Issue::error("[table] key: reason; fix: …")` with the same message text, `Issue::warning` for origin warnings (`src/pump/config/check.rs:65-93` vs base `local_config_command.rs:166-272`). `issues_json` field order and `item` omission identical. Exit logic (`require_ok`, exit 3 on not-ready) stays in the CLI, unchanged.
- **config migrate**: same order — plan → diff → (dry run returns) → timestamped config `.bak` → `config_edit::write_text` → charters (each: `.md.bak.<stamp>` copy then `set_charter`). `write_charters` still re-discovers the mailspace *after* the config write, as the base did. `legacy_planner_charter`'s `~/` expansion preserved. The write path uses `config_edit::write_text` under the config lock exactly as base `470225f4:src/local_config_command.rs:289` did — the guarded-write semantics of migrate (backup first, atomic text write under the config lock, charter backups) are intact. Print order in `print_migrate` matches the base's incremental print order for the success path (`src/local_config_command.rs:187-213`).
- **pool list/show**: `row`, `live_free`, and the unknown-pool error text moved verbatim (`src/pool/config_view.rs:90-137` vs base `local_pool_config_command.rs:19-93,251-279`). `PoolRow`/`BandRow` keep the same field set and order, so `--json` output is unchanged (pinned by `a_row_serializes_with_the_cli_json_fields_in_order`).
- **flow show**: `registry::shipped` refusal text, the outer/inner `Option` shape of runs, and `run_index` (config-load fallback to `PumpConfig::default()`, `open_read_only`, runs then steps in stored order) are verbatim (`src/pump/flow.rs:95-125`). CLI keeps the kind check, `NO_DB` handling, and `print_runs` untouched.
- **pump actions**: each `run` wrapper now only maps CLI fields to plain args and prints `Display` results; the printed strings match the base format strings character for character (checked `RequeueResult`, `WithdrawResult`, `ReplanResult`, `AnswerResult`, `UnlockResult` `Display` impls against the base `println!` formats). Job-wins-over-steps precedence in requeue preserved (`src/pump/requeue.rs:81-90`). Withdraw still trims the reason once and refuses blank; still checks *all* needs for open records before posting anything (`refuse_closed_records` + `post_all`, `src/pump/withdraw.rs:106-121`). Replan/answer/unlock error texts unchanged.
- **logview**: `view` shares `read_pump_file` (same "no pump log at …" error) and `tail_lines` (count after filter) with the streaming `show`; the non-follow CLI output is byte-identical to the base `emit` path. `--follow` still runs through `logview::run`/`show`, which is inherently a streamer.

**Verified non-issue (looked for, not a defect)**: non-follow `pump log` now prints via `println!` instead of `emit`'s explicit `BrokenPipe` arm (`src/local_pump_command.rs:146-155`). The global quiet-pipe hook (`src/main.rs:50`, `quiet_pipe::install()`) keeps the pinned contract — exit 0, silent stderr — and `tests/broken_pipe_cli.rs` (3/3) passes.

## 3. done_when evidence

1. **Library entry point with a unit test per table function** — yes. New companions: `show_test.rs` (3), `check_test.rs` (5, incl. `issues_json`), `migrate_test.rs` (3), `config_view_test.rs` (5), `flow_test.rs` (3), `unlock_test.rs` (3); extended companions: `requeue_test.rs` (+2), `withdraw_test.rs` (+1), `replan_test.rs` (+1), `answer_test.rs` (+1), `logview_test.rs` (+3). The new tests assert values, ids, records and `Display` lines, and negative paths (unknown pool, blank reason, no open record, missing file).
2. **CLI tests pass unchanged** — `git diff 470225f4..fcd0c900 -- tests/` is empty. All six named suites pass in the need worktree (counts below).
3. **Full vivi suite green** — out of my lane by instruction; the parent recorded rc 0, 239 s. My targeted runs corroborate: nothing in the touched area is red.

## 4. Tests I ran (need worktree, tip fcd0c900)

| Suite / filter | Result |
| --- | --- |
| `cargo test --test config_cli --test pool_cli --test flow_cli` | ok — 26 / 20 / 3 passed, 0 failed |
| `cargo test --test pump_cli --test pump_answer_cli --test pump_unlock_cli` | ok — 15 / 3 / 3 passed, 0 failed |
| `cargo test --test broken_pipe_cli` (relevant to the println move) | ok — 3 passed |
| `cargo test --test hygiene` | ok — 13 passed (incl. no-inline-test-modules, no-test-attrs, 1000-line file and 60-line function ceilings) |
| `cargo test --lib` filters: `pump::config::` / `pool::config_view` / `pump::flow::tests` / `pump::flow::answer::tests` / `pump::requeue` / `pump::withdraw` / `pump::replan` / `pump::unlock` / `pump::logview` | ok — 41 / 5 / 3 / 6 / 9 / 7 / 4 / 3 / 12 passed, 0 failed |
| `cargo fmt --check` | clean |

## 5. Test weakening, stale text, hygiene

- **No test weakened**: the only 5 deleted lines in `src/*_test.rs` are `use` statements extended to import the new functions; everything else is additions. `tests/` untouched.
- **No stale text**: the only mentions of `post_unlock` or the pre-lift layout are in the plan doc itself (`docs/factory/wave3-completion-goal.md:608`), which describes intent and is expected to be historical. The remaining `local_*_command` references are live dispatch wiring, not claims. New module docs say "does not print" and the grep confirms: zero `println!`/`print!` in `pump::config::*`, `pool::config_view`, `pump::flow::view`, `pump::unlock`, `logview::view`.
- **Hygiene**: all touched files are far under the ceilings (largest: `local_config_command.rs` 218, `migrate.rs` 218, `local_pump_command.rs` 259 lines); `cargo test --test hygiene` green; companion `#[cfg(test)] #[path]` wiring used throughout.

## 6. Findings

**P1** — none.

**P2** — none.

**P3** (ledger notes):

1. **Error-path stdout is now batched, not incremental.** The value functions post/write everything before returning, so the CLI prints per-item lines only after the whole call succeeds. At base, a multi-item call that failed on item N had already printed lines for items 1..N−1 (e.g. base `470225f4:src/local_config_command.rs:273-291` printed plan/diff/backup before a later charter write could fail). Disk/mail state, error text and exit codes are unchanged (the new `migrate` doc comment says the failure window matches the CLI), no oracle pins the partial output, and batch-on-success is what a server tool wants — but the observable stdout on a mid-batch failure differs. Cites: `src/pump/config/migrate.rs:66-83` + `src/local_config_command.rs:186-190`; `src/pump/withdraw.rs:100-104`; `src/pump/requeue.rs:130-137`.
2. **Four printing wrappers taking CLI structs remain in the library**: `requeue::run(project, &PumpRequeueCommand)` (`src/pump/requeue.rs:127-140`), `withdraw::run` (`src/pump/withdraw.rs:94-101`), `replan::run` (`src/pump/replan.rs:69-77`), `flow::answer::run` (`src/pump/flow/answer.rs:116-124`), plus the streaming `logview::run`/`show` (`src/pump/logview.rs:185-243`, inherent to `--follow`). They are now thin, documented CLI adapters over the plain-arg value entry points (the §3.3 target), and W3-D4 will call the value functions — but unlike config/pool/flow, the pump-action printers stayed in the lib instead of bin glue. Done_when does not require their removal; recorded as architecture residue.
3. **Minor doc nuance, no behavior impact**: `UnlockResult::entry` is sourced from `posted.request.word` (`src/pump/unlock.rs:59-62`), which is the *trimmed* entry the request stored; a whitespace-only entry would surface as `Some("")` in the value while the letter front matter filters it empty (`src/mailspace/pump_action.rs:156-175`). Not printed, not asserted anywhere; only worth knowing when W3-D4 maps it into tool output.

## 7. Verdict rationale

Every §3.3 table function has a plain-argument, value-returning library entry point with unit tests; every CLI became a printer over them; the lifts are line-faithful to the base logic including all error texts, ordering, and the migrate backup/write/charter sequence; `tests/` is untouched and all six named CLI suites plus hygiene and the module filters pass in the worktree; no test was weakened and no stale text remains. The only deltas are the three P3 notes above, none of which narrow the done_when or break a pinned oracle.

VERDICT: CLEAN
