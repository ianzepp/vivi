# W3-B4 audit — vivi-server half

Independent read-only audit of need branch `need/w3-b4` at
`238b177` (merge of W3B4S-2). Base `99006cd` (vivi-server 2.0.0).
Task commits: `08effa7` W3B4S-1, `a64926b` W3B4S-2. The auditor did not
implement this need.

Pin: git-ignored `.cargo/config.toml` patches vivi to
`/Users/ianzepp/work/ianzepp/vivi-server-w3/vivi-pin` at `ac02d796`
(vivi B4, “every letter through the gate”). `Cargo.lock` is dirty in
the worktree from that patch and is not in `99006cd..238b177`. Not
staged. No repo file was edited by this audit.

Authority: `docs/factory/wave3-completion-goal.md` §3.1 (vivi-server
paragraph), §3.8, §6.2 W3-B4 server probe, §5 item 6 (CLEAN / RESIDUAL /
BLOCK; P1/P2 block the need, P3s go to the ledger).

## Verdict

RESIDUAL. No P1. No P2. Four P3s below. The durable done-when probe is
met and pinned.

## 1. Done-when

### Durable push without `X-Vivi-Format` → 422 `letter_format`

`ProjectStore::append` replays first, then `check_letter_format`
(`src/projects.rs:1360-1366`). `LetterError::Legacy` becomes
`ProjectError::LetterFormat` with the exact string
`the server accepts only format-1 letters: this one has no X-Vivi-Format header`
(`src/projects.rs:1035-1041`). The HTTP mapper is 422 `letter_format`
(`src/projects_api.rs:84`).

Pinned by `a_legacy_letter_without_a_format_header_is_refused`
(`tests/projects_api.rs:684-725`): status 422, code `letter_format`,
that exact message, the log stays empty, and the same bytes again are
still a refusal rather than a duplicate. A headerless letter
(`eml_no_format`) is refused the same way. The test also checks
`of_message` returns `LetterError::Legacy` on the fixture, so the
server message is the replacement, not the parser text.

Nothing is stored on that path: the check runs before `process_vouch`
and `file_letter` (`src/projects.rs:1364-1375`).

### Every server-composed letter is format-1

Composers in `src/`:

| Writer | Format header | This need |
| --- | --- | --- |
| `compose_send` / `compose_reply` (`src/mcp/letters.rs:190-301`) | `set_format_header` when `gate` sets `formatted` | unchanged (C3). Memo, mail, chat, rule, and work all set `formatted: true` in the pin’s `letter::gate`. |
| `compose_event` (`src/mcp/letters.rs:318-418`) | unconditional `X-Vivi-Format: 1` at line 387, then `of_message` at line 412 | new. Close facts are `CloseFront` via `render` for `done`/`reopened` (`CLOSE_EVENTS`); any other verb would be `EmptyFront`. |
| durable `save_memo` (`src/mcp/tools.rs:802-822`) | renders `MemoFront`, then `compose_send(..., "memo")` | new. |
| hosted `save_memo` / `move_item_outcome` | library at the pin (`delivery/memo.rs`, `item_events/emit.rs`) | arguments passed through. |

`compose_event` parse-checks the bytes it is about to store. The pin’s
`of_message` refuses a format-1 letter that carries any of the seven
moved headers (`vivi-pin/src/letter/message.rs:14-22,108-112`), so a
writer that put one back would fail at compose time.

Send/reply did not regress. Still green on this tip:
`a_durable_chat_send_carries_the_format_header`,
`a_durable_reply_carries_the_format_header_when_its_body_is_prose`,
`a_sugared_send_builds_front_matter_in_a_durable_project`,
`a_hosted_send_stores_a_format_one_letter_with_done_when`,
`a_need_without_front_matter_or_done_when_is_refused_with_vivis_message`.
Rooms still compose through `compose_send` / `compose_reply`
(`src/rooms.rs:437-440`); `tests/rooms.rs` is green and was not rewritten.

Grep of `src/` for `X-Vivi-Verdict`, `-Repo`, `-Branch`, `-Commit`,
`-Tag`, `-Rule-Scope`, `-Supersedes`: no matches. No moved header is
written by server code.

### Smokes

Not re-run here (not in the required command list). Script review only.
Parent’s claimed counts (suite rc 0, hosted 23/0, psync 84/0, 2co 46/0,
mcp 10/0) were not re-measured. The script edits are header additions
plus the vouch workaround in §3; they do not drop the old assertions
(idempotency 201/200/200/409, empty 400, at-cap 201, over-cap 413,
lying envelope, forged From).

## 2. Correctness

### Done-tool close metadata: durable vs hosted

Both tools take `verdict`, `branch`, `commit`, and `repos` (`src/mcp.rs:453-469`).
`additionalProperties` is descriptive only; dispatch does not validate
the schema, so the Rust checks are the gate.

Shared parser `close_arguments` (`src/mcp/tools.rs:1007-1041`): `Ok(None)`
when all four are missing or null; otherwise a `CloseFront`. `repos`
must be an object of strings. Branch/commit pairing and blank facts are
left to `CloseFront::validate` in the pin (`plain.rs:75-93`).

Durable (`src/mcp/tools.rs:435-472`):

- `reopen` with any close argument is refused:
  `reopen takes no close metadata: the facts belong to the done event`.
- A caller whose mailspace prefix does not own the item, and who passed
  close arguments, is refused:
  `close metadata rides the stamped event letter; this caller files a report reply`.
  `hand_done_is_a_report_not_a_close` still pins the no-metadata report
  path (mode `reported`, item stays open). The with-metadata refusal is
  untested (P3-4).
- An owner’s `done` calls `compose_event` with the front (or
  `CloseFront::default()`). `reopen` passes the default. An empty front
  stores the note bare: `render` returns the prose when the TOML is
  empty (`pin plain.rs:125-127`). Pinned by
  `done_composes_a_format_one_event_letter_with_close_front_matter`,
  including the branch-without-commit refusal
  (`` `branch` and `commit` are set together or not at all ``) before
  the successful close, the seven-header absence check, `of_message`
  round-trip, and a fenceless reopen whose prose is `reopened`.

Hosted (`src/mcp/live/write.rs:144-176,187-203`):

- Same reopen refusal, before the library call.
- `done` maps the front to the library’s metadata map (`verdict`,
  `branch`, `commit`, `repo:<name>`) and calls `move_item_outcome`.
  It does **not** refuse a non-author. That is the right split, not a
  bug: the pin’s assignee `done` is a local report whose note carries
  `[close]` (`delivery/moves.rs:331-341`, `try_emit_for_move` returns
  `Ok(None)` from `refuse_assignee_reopen` for a non-owner done).
  Durable `compose_reply` cannot carry that front, so dropping the
  arguments would be a silent loss; refusing is the honest durable
  behaviour. Hosted passing them through matches the CLI.
- Pinned for the author/mind path by
  `done_and_reopen_move_the_item_as_the_cli_does`: format-1, verdict /
  branch / commit / `repos.vivi` in the front, no `X-Vivi-Verdict` /
  `-Branch` / `-Commit`.

### Refusal precedence (durable append)

Order in `append` / `AppendItem::new`:

1. Empty body → 400. Over `BODY_CAP_BYTES` (64 KiB, strict `>`) → 413
   `body_too_large`, inside `AppendItem::new` (`src/projects.rs:290-295`,
   `src/safety.rs:100-101`), before any format judgement. Preserved by
   `size_cap_and_input_validation` and the project-sync smoke (over-cap
   is still raw 65537 `X` bytes and must be 413).
2. Hosted projects return at `append_hosted` (`src/projects.rs:1350-1351`)
   and are not format-checked. Contract § “Project letter append” says
   hosted “is not covered here” (`docs/contract.md:306-308`). B5 owns
   hosted bad-format refusal; B5 also files legacy as history. Out of
   this need’s probe, which is the durable push.
3. `find_replay` before the format check. Same id and same bytes →
   duplicate. Same id and different bytes → conflict, even when the new
   bytes would themselves be a format refusal. Moved, not dropped:
   `a_format_one_letter_that_parses_is_accepted`
   (`tests/projects_api.rs:670-678`).
4. Fresh letter: legacy → the fixed message; any other `of_message`
   error → `error.to_string()` (the parser’s message). Unknown key,
   missing `done_when`, and format `2` stay pinned by
   `a_broken_or_unknown_format_claim_is_refused_and_stores_nothing`,
   and the helper compares the HTTP message to the parser string.
5. A refused letter is not a vouch: `process_vouch` is after the check.
   Malformed format-1 vouches are still 400 from the vouch parser, and
   a later good vouch is still sequence 1
   (`vouch_wire_admits_refuses_malformed_and_conflicts_without_storing`).

A legacy letter already on disk is not re-judged: replay returns before
the check. The contract says so (`docs/contract.md:354-355`). There is
no HTTP test that injects one past the gate; the code order is the same
path the conflict test pins. Not filed.

### Memo tags: string or list

Schema `oneOf` string | array (`src/mcp.rs:488-494`). Both paths use
`string_list` (`src/mcp/live/read.rs:34-54`): a string is split on comma
or whitespace; an array is trimmed strings. Durable then
`normalized_memo_tags` (`src/mcp/tools.rs:1043-1056`); hosted relies on
the same rule inside `Mailspace::save_memo`. Empty or whitespace tags
are refused. A whitespace-separated string becomes several tags rather
than an error, which is `string_list`’s existing marks rule, and no
stored tag contains whitespace.

Tags beside a body that `opens_with_fence` are refused on the durable
path only (`src/mcp/tools.rs:513-521`), matching the send-tool sugar
rule. Hosted does not (P3-3).

Array form is pinned on both sides
(`a_memo_save_is_a_format_one_letter_with_tags_in_front_matter`,
`reads_reflect_what_the_library_wrote`). The string form is not (P3-4).
An untagged durable save still goes through `compose_send` and is
exercised by `memo_is_caller_local_on_the_server` (it stored); that test
does not assert the format header or the absent fence. Empty `MemoFront`
renders as bare prose, and `gate("memo")` sets `formatted: true`, so the
header is written. The reopen test is the one that pins “no `+++`”.

## 3. Tests were not weakened

`git diff 99006cd..238b177 -- tests/` moves behaviour the ruling removed
and keeps the old assertions on the new letter shape.

- The C3 test `a_legacy_letter_without_a_format_header_is_still_accepted`
  was renamed to `…_is_refused`. Its own pre-change comment said W3-B4
  would move it. The move matches §3.1: legacy is no longer filed. The
  “replay runs first, even if the new bytes are a format refusal”
  assertion was kept, on a stored format-1 letter instead of a stored
  legacy one (you can no longer store a legacy letter to conflict
  against). That is the same precedence.
- `graph_blocks_on_an_open_dependency` no longer files a legacy
  `X-Vivi-Depends-On` letter and then asserts the graph ignored it. It
  asserts 422 `letter_format`, and the front-matter dependency case is
  unchanged. The “stored legacy contributes no deps” rule is still
  pinned at the function by `graph_dependencies_come_from_front_matter`
  (`src/mcp/tools.rs:1227-1229`), which does not go through append.
  `depends_of` (`src/mcp/tools.rs:705-718`) still returns nothing when
  `of_message` fails.
- `tests/projects.rs` event fixtures gained `X-Vivi-Format: 1` and still
  file as mail, not work. The decoy that only mentions `X-Vivi-Event`
  in the **body** is still a body line
  (`tests/projects.rs:291-298`) and is still counted as work
  (`assert_eq!(needs, [item_id, decoy_id])`). Not turned into a real
  event header.
- Admissions / hosted / projects_api `eml` helpers and vouch bodies
  gained `X-Vivi-Kind` + `X-Vivi-Format: 1` so the new gate does not
  steal the assertion. Malformed vouches still expect 400, and the
  comment says why: they are format-1 first, so the vouch parser is
  what refuses them. Non-UTF-8 and “no blank line” cases still pass
  (admissions 10/10, the vouch wire test green).
- Byte-exact round trips (non-UTF-8, NUL, CRLF, no trailing newline)
  keep the same payload bytes; only the header block gained the two
  format lines. Hash and served-bytes assertions are unchanged.
- `assert_letter_format` still requires the HTTP message to equal the
  parser’s, and still refuses to be used on a legacy letter. The
  failure text “legacy must not be refused” is stale wording (P3-4),
  not a weakened pin. The legacy pin is the other test.

### W3B4S-2 scripts

`scripts/project-sync-smoke.sh`: every hand-rolled letter gained
`X-Vivi-Kind: mail` and `X-Vivi-Format: 1`. The 64 KiB case builds a
real format-1 letter and truncates to exactly 65536 bytes (header is
~70 bytes, so the blank line survives). The assertion is still
`C_AT = 201` and `C_OVER = 413`. Over-cap never reaches the format
check. Idempotency, empty, forged From, and the lying envelope keep
their old codes.

`scripts/two-checkout-smoke.sh:206-240`: `vivi sync vouch` is no longer
called. The comment is true at this pin. `compose_vouch`
(`vivi-pin/src/sync/vouch.rs:60-72`) writes governance mail with no
`X-Vivi-Kind` and no `X-Vivi-Format`. This server now 422s that letter,
so beta/gamma would never be admitted and the later `sync api-key mint`
would fail for lack of membership. The workaround composes the same
JSON vouch body project-sync already posts, plus the two headers the
gate requires, with A’s real bearer (`sync key` + the challenge the
script already knows how to sign). `sync key`, `sync join`, and
`api-key mint` stay the real verbs. From is `alpha@smoke.local`, not the
mailspace domain; admission uses the authenticated name
(`process_vouch` reads `item.from`), which is the same shortcut
project-sync has always used. The workaround is honest and minimal for
a pin that does not contain W3B5-5. It will be wrong once the pin’s
`compose_vouch` writes format-1: restore `vivi sync vouch` then. That
is a pin follow-up, not a defect in this branch.

## 4. Docs (§3.8)

- `docs/contract.md` letter section (`:306-355`) and the `done` / `memo`
  / `graph` tool rows (`:423-426`) match the code: format-1 on composed
  letters, close facts and memo tags in front matter, the seven headers
  named as not written, legacy durable push refused with the exact
  message, replay not re-judged, already-stored legacy stays filed and
  contributes no dependencies. The section is explicitly durable-only,
  which matches `append`’s hosted early return.
- `CHANGELOG.md` Unreleased matches that contract, including “a legacy
  letter already stored stays filed.”
- `README.md` and `AGENTS.md` do not describe letter format, memo tags,
  or close metadata. Nothing in them contradicts the change. No edit
  was required. `AGENTS.md` already points wire truth at `docs/contract.md`.
- The item-events “On the wire” section (`docs/contract.md:554-589`)
  still describes the headers that stay (`X-Vivi-Event` and friends).
  It does not claim verdict/tag live in headers. The letter-format
  section is where the move is stated. No contradiction.

## 5. Hygiene

This repo does not enforce vivi’s rules. `AGENTS.md` has no file or
function ceiling and no ban on inline tests. `src/mcp/letters.rs`,
`src/mcp/tools.rs`, `src/crypto.rs`, and `src/ids.rs` already contain
`#[cfg(test)]` modules. New behaviour is tested from `tests/`, not from
new inline modules.

`src/mcp/letters.rs` is 2016 lines. The inline test module starts at
line 1012, so production is about 1011 lines. `99006cd..238b177` changes
that file by about 28 lines (`compose_event`’s front matter and the
parse-back). The size is pre-existing. No ceiling fires. Filed as P3-1
because the implementer flagged it and the file is past a size anyone
will want to split, not because this need broke a rule.

`compose_event` is about 100 lines. Not enforced here.

`cargo fmt --check` in the worktree is clean.

## 6. What this audit ran

Worktree `/Users/ianzepp/work/ianzepp/vivi-server-w3/need-w3-b4`, tip
`238b177`, pin `ac02d796`.

| Command | Result |
| --- | --- |
| `cargo fmt --check` | rc 0 |
| `cargo test --test mcp --test projects_api --test mcp_hosted --test mcp_server --test rooms --test admissions --test hosted --test projects -- --test-threads=8` | rc 0, real 10.42s. 176 passed, 0 failed, 1 ignored (`mcp` `an_event_past_the_record_capped_first_page_is_folded_in_a_long_project`, pre-existing ignore). Per binary: admissions 10, hosted 10, mcp 32 (+1 ignored), mcp_hosted 29, mcp_server 17, projects 32, projects_api 38, rooms 8. |
| `cargo check --all-targets --quiet` | rc 0, real 11.52s. Warnings are all in the vivi pin (`pump_note.rs` deprecated `fetch_update`, dead `pump/amend.rs` / `collect.rs`). Held Q-A code, out of this need. None in vivi-server `src/`. |

Full suite not run. Smokes not run.

## Findings

### P3-1 — `src/mcp/letters.rs` is 2016 lines

Production ends at the `#[cfg(test)]` module on line 1012 (~1011 lines);
the rest is the pre-existing inline fold tests. This need added the
`compose_event` front-matter block (~28 lines in the diff stat). No
ceiling is enforced in this repo, so it is not a broken rule. It is the
split the implementer already called a P3.

### P3-2 — durable close-argument checks run after the idempotent short-circuit

`src/mcp/tools.rs:423-437` returns `already_done` / `already_open`
before `close_arguments`. A second `done` that carries a verdict, or a
`reopen` that carries close arguments while the item is still open,
succeeds as a no-op and drops the arguments. Nothing is written, and
the mode name is honest, so this is not a wrong letter on disk. Hosted
checks the arguments first (`src/mcp/live/write.rs:147-152`). Worth
moving the durable check above the short-circuit so a bad pair
(`branch` without `commit`) and a reopen-with-verdict fail the same way
whether or not the item is already in that state.

### P3-3 — hosted memo does not refuse tags beside a `+++` body

Durable refuses (`src/mcp/tools.rs:513-521`). Hosted calls
`save_memo` with the raw body (`src/mcp/live/write.rs:228-230`). The
library renders `MemoFront` and treats the caller’s text, fences
included, as prose. The stored letter is still format-1 and the
argument tags win; the caller’s own front matter becomes prose. That
matches the CLI and diverges from the durable sugar rule. Either teach
the hosted path the same refusal, or say in the contract that only the
durable memo tool rejects a fenced body.

### P3-4 — stale helper text, and two refusal branches untested

`assert_letter_format` (`tests/projects_api.rs:591-594`) still fails
with `legacy must not be refused` when its input is a legacy letter.
The helper’s job is “compare the HTTP message to the parser’s, so do
not use me for legacy,” and the real legacy pin is the test at line 684.
The message states the pre-B4 rule and will mislead the next editor.

Also untested, though the code is straight: durable non-author
`done` with close arguments (`src/mcp/tools.rs:467-471`), `reopen` with
close arguments on a closed item (`:438-441`), and memo `tags` passed as
a string rather than an array. The author/array paths are pinned.

## Not findings

- Hosted `append_hosted` still accepts a legacy letter. The probe and
  the contract’s letter section are durable-only. B5 files legacy as
  history and refuses a bad format-1 on the hosted path.
- Hosted non-author `done` with close arguments is passed to
  `move_item_outcome` rather than refused. The library writes those
  facts on the report note. Durable refuses because its report is a
  plain `compose_reply`. See §2.
- Pin warnings (`HASH_SCHEME`, `fetch_update`) are the held Q-A tree in
  the vivi pin, not this branch.
- Dirty `Cargo.lock` is the path patch. It is not in the commit range.

VERDICT: RESIDUAL
