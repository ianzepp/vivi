# W3-B5 audit — sync receive: quarantine, never silent (vivi half)

**Need branch**: `need/w3-b5` at `e3e6fdcb` (worktree `.claude/worktrees/need-w3-b5`), base
`ac02d796` (vivi main with B4). Worktree clean at audit time.
**Auditor**: independent fresh agent; read-only plus test runs; no repo edits.
**Authority**: `docs/factory/wave3-completion-goal.md` §3.2 (target + done_when, open questions
settled: unknown-front-key format-1 letters from newer members quarantine; the pump scans the
directory), §6.2 W3-B5 probe row, §5 process item 6 (verdict scale: CLEAN / RESIDUAL / BLOCK with
P1/P2/P3; P1/P2 fixed in-need, P3s to the ledger).
**Scope**: the vivi half only. The vivi-server hosted-push refusal task runs later on the server
side and is excluded here, per the audit charter.

## 1. What landed (task commits, `git log ac02d796..`)

- `7be95c4a` W3B5-1 — core: `src/sync/quarantine.rs` (hold/list/held/read_bytes/discard/refile),
  filer gate, `PullReport.quarantined` + summary + `failure()`, `FetchReport` count.
- `60fffcd8` W3B5-2 — `vivi sync quarantine list|retry|drop` CLI, doctor sync-section surface,
  SKILL.md.
- `4ee6a88f` W3B5-3 — pump board-pass alarm `sync-letter-quarantined` via `raise_once`,
  directory scan after a non-zero pull.
- `49f86914` W3B5-4 — `receipt_reason` explicit match arms instead of `.ok()` chains (hygiene
  `.ok()` budget is 7; the two `.ok()?` calls pushed it to 9).
- `bee7e7e8` W3B5-5 — vouch letters composed as format-1 mail (defect found by the server smoke).

## 2. done_when evidence (goal §3.2), vivi half

| done_when item | Evidence | Verdict |
| --- | --- | --- |
| Bad-front format-1 letter → held with JSON, not filed, pull non-zero naming it; second pull silent | `src/sync/quarantine_test.rs:97-165` (`an_unknown_front_key_letter_is_held_named_and_not_filed`): two mailspaces through the fake server, `put()` raw-ingests into A's store via `storage().ingest_message` bypassing A's compose gate, A pushes, B pulls. Asserts: `quarantined.len()==1`, id = content sha, reason names `flavor`, `refused` empty, summary contains `1 quarantined: <sha> (`, `failure()` contains the sha, exact bytes in `<sha>.eml`, receipt JSON fields, `live_messages_for_content == 0`. Second pull silent; a **resync replay** (re-download) is also silent and files nothing. Non-zero CLI exit is one pre-existing hop (`run_passes` → `failure()` → `Err`, `src/local_sync_command.rs:169-184`), pinned at unit level. | met |
| Legacy letter files as history | `quarantine_test.rs:167-182`: legacy need (no `X-Vivi-Format`) files under `needs`, quarantine dir never created. Gate: `src/sync/filer.rs:130-136` exempts only `LetterError::Legacy`. | met |
| `retry` files once the parser accepts; `drop` moves to `dropped/` | `quarantine_test.rs:296-337` (refile Filed / StillHeld) and `:264-291` (discard + id checks); CLI level `tests/quarantine_cli.rs:147-248` (retry heals, retry still-held prints reason, drop moves both files, list empties). | met |
| Pump raises `sync-letter-quarantined` exactly once per content id | `src/pump/board_test.rs:151-216`: one alarm per id across two board passes, `dropped/` not scanned, child's decoy text not the alarm, corrupt receipt still alarms without a reason, one exception log line each. | met |
| Doctor shows the quarantine count | `src/doctor/sync_section_test.rs:182-236`: warn with per-letter id+reason and a `quarantine list` fix; ok when clean; held letters surface even with no `[sync]` table; corrupt receipt → Fail. | met |
| vivi-server hosted push refused/reported | Out of scope of this audit (later server-side task); the vivi enabling surface (`PullReport.quarantined` through `file_records`, `FetchReport` count) is in place. | deferred, as ruled |
| Full suites green | Parent's ledger: 232.6 s full pass pre-B5-4; post-B5-5 rerun was in flight at audit time and is **not** claimed as evidence here. This audit's own battery (§5) is green. | parent's record |

**§6.2 probe shape check**: the probe asks for exactly "two temp mailspaces syncing through the
fake server, push a format-1 letter with an unknown front key from A bypassing A's gate with a raw
ingest helper; pull on B → `.vivi/quarantine/<sha>.eml` + `.json`, pull exit non-zero naming it;
second pull silent; `drop` moves to `dropped/`". The landed tests do exactly that shape — `put()`
(`quarantine_test.rs:23-43`) is the raw ingest helper; every probe step is asserted, spread across
the three test files above.

## 3. Correctness edges

- **id validation vs path tricks** (`src/sync/quarantine.rs:268-277`): `checked_id` requires
  exactly 64 chars of `[0-9a-f]`. No slash, dot, or uppercase survives; `../../mailspace.toml`
  and `zz` are refused (pinned, `quarantine_test.rs:286-289`, `quarantine_cli.rs:244-266`).
  Airtight: the id is only ever interpolated into a filename after this check.
- **Cursor advance / re-download / idempotency**: a quarantined record returns `Ok(())` from
  `file_record`, so `advance` + `state.save` proceed (`src/sync/pull.rs:386-400`) — held, not
  dropped, and a normal re-pull does not re-download (cursor moved past). A resync replay does
  re-download; the gate re-fires and `hold` finds the receipt → `Ok(false)` → not counted, pull
  exits zero, nothing filed (pinned at `quarantine_test.rs:146-164`). The receipt is written last
  (tmp + rename, `quarantine.rs:80-99`), so a crash between the `.eml` and `.json` writes
  self-heals on the next hold. If the hold write itself fails, the error propagates out of
  `file_batch` before `advance`/`save`, so the cursor does not move past the letter. Idempotency
  is keyed on content sha256 = file name; airtight.
- **Receipt corrupt/missing branches**: corrupt `.json` → `list` errors (surfaced, never
  skipped) → doctor Fail (`sync_section_test.rs:228-236`), CLI list non-zero; the pump alarm
  still fires, without a reason (`board_test.rs:204-215`). Missing `.json` with `.eml` present →
  invisible to list/doctor/pump; a resync re-download re-holds and alarms once (self-healing).
  `.json` present with `.eml` hand-deleted → listed but `read_bytes` errors; `discard` tolerates
  the missing `.eml` (`quarantine.rs:169-179`). Orphan `.json.tmp` is skipped everywhere
  (extension != json). No silent loss path found.
- **Filer gate placement** (`src/sync/filer.rs:99-145`): after base64, hash, envelope,
  forged-from, governance, and the already-held check; before `file_new`. A letter failing BOTH
  transport and parse → **transport wins**: it lands in `report.refused`, bytes are not held.
  This matches the goal's ordering ("after the transport checks") and the implementer's stated
  contract ("transport refusals keep report.refused"); the pure-parse case pins
  `refused.is_empty()` (`quarantine_test.rs:113-115`). The both-fault precedence itself is not
  pinned by a combined test — P3-1.
- **Refile's synthesized record** (`quarantine.rs:198-256`): `from_prefix` is derived from the
  From header's own domain claim, so `forged_from` agrees by construction; the real stamp check
  ran at first quarantine (a forged letter is refused before the gate and can never be in
  quarantine). `to: None` passes `envelope_agrees`; a non-traveling wire id still refuses inside
  `file_new` → `StillHeld` with the reason. Cursor/floor untouched by `file_records`. Sound.
- **Doctor fail-vs-warn boundaries**: held → Warn (actionable, operator clears by hand); corrupt
  receipt → Fail (`Status::Fail` drives the doctor verdict, `src/doctor.rs:191`); clean → Ok;
  held letters surface even when sync is unconfigured (`sync_section.rs:44-51`, pinned). Matches
  "a corrupt receipt is surfaced, never skipped".

## 4. The pump alarm

- **Exception shape**: `raise_once(ctx, "sync-letter-quarantined", <content id>, text)` — kind
  `sync-letter-quarantined`, unit the receipt stem, text `letter <id> quarantined: <reason>`
  (`src/pump/board.rs:113-121`). Same `raise_once` helper as the flow alarms
  (`src/pump/flow/tick.rs:233-238`), deduped per home in-process.
- **Health surface unchanged**: `src/pump/health.rs` is untouched by the diff; the alarm appears
  through the pre-existing exceptions surface (`exceptions.txt`, pinned in `board_test.rs`).
- **Once per id across passes**: pinned — two board passes, one alarm, one exception log line
  (`board_test.rs:197-201`); a second held id added later alarms independently (`:203-210`).
- **Zero-exit no-scan**: pinned (`board_test.rs:218-242`) — a zero exit does not scan and does
  not log, even with a receipt on disk. The condition aligns with the CLI: a newly held letter
  makes `failure()` non-empty → `vivi sync pull` exits non-zero.
- **`.eml`/`dropped` ignored**: the scan takes only `*.json` stems (`json_stem`, non-empty) and
  skips directories; pinned by the decoy and `dropped/` assertions (`board_test.rs:155-190`).
- **Restart re-raise risk, honestly**: `raise_once`'s memory is a process-local static
  (`tick.rs:222-231`), so after a pump restart a still-held letter alarms **once more** the next
  time some pull exits non-zero (e.g. a later unrelated refusal or a new quarantine triggers the
  directory scan, which raises for every held id). Assessment: acceptable noise, not a defect —
  it is bounded (≤ one line per id per process), self-describing (id + reason), redundant with
  the doctor's persistent warn while anything is held, and the clearing verbs remove the receipt
  that drives it. Persisting the seen-set would add state for marginal gain and would diverge
  from the other `raise_once` alarm kinds. P3-2 to the ledger, no fix demanded.

## 5. Tests I ran (need worktree, 2026-10-24, burgus)

| Command | Result |
| --- | --- |
| `cargo test --lib sync::` | 355 passed, 0 failed, 2 ignored (17.7 s) |
| `cargo test --lib pump::board` | 13 passed, 0 failed (0.13 s) |
| `cargo test --lib doctor::` | 151 passed, 0 failed (0.36 s) |
| `cargo test --test quarantine_cli` | 6 passed, 0 failed (0.20 s) |
| `cargo test --test hygiene` | 13 passed, 0 failed (0.22 s) |
| `cargo test --test skills_cli` | 6 passed, 0 failed (0.16 s) |
| `cargo check --all-targets` | rc 0; the warnings it emits all point at pre-existing sites (`src/pump/amend.rs`, `collect*`, pool/storage tests) — none at a B5 file |
| `cargo fmt --check` | clean |

Full suite: not rerun by this audit (charter says not to); parent's green records remain the
delivery evidence, and the post-B5-5 run was still in flight when this audit started.

## 6. W3B5-5 vouch cross-checks

- **Server parse path not broken**: vivi-server `src/projects.rs:993-1001` (`letter_body`) reads
  the prose after the first blank line (`\r\n\r\n` or `\n\n`, whichever first) and
  `parse_vouch` (`:1019-1029`) JSON-parses that. The new `X-Vivi-Kind`/`X-Vivi-Format` headers
  live inside the header block, before the blank line, so the cut still lands on the JSON. The
  fake server's `admit_vouch` was updated to the identical cut (`src/sync/fake_server_test.rs`)
  with the same admission assertions — an accommodation to the new letter shape, not a
  weakening. `check_letter_format` (`projects.rs:1033-1038`) accepts the vouch: it parses as
  format-1 mail with a prose body.
- **Compose-side guarantees**: `compose_vouch` runs the body through `letter::gate("mail", …)`
  and parses the composed bytes back with `of_message` before anything is sent
  (`src/sync/vouch.rs:54-72`); pinned by `vouch_test.rs:54-74` (format header present, kind
  mail, prose not a fence, JSON round-trips).
- **No legacy writer left in vivi** (stamp sites re-grepped, not taken from the ledger): every
  production letter-writing path stamps `X-Vivi-Format` via `add_header` — `delivery.rs:189`,
  `reply.rs:102`, `delivery/memo.rs:51`, `rules.rs:296`, `item_events/emit.rs:442`,
  `item_events/property_emit.rs:48`, and vouch since W3B5-5. The remaining raw `From:` sites
  (`message/render.rs:41`, `mailspace/thread.rs:491`, `local_mailspace_dump.rs:81`,
  `message/compose.rs:196`) are display/dump printers, not stored-letter composers.

## 7. No test weakened, hygiene

- `git diff ac02d796.. --name-only -- tests/` → only the new `tests/quarantine_cli.rs` (266
  added lines). Under `src/`, the companion-test diffs are pure additions except
  `fake_server_test.rs` (+18/−5, the vouch-body cut above, assertion strength unchanged).
- `cargo test --test hygiene` green: no inline test modules, no test attrs in production files,
  file/function ceilings, and the monotonic banned-pattern budgets (the `.ok()` count that
  motivated W3B5-4 is back inside budget; no `.ok()` remains in any B5 production file).

## 8. Findings

No P1. No P2.

- **P3-1 (pinning gap)**: the both-fault precedence — a letter that fails a transport check AND
  the letter gate is refused (not quarantined), by ordering at `src/sync/filer.rs:104-127` —
  matches the goal but is not pinned by a test that combines both faults. Suggest one filer test:
  bad hash + bad front → `refused`, quarantine dir absent.
- **P3-2 (accepted noise)**: `raise_once` is process-local (`src/pump/flow/tick.rs:222-231`), so
  a pump restart plus a later non-zero pull re-raises a still-held id once. Assessed acceptable
  (§4); ledger note, no change requested.
- **P3-3 (cosmetic edge)**: a receipt whose `.eml` was hand-deleted stays listed while `retry`
  errors (`read_bytes` → "no quarantined letter"); `drop` clears it. Could say "bytes missing"
  instead of "no quarantined letter", but no data path is wrong.
- **P3-4 (pinning gap)**: the non-zero exit of `vivi sync pull` on a quarantine is pinned at the
  `failure()` unit level (`quarantine_test.rs:126-129`) plus the pre-existing refused-path CLI
  wiring; no CLI-level test spawns a pull that holds a letter. Low risk (one pre-existing hop).

## 9. Verdict

The done_when list for the vivi half is met with real tests in the probe's exact shape, the
edges hold up under reading (id validation, cursor semantics, receipt branches, refile
synthesis, doctor boundaries, alarm once-per-id), no test was weakened, hygiene and skills are
green, and W3B5-5's server cross-check passes. Four P3s, none demanding in-need work.

VERDICT: RESIDUAL (no open P1/P2; P3-1..P3-4 to the ledger)
