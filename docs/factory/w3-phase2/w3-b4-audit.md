# W3-B4 audit — headers carry transport only; every writer goes through the gate

Independent auditor: fresh agent, read-only + tests; did not implement any of it.
Date: 2026-10-24. Need branch `need/w3-b4` at `e4760b74`, base `470225f4` (v21.0.0), 9 merges
(B4-1 letter core, B4-2 close events, B4-3 notes/reports/extras retired, B4-4 memo tags, B4-5 rules,
B4-6 property events + task_from, B4-7 pin tests + of_message refusal, B4-8/8b skills).
Authority: `docs/factory/wave3-completion-goal.md` §3.1 (target behaviour, settled rulings,
recommendations 1-3 treated as settled, done_when), §5 item 6 (verdict scale), §6.2 W3-B4 probe row.
No repo file was edited; the only file written is this report.

## 1. done_when evidence

### (a) The seven content headers find no writer — PASS

`grep -rn '"X-Vivi-\(Verdict\|Repo\|Branch\|Commit\|Tag\|Rule-Scope\|Supersedes\)"' src` in the
need worktree returns exactly one production hit: `src/letter/message.rs:15-21`
(`MOVED_CONTENT_HEADERS`, the refusal list — the one reader the goal allows). Every other hit is a
test file pinning absence or exercising legacy:

| Hit | Classification |
| --- | --- |
| `src/letter/message.rs:15-21` | refusal list (allowed reader) |
| `src/mailspace/writers_test.rs:14-20`, `src/mailspace/reply_test.rs:12-18`, `src/mailspace/item_events/property_emit_test.rs:77-83` | absence pins |
| `src/mailspace/delivery_test.rs:360-364,462-465,640` | absence pins |
| `src/mailspace/memos_test.rs:53` | legacy header-tag fixture (pinned ignored) |

A case-insensitive sweep (`vivi-verdict|vivi-repo|vivi-branch|vivi-commit|vivi-tag|rule-scope`) over
production `src` finds the same single list; `plain.rs:54` matches only the front-matter key name
`supersedes` in `check_text`. No `header_value`/`header_raw` reads of content headers exist.

### (b) Every bypass-table writer produces format-1 bytes — PASS

`src/mailspace/writers_test.rs` composes through each writer family (14 letters,
`assert_eq!(letters.len(), 14, "a writer family went missing")`) and asserts per family:
`X-Vivi-Format: 1` present and none of the seven headers present. Mapping to the goal's bypass
table:

| Goal writer | Families in the pin | Count |
| --- | --- | --- |
| `note_reply` | plain note; report note (`[close]` front); pool result note (`record_pool_note` routes through `note_reply`) | 3 |
| `save_memo` | tagged memo; bare memo | 2 |
| `ingest_rule` (via `save_rule`/`supersede_rule`, `rules.rs:90-130` → `ingest_rule:266`) | rule with scope + supersedes | 1 |
| lifecycle events (`compose_event_letter`, `emit.rs:412`) | close with verdict/branch/commit/repo tips; bare close (empty front) | 2 |
| property events (`compose_property_letter`, `property_emit.rs:183`) | marked, bound, dependency-added, delegation (assigned), state (absorbed) | 5 |
| `task_from_source` (`lifecycle.rs:297`) | task-from letter | 1 |

`dependency-removed` shares `compose_property_letter` with `dependency-added` (one family, same
composer); `reopened` shares `compose_event_letter` with `done`. None missed.

Round trips through `letter::of_message` per writer exist in the companion tests:
`reply_test.rs` (note, report note `[close]`, pool note — each `of_message` + kind + front + prose),
`delivery_test.rs` (close event with facts, bare close, report note, memo tags, bare memo),
`memos_test.rs` (`parse_tags`), `tests/rules_cli.rs:163-180` (`of_message` on a superseding rule),
`item_events/property_emit_test.rs` (2 `of_message` round trips), `src/mailspace/tests.rs`
(task_from parses as a task). `writers_test.rs` itself greps bytes without calling `of_message`
(P3-1). Every production composer also parse-checks its body at write time
(`parse_kind` in `emit.rs:429`, `property_emit.rs:32`, `rules.rs:281`; `letter_gate::gate` in
`reply.rs:88`, `delivery/memo.rs:37`), so a parse regression fails the writer call loudly.

### (c) No empty-front letter shows a `+++` fence — PASS

Pinned in `tests/local_mailspace_cli.rs` (53 tests green): plain reply notes and empty-front done
events render with no `+++` in thread text and `--json`
(`replies_and_lifecycle_notes_assemble_cross_kind_threads` additions); an untagged memo thread
shows no fence in text or json (`an_untagged_memo_thread_shows_no_fence_in_text_or_json`); a rule
always carries `scope =` so its fence has content, never an empty pair. Unit level:
`message_test.rs` `letters_with_an_empty_front_carry_no_fence_through_the_wire` (memo, rule, close
event, `EmptyFront` event), `plain_test.rs` `an_empty_front_renders_as_prose_with_no_fence`.

Live probe (mine, temp mailspace, `target/debug/vivi`): close with verdict/branch/commit/repo tips,
tagged + untagged memos, rule with supersedes, marked need. Every stored blob carries
`X-Vivi-Format: 1`; none carries any of the seven headers; the untagged memo is stored and shown
as bare prose; no empty `+++\n+++` pair exists in any stored letter outside need/task/want work
letters — which the goal and the gate explicitly keep ("an empty front matter is fine" for a task;
the no-fence ruling V1 covers note, memo, rule and event letters). The probe scripts live at
`/private/tmp/w3b4-probe.sh` (session scratch, not in the repo).

### (d) Full suite — recorded, not rerun

Parent ran `cargo test` on the need branch: rc 0, 237 s, per instruction not repeated here.

## 2. No test weakened — PASS

`git diff 470225f4..e4760b74 -- tests/ 'src/**/*_test.rs'`: 20 files, +1672/−88. Each changed pin
maps to a ruling; most got stronger (byte-string assertions became `of_message` parse assertions
with the same values, plus new negative pins that the old headers are gone):

- `backlog_test.rs:324-334`, `delivery_test.rs` (close, report, memo), `rules_cli.rs:163-180`:
  header-substring pins → `of_message` front-matter parse pins, same facts asserted.
- `delivery/compose_test.rs:399-407`: `a_memo_kind_send_is_unchanged_and_unformatted` →
  `..._formatted_with_no_fence`. The flip from `!contains("X-Vivi-Format")` to `contains(...)`
  is the ruled behavior (goal: every writer format-1); memo prose behavior is separately pinned.
- `gate_test.rs:149-156`: unknown-kind probe switched from `memo` to `zap` because memo is now a
  gated kind; unknown-kind semantics preserved, and memo/rule prose is covered by three new tests.
- `memos_test.rs:40-58`: repeated-`X-Vivi-Tag`-header round trip deleted (transport removed by
  ruling) and replaced by front-matter tags + an explicit legacy-header-ignored pin
  (`parse_tags_ignores_a_legacy_memo_header_tag`) — recommendation 1 pinned, not silently dropped.
- `filer_test.rs:52-96`, `link_test.rs`, `item_events_test.rs:110-160`, `sync/events_test.rs`:
  legacy verdict headers now pinned as *not* read (recommendation 1, legacy-ignored, no
  legacy-display branch anywhere); format-1 front-matter shapes pinned in their place.
- `rules_test.rs:55-70`: legacy rule headers pinned unread; empty-front rule reads project scope.

No expectation was deleted without a ruled replacement; no coverage silently loosened.

## 3. The B4-7 refusal (format-1 + moved header → `LetterError::Invalid`)

`src/letter/message.rs:92-114`: header scan stops at the first blank line (prose quoting
`X-Vivi-Tag:` cannot trigger it), matches names case-insensitively, and refuses with
`Invalid("letter {handle} carries `{header}` as a header: a format-1 letter states that fact in
its front matter, and the header is not read")` — names the header, the fix, and why. Pinned per
family shape in `message_test.rs` (`a_format_one_letter_with_a_moved_content_header_is_refused_per_family`:
memo, rule, note, report, close-event paths).

Edge cases assessed honestly:

- **Legacy letters**: no `X-Vivi-Format` → `LetterError::Legacy` before any moved check; headers
  unread, letter filed as history (`a_legacy_letter_keeps_its_moved_content_headers_ignored`;
  sync fold/store pins). No legitimate letter broken.
- **Newer formats**: the format check runs *before* the moved check, so a format-2 letter carrying
  a moved name is `UnknownFormat` ("upgrade vivi"), not the moved refusal. Correct ordering.
- **A future format-1 vivi reintroducing one of the names**: today's vivi refuses it. That is the
  decided strictness (goal test spec: "a raw letter with a moved header and `X-Vivi-Format: 1` is
  refused ... decide and pin"); the seven names are permanently retired from the format-1 header
  namespace, and a writer that wants them back must bump the format, which `UnknownFormat` already
  models with an actionable upgrade message. The alternative (silently ignoring the fact) is what
  legacy gets; refusing format-1 is what keeps the format contract honest. No change requested.
- **Raw import** (`vivi mail deliver` → `deliver_raw` → `compose::raw_letter`,
  `delivery.rs:274`): non-Legacy parse errors map to `VivariumError::Message("letter format:
  {error}")` and abort the deliver; legacy bytes pass through. Surfaced sanely at the CLI.
- **Sync pull**: `file_record` has no letter-format check on this branch, so a malformed format-1
  letter arriving by sync is stored and simply reads as carrying no facts (every reader routes
  through `of_message`, which errors → empty). Quarantine is W3-B5's admitted scope (goal §3.2),
  which runs after B4 — recorded as P3-2, not a B4 defect.

## 4. Interim-regression closure: report-verdict-over-sync — PASS

B4-2 flagged, B4-3 closed. The read path is one shape for both carriers:
`item_events/property.rs:222-282` (`parse_letter_verdicts` → `close_front_of`) accepts a
format-1 `done`/`reopened` event's own `CloseFront` **and** a report note's `[close]` table in
`MailFront` (`records.rs:240-243`), stamps both by `(X-Vivi-Event-At | Date, Message-ID)`, and
folds winners into `item_metadata` identically; `sync/link.rs:100-116`
(`carries_close_facts`) mirrors the same dual shape for the filer's touched/settle decision.
`sync/events_test.rs` proves the wire path end to end:
`a_pulled_report_letter_stores_its_verdict_in_item_metadata` (assignee close travels as a report
note; pull on the author's side folds `verdict=fixed`, `repo:vivi=sha9`, row to `sent`) and
`a_pulled_legacy_report_letter_with_verdict_headers_folds_nothing` (recommendation 1). The
deliberate predicate difference on unlocated closes (`property.rs carries_verdict` counts a close
event's clearing branch/commit pair; `link.rs has_verdict` does not) is coherent: close events are
already covered by `is_event`/`parse_event` in both consumers, and the empty pair matches local
close behavior (`emit.rs:344-352` writes the empty pair as the reset marker).

## 5. Hygiene — PASS

- No inline `#[cfg(test)]` module bodies and no `#[test]` attributes in production files; only the
  sanctioned companion wiring (`#[cfg(test)] #[path = "..._test.rs"] mod tests;`). `cargo test
  --test hygiene`: 13 passed.
- File ceiling: `delivery.rs` split to 576 lines (+ `moves.rs` 355, `memo.rs` 128, `compose.rs`
  96); largest touched file is `lifecycle.rs` at 875. All under 1000.
- No new `unwrap`/`panic!`/`expect` in production files (diff hits were in `src/mailspace/tests.rs`,
  an excluded test companion).
- No dependency changes: `Cargo.toml`/`Cargo.lock` untouched by the diff.
- `cargo fmt --check` in the need worktree: clean.

## 6. Suites run in the need worktree (all green; full suite intentionally not rerun)

| Command | Result | Wall |
| --- | --- | --- |
| `cargo test --lib letter` | 302 passed, 2 ignored | 3.9 s |
| `cargo test --lib 'mailspace::'` | 520 passed | 8.5 s |
| `cargo test --lib 'sync::'` | 347 passed, 2 ignored | 14.9 s |
| `cargo test --test hygiene` | 13 passed | 0.2 s |
| `cargo test --test local_mailspace_cli` | 53 passed | 1.4 s |
| `cargo test --test skills_cli` | 6 passed | 0.2 s |
| `cargo check --all-targets` | rc 0 | 14.9 s |

`cargo check --all-targets` warnings are pre-existing in files this branch does not touch
(`pump_note.rs:30` deprecated `fetch_update`, dead code in `pump/amend.rs`/`pump/collect.rs`,
`pool/claim_test.rs:108` unused `Result`) — sideband, outside the need.

## 7. Reader completeness — PASS

The only production occurrence of any of the seven header names is the refusal list at
`src/letter/message.rs:15-21`. The fold (`property.rs`), `sync/link.rs`, memo tag search
(`memos.rs:22-31`), and rule readers (`rules.rs:523-543`) all read front matter via `of_message`,
with legacy letters degrading to defaults (tags empty, project scope, no verdicts). Event
protocol headers (`X-Vivi-Event*`, slug/unit/dep/state payload headers) stay — ruling 6.

## 8. Docs and skills (ruling V4)

`skills/vivi/SKILL.md` (letters section rewritten: every letter format-1, front-matter facts, the
seven headers named as gone, refusal + legacy behavior; rules `--local` now documented as
`scope = "local"` front matter), `skills/vivi/references/envelope.md` (every letter kind listed as
format-1), `README.md` updated. `cargo test --test skills_cli` green. Descriptions match shipped
behavior as verified above.

## Findings

- **P1**: none.
- **P2**: none.
- **P3-1** — `src/mailspace/writers_test.rs:239-264` pins format header + moved-header absence per
  family but does not itself call `of_message` per family; the round trips live in the companion
  tests (reply/delivery/memos/rules_cli/property_emit/tests.rs) and every composer parse-checks at
  write time, so coverage exists — consolidating the round trip into the pin would make it
  self-contained.
- **P3-2** — sync `file_record` has no letter-format check yet, so a malformed format-1 letter
  pulled from a peer is stored unread rather than refused; quarantine is W3-B5's admitted scope
  (goal §3.2) — cross-need boundary note, no action in B4.
- **P3-3** — a legacy rule letter with `X-Vivi-Rule-Scope: local` reads as project scope on a
  fresh parse (`rules_test.rs` pins this as intended under recommendation 1); item state is
  unaffected and only fresh reads lose the local mark. Settled behavior, recorded for the ledger.
- **P3-4** — pre-existing compiler warnings in untouched files (`pump_note.rs:30`,
  `pump/amend.rs:33,52`, `pump/collect.rs:24,70`, `pool/claim_test.rs:108`) — sideband.

## VERDICT: CLEAN
