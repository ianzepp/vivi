verdict: BLOCK

# W3-E2 audit — `vivi pump reset`

Auditor: independent (did not implement). Read-only on git; no repo files touched.
Repo: `/Users/ianzepp/work/ianzepp/vivi/worktrees/need-w3-e2`, branch `need/w3-e2`, tip `a67d4268`.
Working tree clean. Commits `main..need/w3-e2`:

- `a67d4268` merge W3E2-2 into need/w3-e2
- `9207113e` W3E2-2: vivi pump reset CLI, skills and README
- `7962a54b` merge W3E2-1 into need/w3-e2
- `1c0a00cb` W3E2-1: pump::reset library: archive the pump home, initialize fresh

Authority: `docs/factory/wave3-completion-goal.md` §2.3. Full-suite rc was not re-run here (parent record: `cargo test --no-fail-fast` rc 0, 4:20; `cargo fmt --check` clean).

BLOCK is one open P2. The single-reset done_when holds. Do not merge until the same-second archive clobber is fixed and re-audited.

## Findings

No other P1. One P2. Two P3s.

### P2-1. A second reset in the same UTC second overwrites the archive and exits 0

`src/pump/reset.rs:59` uses `create_dir_all` on `pump-archive-<YYYYMMDDTHHMMSSZ>` (`reset.rs:84`). An existing directory is success. `src/pump/reset.rs:96` then `fs::rename`s each present entry into that directory. On POSIX, renaming a file onto an existing file replaces it. There is no "destination must not exist" check.

Reproduced live (probe below), without setting the clock. First reset archived a 13-byte `pump.db` (`seed pump.db\n`) into `pump-archive-20261024T023654Z`. The second reset, same home, same stamp, rc 0, printed that same path and `moved pump.db` / `pump.db-wal` / `pump.db-shm`. The archived `pump.db` is now the fresh SQLite file (294912 bytes, `SQLite format 3`). The archived `-wal` and `-shm` were replaced too (wal is now 0 bytes). `plans/`, `state.json`, and the other first-reset entries stayed, so the directory is a mix of the first archive and the second database. `log.txt` gained a second identical `reset: archived to …` line. The original database bytes are gone.

One reset is fast enough that a retry, a double invocation, or two calls in one script second hits this. The archive is the safety copy; this destroys it and reports success. Tests only check the `pump-archive-` prefix (`src/pump/reset_test.rs:67-73`, `tests/pump_reset_cli.rs:38-46`), so they do not catch it.

Fix in this need: do not reuse an existing archive directory. `create_dir` (fail if it exists) before any rename, and do not rename onto an existing destination. On that failure, move nothing.

### P3-1. Any stat error is treated as "missing"

`src/pump/reset.rs:93-95`: `symlink_metadata` failure `continue`s. Not-found is the skip §2.3 asks for. Any other error (permission, I/O) also skips. A present `pump.db` that could not be stat'd stays in the home, and `control::initialize` then opens that existing database instead of seeding a new Stopped one (`src/pump/control.rs:70-75`, `initialize_control` keeps an existing mode at `src/pump/store.rs:305-314`). Not seen in the probe. Ledger.

### P3-2. The skill does not say a paused pump is refused

`skills/vivi/SKILL.md:2501-2503` says reset "is refused while the pump runs". That is true of a live process: pause returns from the tick without dropping the lock (`src/pump/run.rs:45-48`, lock taken at `run.rs:148` and held until the process returns). The next paragraph (`SKILL.md:2507-2508`) tells the reader that target/rungs refusals are cleared with `vivi pump pause`. Reset does the opposite of those config writes: a paused process still holds the lock and reset refuses it (§2.3). The command error names `vivi pump stop`, so this is not a behaviour bug. The skill entry should say pause is not enough. No removed-surface names in the new reset lines. README `README.md:275` only adds `reset` to the pump command cell, which is what §2.3 asked for.

Not filed: `error: message error:` in front of the refusal sentence. That is `VivariumError::Message`'s Display (`src/error.rs:17`) plus `eprintln!("error: {e}")` (`src/main.rs:66`). The required sentence is in stderr. The CLI test asserts `contains` (`tests/pump_reset_cli.rs:113-117`).

Not filed: reset does not hold the pump lock across the renames. `lock_is_held` (`src/pump/run.rs:538-545`) drops its `try_lock` on the way out, and `archive` (`reset.rs:53-61`) moves only after that. A pump that starts in the window can interleave. §2.3 requires the check, not a held lock for the duration. Ledger-worthy, same family as P3-1, left as a note rather than a third P3.

## What matches §2.3

- Public `vivi::pump::reset::archive(&Path) -> Result<ResetReport, VivariumError>` (`src/pump/reset.rs:51`). No printing. Wired `pub mod reset` at `src/pump.rs:38`; unit tests via `src/pump.rs:145-146`.
- CLI `vivi pump reset [--project P] [--json]` (`src/cli/pump_command.rs:25-26`, `102-109`; dispatch `src/local_pump_command.rs:55`, `117-148`).
- Refusal text is exactly `the pump is running (lock held): run vivi pump stop and wait for it to exit, then reset` (`reset.rs:54-56`), and it returns before `create_dir_all` / rename / log append / `initialize`. A paused process is refused because it still holds that lock, not because of the db mode.
- Archive directory is `<home>/../pump-archive-<UTC YYYYMMDDTHHMMSSZ>/` (`reset.rs:77-85`). Literal `Z`, `Utc::now`, zero-padded. Probe name `pump-archive-20261024T023654Z` (22:36:54 ET is 02:36:54Z).
- Move list matches §2.3 (`reset.rs:15-26`): `pump.db`, `pump.db-wal`, `pump.db-shm`, `state.json`, `state.json.imported`, `plans`, `resplit`, `reserved.txt`, `exceptions.txt`, `status.txt`. Rename, not copy. A missing entry is skipped (probe omitted `state.json.imported`; it was not in the archive).
- Kept in place, with contents: `targets/`, `merge-lanes/`, `sync-push.log`, `lock`. `log.txt` kept and appended `reset: archived to <dir>\n` (`reset.rs:103-109`).
- Fresh db via `control::initialize` (`reset.rs:63`), which seeds Stopped when no db exists (`control.rs:70-75`). Probe health JSON `"control": "stopped"`. A leftover `PAUSE` file is honoured (report and health `paused`, marker removed). A leftover `STOP` file stays Stopped and is removed. That is today's `initialize`, not a special case in reset.
- Open pump records are mailspace letters: `Mailspace::pump_list(None, Some(PumpState::Open))` (`reset.rs:64-66`, `src/mailspace/pump_record.rs:225-238`, holder rows in `pumps`/`done` at `pump_record.rs:398-416`). Count survived replacing `pump.db`. Text and JSON name `vivi pump withdraw` and say the next tick re-ingests open records (`local_pump_command.rs:115`, `122-145`).
- No call to `state_rows::clear` or `job_release`. The branch diff does not touch `src/pump/store/state_rows.rs` or `src/pump/store/job_release.rs`.

## Tests were not weakened

`git diff main..HEAD --numstat`: the only test file is new `tests/pump_reset_cli.rs` (290/0). `src/pump/reset_test.rs` is new (189/0). Deletions elsewhere are the `cli.rs` re-export wrap, the `local_pump_command.rs` module comment and import wrap, and one README cell. No assertion line was removed or loosened. No production behaviour outside reset's dispatch, the `pub mod reset` line, and the `PumpResetCommand` re-export.

Required tests are present and pin the contract:

- moves the listed entries, keeps `targets/` `merge-lanes/` `sync-push.log` `lock` `log.txt`, appends the log line (`reset_test.rs:59-122`)
- held lock refuses with the exact sentence and creates no archive (`reset_test.rs:125-153`)
- after reset `control::inspect` is Stopped and the old running db was archived (`reset_test.rs:156-175`)
- open-record count is 1 with one open and one done letter (`reset_test.rs:178-189`)
- CLI text, JSON, and lock refusal (`tests/pump_reset_cli.rs:28`, `:74`, `:101`)

## Hygiene

- `src/pump/reset.rs` has no `#[cfg(test)]` and no `#[test]`. Tests live in `src/pump/reset_test.rs`.
- Scanner in `tests/hygiene.rs` (60-line ceiling; it only recognises lines starting `fn ` / `pub fn ` / `async fn ` / `pub async fn `): `archive` 23, `archive_dir` 10, `move_entries` 12, `append_log_line` 8, `pump_reset` 4, `print_reset` 15, `reset_json` 11. All under 60.
- File sizes: `reset.rs` 110, `local_pump_command.rs` 265, `pump_command.rs` 480. Under 1000.
- `pub(crate) fn handle_pump_command` (`local_pump_command.rs:37-113`, 77 lines) is not counted by that scanner and grew by one match arm. It was already over 60 before this need. Not a new hygiene break; the enforced test does not see it.

## Live probe

Binary: `target/debug/vivi` in this worktree, mtime 2026-10-23 22:17:02, newer than `src/pump/reset.rs` (21:59:06) and `src/local_pump_command.rs` (22:16:14). No rebuild. Script: `/tmp/w3-e2-probe.sh`. Tree left at `/tmp/w3-e2-audit-probe/`.

| Check | Command / setup | Result |
| --- | --- | --- |
| Help | `vivi pump --help` | rc 0. Line: `reset     Archive the pump home and start fresh; refused while the pump runs` |
| Single reset | `mailspace init`, identity `mind`, `need send`, `pump send`, seeded home including `targets/default/marker.txt` and `merge-lanes/lane.txt`, omitted `state.json.imported`. `vivi pump reset --project` | rc 0, stderr empty. Archive `/tmp/w3-e2-audit-probe/a/.vivi/pump-archive-20261024T023654Z` (parent of the home, regex `pump-archive-[0-9]{8}T[0-9]{6}Z`). Moved the nine present entries. Archived `pump.db` is `seed pump.db\n`. New home `pump.db` starts `SQLite format 3`. `state.json.imported` absent from the archive. `targets` marker, lane file, `sync-push.log`, and `lock` (`keep-lock`) unchanged. `status.txt` gone from the home. `log.txt` is `old line\n` plus `reset: archived to <archive>\n`. Stdout: `control: stopped`, `open pump records: 1`, and the withdraw note. |
| Health | `vivi pump health --json --project` on that project | rc 0. `"control": "stopped"`. Verdict `unknown` only because the fresh project is not ready (pools/roles); not a reset failure. |
| JSON | second project, two open pump records, `vivi pump reset --json` | rc 0. `"mode": "stopped"`, `"open_records": 2`, moved list equals the nine seeded names, note names `vivi pump withdraw`. `pump list` showed both records `open` before the reset. |
| Empty home | `mailspace init` only, no seeded entries, `vivi pump reset` | rc 0. No `moved` lines. `control: stopped`, `open pump records: 0`. Fresh `pump.db`, created `log.txt` with the reset line, empty `lock` from `initialize`'s migration lock. Empty archive dir (nothing to move). |
| `PAUSE` | `PAUSE` file plus `pump.db` = `old-db\n`, then reset | rc 0. `control: paused`. Health `"control": "paused"`. `PAUSE` removed. Archived `pump.db` is `old-db\n`. |
| `STOP` | `STOP` file plus `pump.db` = `old-db\n`, then reset | rc 0. `control: stopped`. Health `"control": "stopped"`. `STOP` removed. |
| Lock held | Python `fcntl.flock(LOCK_EX\|LOCK_NB)` on `<home>/lock` (same flock as `std::fs::File::try_lock`), then `vivi pump reset` | rc 1. Stdout empty. Stderr: `error: message error: the pump is running (lock held): run vivi pump stop and wait for it to exit, then reset`. No `pump-archive-*`. SHA-256 snapshot of the pump home unchanged. |
| Same-second second reset | `vivi pump reset` again on project A in the same UTC second | rc 0. **P2-1.** Same archive path. Archived `pump.db` no longer the seed; it is the first reset's fresh SQLite file. `-wal`/`-shm` replaced. `plans/need.json` from the first archive still there. |

## Out of scope, checked and left

`state_rows::clear` / `job_release` are untouched, per the §2.3 ledger correction. No v19 surface names (`promote`, `closeout`, `pump status`, `adopt`) in the added skill or README lines.

## pass 2

verdict: CLEAN

Tip `fe166263` (merge W3E2-3). Fix commit `b1496d14`. Diff vs pass-1 tip `a67d4268`: `src/pump/reset.rs`, `src/pump/reset_test.rs`, `skills/vivi/SKILL.md` only. Working tree clean. No new finding.

### Findings closed

- **P2-1 fixed.** `archive_dir` (`src/pump/reset.rs:84-107`) takes `pump-archive-<stamp>` when `symlink_metadata` fails, otherwise the first free `pump-archive-<stamp>-N` for `N` in `2..=1000` (`ARCHIVE_SUFFIX_LIMIT`, `reset.rs:29`). Past `-1000` it returns an error and does not move. `archive` then `fs::create_dir` (`reset.rs:64`), not `create_dir_all`, so a name taken between the pick and the create fails before any rename. `move_entries` (`reset.rs:122-127`) returns `already exists in the archive; refusing to replace it` when the destination stats, and only then calls `rename`. Regression: `a_second_reset_gets_its_own_archive_and_keeps_both_pump_dbs` (`reset_test.rs:125`) and `an_archive_destination_that_exists_is_an_error_not_a_replace` (`reset_test.rs:152`). Both passed.
- **P3-1 fixed.** `move_entries` (`reset.rs:116-119`) continues only on `ErrorKind::NotFound`. Any other `symlink_metadata` error is returned. A present `pump.db` that cannot be stat'd is no longer skipped.
- **P3-2 fixed.** `skills/vivi/SKILL.md:2501-2503`: "refused while the pump runs (a paused pump still holds the lock and is refused too)". No removed-surface names.

### Bound, paths, races (no new defect)

- Suffix range is inclusive: second archive in one second is `-2`, last try is `-1000`, then the error at `reset.rs:102-106`. Not an off-by-one against the stated `-2`..`-1000` bound. The error text says "after 1000 suffixes"; the numbered tries are `-2` through `-1000` (999 names) plus the unsuffixed base. Wording only. Not filed.
- The archive is still `home.parent()` joined with the stamp name (`reset.rs:85-97`). A custom `[pump] home` archives beside that home, same as pass 1. `create_dir` does not mkdir missing parents; the parent exists whenever the home directory does. A symlinked home is still lexical (unchanged, not introduced here).
- Two resets that both observe the base name free: one `create_dir` wins, the other gets the io error and has not moved anything. It does not fall through to `-2` and it cannot enter the winner's directory. A later sequential reset in that same second takes `-2`. That is the exclusive-create rule, not a hole.
- `symlink_metadata(...).is_err()` on the name picker (`reset.rs:93-99`) still treats a non-NotFound stat error as "free". `create_dir` is the gate: if the name exists, create fails and nothing is moved. Not the P3-1 failure (that one skipped a move and opened the old db). Not filed.
- A destination that appears between the exists-check and `rename` could still be replaced. The directory was just created empty by `create_dir`, so this does not overwrite a previous reset's archive. Not filed.
- `move_entries` is `pub(crate)` so the sibling test module can call it. Hygiene scanner ignores `pub(crate) fn`; the function is 21 lines. `archive` 25, `archive_dir` 24, by that scanner. `reset.rs` is 142 lines.

### Commands

`cargo test --lib reset` (the filter also matches other tests whose names contain `reset`). Compile reused the warm test binary (`Finished … in 0.17s`). rc 0, 29 passed, 0 failed, 1.23s run, 1.43s wall. The six `pump::reset_test` tests passed, including both new ones, the lock refusal, the move/keep test, `control` Stopped, and the open-record count.

`cargo test --test pump_reset_cli`. rc 0, 3 passed, 0 failed, 0.20s run, 0.36s wall (`Finished … in 0.13s`). Text, JSON, and lock-held refusal.

No full suite. No new probe of the old binary (`target/debug/vivi` predates `b1496d14`); the tests above are the new code.
