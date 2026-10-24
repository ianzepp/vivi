# W3-B5 vivi-server audit (hosted refuse)

Independent audit of need branch `need/w3-b5` at `182564f` (merge of `8a73288` W3B5-6 onto `b5b69fe`). Read-only. No branch edits. Vivi pin `.cargo/config.toml` → `vivi-server-w3/vivi-pin` `634c5181` (not staged). Worktree has an unstaged `Cargo.lock` from that pin; it is not in `b5b69fe..HEAD` and was not touched.

The goal has no heading §5.6. The audit rule used here is §5 step 6 (`wave3-completion-goal.md:847-849`): correctness, done-when evidence, no test weakened, stale text; CLEAN / RESIDUAL / BLOCK with P1/P2/P3. Server done-when is the §3.2 hosted bullet, not the §6.2 probe (that probe is the client quarantine scenario).

## What landed

`b5b69fe..HEAD` is three files, `+150/-2`:

- `src/projects/hosted_index.rs` — after `file_records`, a non-empty `report.quarantined` is `quarantine::discard`ed and the reasons become `Filed.refused`.
- `tests/hosted.rs` — three new tests (bad format-1, good format-1, legacy).
- `docs/contract.md` — hosted push paragraph under Letter format (v1).

## 1. Done-when (happy path)

§3.2 done-when, server row: "vivi-server hosted push of a bad format-1 letter is refused and reported (test)."

`tests/hosted.rs:392-425` `a_bad_format1_letter_is_refused_at_push_and_nothing_is_filed_or_held` does what that row asks, and what the implementer claimed beyond it:

- Unknown front key `frob` on an `X-Vivi-Format: 1` need.
- `ProjectStore::append` returns `ProjectError::BadRequest`.
- Message starts with `letter refused: ` and contains `unknown field` and `frob` (the parser reason).
- Host mailspace `list_messages()` is 0.
- `quarantine::list` is empty and no file sits at the top of `.vivi/quarantine/`.
- `.vivi/quarantine/dropped/` has 2 entries (the `.eml` + `.json` pair).

`tests/hosted.rs:428-446` files a good format-1 need (`done_when` set): not a duplicate, one stored message.

`tests/hosted.rs:449-470` files a letter with no `X-Vivi-Format` header: append succeeds, one stored message. The comment states the hosted wire follows the filer, not the durable 422.

`ProjectError::BadRequest` is the existing mapper to HTTP 400 `invalid_request` (`src/projects_api.rs:80-81`). The test stops at the store error the handler returns. That is the same variant transport refusals already use (`src/projects/hosted_index.rs:392-401`, one `refusal()` for both). It does not hit the HTTP stack. That is enough for this done-when: the status mapping is not new.

I ran the tests (below). They pass. The happy path meets the done-when.

## 2. Correctness edges

### Discard failure, and a receipt that is already there — P1

`file_in_host` refuses only when **this** `file_records` call appended to `report.quarantined` (`src/projects/hosted_index.rs:371-385`). An empty list falls through to `report.refused` only.

The filer does not report a letter it already holds. `quarantine::hold` returns `false` when `<sha>.json` exists and writes nothing (`vivi` pin `src/sync/quarantine.rs:88-90`). `hold_letter` pushes `QuarantinedRecord` only when `hold` returns `true` (`vivi` pin `src/sync/filer.rs:171-176`). So a live receipt makes this push look like a clean file: `quarantined` empty, `refused` empty, `governance` 0, `existing` 0.

`append_hosted` then treats `filed.refused == None` as success and calls `project.record` (`src/projects/hosted_index.rs:296-306`). `record` inserts a transport row. It does not write a blob. The quarantine path never ingested one (`file_new` is not reached). `Project::letter` turns a missing blob into `ProjectError::Storage` (`src/projects.rs:882-887`). `read_page` uses `?` on `record_of` (`src/projects.rs:1507-1508`), so the whole list fails at that sequence.

Two ways the receipt is still live when the next push runs:

1. **`discard` returns an error.** The `?` at `hosted_index.rs:374-376` leaves `file_in_host` before any refusal is built. The member gets `ProjectError::Storage`, which the handler turns into HTTP 500 `internal` / `storage failure` (`src/projects_api.rs:94-99`). The parser reason is not in the answer. The client's `classify_append` treats 500 as `AppendFailure::Stop`, not `Refused` (`vivi` pin `src/sync/http.rs:264-278`). Stop is retried on the next push. That retry is the silent-hold path above: 201, transport row, quarantine pair still in the live directory.
2. **Crash after `hold` commits the receipt and before `discard` renames it.** Same retry shape. `hold` writes the `.eml` and the `.json` before `file_in_host` reaches `discard`. The append comment above this path says a crash between the two hosted writes converges (`hosted_index.rs:260-261`). This third write does not.

A discard that fails **after** moving the `.json` into `dropped/` is recoverable: the live receipt is gone, so the retry holds again and refuses. The bad case is a receipt that is still live.

What the member is told, and whether the hold leaks:

| Path | Member told? | Hold |
| --- | --- | --- |
| `discard` ok | Yes. 400 `invalid_request`, `letter refused: <parser reason>`. | Moved to `dropped/`. Not listed. Not indexed. |
| `discard` errors, or crash before it | No. 500 `storage failure` (or a dropped connection). | Live pair remains. |
| Next push while that receipt is live | Yes, but the wrong answer: 201 Created. Client binds the letter as sent. | Still live, and a transport row now points at a blob the store does not have. A later pull that reaches that sequence fails the page. |

That breaks the settled recommendation in §3.2: refuse rather than quarantine, because a server copy must not hold an unreadable letter. The happy path does refuse. The error path the new `?` claims to handle, and the retry the contract tells the member to do, accept the letter and keep the hold.

Fix direction (not applied): a live receipt for this content is still a refusal, including when `hold` returns false and `report.quarantined` is empty. Discard it (again) and return `refusal` with the receipt's reason. Do not `record` a row whose bytes were not filed. A discard error must not be the only signal; if the receipt is still live the answer is still a refusal, not a storage failure the client will retry into a 201.

### Repeated push after a successful discard — correct, untested

`discard` moves the pair under `dropped/` (`vivi` pin `src/sync/quarantine.rs:166-180`). `hold` only looks at the live `<sha>.json`. A second push of the same bytes holds again, `report.quarantined` is non-empty again, discard overwrites `dropped/` (the function's own contract), and `refusal` runs again. `tail()` is read, not reserved, and `record` is not called, so the sequence is not consumed. No transport row, so `find_replay` does not turn the second push into a duplicate success.

That is the idempotent re-refusal. No test pushes the bad letter twice. P3.

### Sync cursor — the hosted path does not advance it

The prompt's premise ("the filer advanced the cursor when it held") is the client pull, not this call. `file_records` documents that it does not touch the cursor or the floor (`vivi` pin `src/sync/pull.rs:411-429`). `handle_page` advances only on a pull page, and only after `file_batch` returns (`pull.rs:395-396`). `hold_letter`'s comment that the cursor has already advanced (`filer.rs:159-161`) describes that pull caller, and even there `advance` is the next step, not something that ran before `hold`.

A quarantined record returns before reply linking (`filer.rs:136-137`), so it does not add a dangling parent. `state != before` is false for this letter; the save at `hosted_index.rs:362-366` does not run. A later push is not skipped by the cursor. It re-enters the gate. After a successful discard that re-holds and re-refuses, which matches "no wire residue": nothing was `record`ed, so another member's pull does not see the letter. The `dropped/` pair is on-disk residue of `quarantine::discard`, same as the client `drop` verb §3.2 names. `list` does not recurse into it. It is not served and not refiled. The test pins that pair (`tests/hosted.rs:421-424`). That is the discard API, not a second hold. The contract's "never stored" means not filed and not left listed; the next sentence says the pair is discarded. Read together, they match the test. Not a defect.

### Answer shape vs transport refusals — same shape

Both go through `refusal` (`hosted_index.rs:392-401`):

- Ordinary reason → `ProjectError::BadRequest` → 400 `invalid_request`, body message `letter refused: <reason>`.
- Reason containing `already names different content` → 409 `conflict`. A parser reason does not contain that phrase.

Quarantine puts the parser `Display` in that reason (`front matter line N: …`). One hosted record cannot be both quarantined and transport-refused; the match's quarantine arm is the one that runs. Joining several reasons with `"; "` does not matter: `file_in_host` is called with one record.

Durable bad format is a different answer on purpose: `ProjectError::LetterFormat` → 422 `letter_format`, parser text, no `letter refused:` prefix (`src/projects.rs:1035-1043`, `src/projects_api.rs:83-84`). 422 would be `AppendFailure::Stop` on the client (`http.rs:276-278`), so the member would retry forever instead of learning. 400 is the shape that reports the refusal. The contract says "a different answer" and does not name the status code. The test locks the `BadRequest` variant and the prefix. Consistent.

## 3. Legacy hosted-files-as-history vs durable 422

Pinned in both places:

- Test: `tests/hosted.rs:449-470` (no `X-Vivi-Format`, append succeeds, one message).
- Contract: `docs/contract.md:365-368` (legacy is history on the hosted wire; the durable `letter_format` refusal does not reach it).

`of_message` returns `LetterError::Legacy` as soon as the header is absent, without parsing the body (`vivi` pin `src/letter/message.rs:94-103`). The filer files that and quarantines every other `LetterError` (`filer.rs:133-137`). The hosted test uses a well-formed front without the header; the code files any header-less letter, garbage body included. That is the §3.2 rule ("`LetterError::Legacy` → file as history"), not an accident of the fixture.

Does any goal line contradict it?

- §3.2 target (`wave3-completion-goal.md:560-574`) is the authority for this need. Legacy files as history. The hosted recommendation changes only the fate of a **quarantined** letter (unreadable format-1): refuse and do not keep a server copy. It does not say to 422 legacy on the hosted wire.
- §3.1 (`:503-504`) says `ProjectStore::append` refuses legacy too. Its done-when (`:542-543`) narrows that to **durable** `ProjectStore::append`. The code matches the done-when: `append` returns at `src/projects.rs:1350-1352` before `check_letter_format` (`:1366`), and the comment at `:1344-1346` says hosted is not checked there.
- §6.2's W3-B5 probe is the client quarantine flow. It does not require a hosted 422 for legacy.

No goal line requires hosted legacy to be refused. The §3.1 sentence is broader than its own done-when; §3.2 plus the contract are the ruling for this branch. The split is not a defect.

## 4. Tests and conventions

`git diff b5b69fe..HEAD -- tests/` is `tests/hosted.rs` `+112/-0` only. No assertion removed, no `#[ignore]`, no expected-output edit. `tests/hosted_index.rs` is unchanged, including `a_forged_from_is_refused_and_nothing_is_filed_or_indexed`, which does assert the served page is empty. The new refuse test does not. P3.

No new dependency, no `anyhow`, no production `unwrap`. `file_in_host` is `hosted_index.rs:340-390` (51 lines). The file is 538 lines. Comments state the invariant the code is implementing. `Cargo.lock` and `.cargo/` are not in the commit.

Coverage gaps (not happy-path failures):

- No second push of the same bad letter (the successful-discard re-refusal above).
- The refuse test does not assert an empty transport page. A row recorded before the error return would still pass `list_messages() == 0`.

## 5. Commands run (this audit)

Worktree `/Users/ianzepp/work/ianzepp/vivi-server-w3/need-w3-b5`, tip `182564f`. Target directory already warm.

| Command | Result | Wall |
| --- | --- | --- |
| `cargo fmt --check` | exit 0 | 0.15 s |
| `cargo test --test hosted --test hosted_index` | exit 0. hosted 13 passed, 0 failed. hosted_index 10 passed, 0 failed. | 1.03 s (already built) |
| `cargo check --all-targets` | exit 0. `vivi` pin warnings only (dead code / deprecated `fetch_update` in `pump/`). | 11.53 s |

Full suite and the smokes were not run (out of this audit's brief). Parent's "hosted smoke 23/0, project-sync 84/0" was not re-checked.

## Findings

**P1.** `src/projects/hosted_index.rs:371-377` and `:296-306`. A live quarantine receipt (`hold` returns false, or `discard` errors before removing it) is not a refusal. The member is not told the parser reason (500, or a dropped connection). The next push is indexed with no blob and the hold stays. A pull that reaches that row fails the page (`src/projects.rs:882-887`, `:1507-1508`).

**P3.** No test for a second push of the same bad letter after a successful discard, and `tests/hosted.rs:392-425` does not assert the transport page is empty.

No P2.

VERDICT: BLOCK

---

## Pass 2 (tip `65ea1f1`)

Re-audit after the P1 fix. Net `b5b69fe..65ea1f1` is still only `docs/contract.md`, `src/projects/hosted_index.rs`, and `tests/hosted.rs`. The accidental `need/w3-d4` merge is fully undone by `2d4eb6a` (name-status against `b5b69fe` shows no D4 files). Pin `.cargo/config.toml` → `vivi-server-w3/vivi-pin` `4b8186be`, which contains `c28474b2` and `64c251f7`. `cargo test` compiled `vivi` from that path. Unstaged `Cargo.lock` was not touched.

### P1 closed

Two pieces, both required.

**Filer (vivi `64c251f7`).** `hold_letter` now pushes a `QuarantinedRecord` whether `hold` newly wrote the receipt or returned false (`vivi-pin` `src/sync/filer.rs:173-182`). The already-held reason is `quarantine::reason_of` (`src/sync/quarantine.rs:154-160`): the live receipt's reason, or `"held (receipt unreadable)"` when the receipt is missing or corrupt. A quarantined letter is not a live message (`filer.rs:128-137`), so a later `file_records` of the same bytes enters `hold_letter` again. A plain re-pull stays silent because the cursor is already past the record and the page does not contain it (`quarantine_test.rs:139-142`). A resync re-reads and names the letter again (`:144-175`). That split is the ruled one, and hosted pushes go through `file_records`, which does not advance the cursor, so every hosted retry is a fresh arrival.

**Server (`216c996`).** `file_in_host` sets `Filed.refused` whenever `report.quarantined` is non-empty (`src/projects/hosted_index.rs:376-380`). `refuse_held` (`:390-403`) logs a discard error and still returns the receipt reasons. `append_hosted` returns `refusal` before `project.record` (`:299-303`). `BadRequest` is still HTTP 400 `invalid_request`, not 500.

| Entry | What happens now |
| --- | --- |
| `discard` errors (first push) | `hold` returned true, so `quarantined` is non-empty. The member gets `letter refused: <reason>`. No transport row. The receipt stays live. Tested: `tests/hosted.rs:611-638` (`dropped/` is a file, so `create_dir_all` fails). Message equals `letter refused: ` plus the live receipt's reason. Page empty. |
| Crash after `hold` commits and before `discard`, then retry | No row was written (`record` is after the refusal return). The retry finds the live receipt, `hold` returns false, the filer still reports `quarantined` with `reason_of`, and the server refuses before `record` again. Same shape if the second `discard` also fails. |
| Second push after a successful discard | Receipt is under `dropped/`, so this push holds again, refuses again, and overwrites the one pair. Tested: `tests/hosted.rs:565-594`. |

A missing blob can no longer be indexed by this path: the refusal returns first, and `read_page` would error the page if a row existed (`src/projects.rs:1442`). The new assert (`tests/hosted.rs:422-440`) is not a false green. A need has `kind IS NOT NULL`, which `visible_clause` shows to every member (`src/projects.rs:619-620`), and a row with no blob panics the test instead of looking empty.

The old P3s are closed. `pushing_the_same_bad_letter_again_refuses_it_and_keeps_one_dropped_pair` is the second push. `assert_transport_page_empty` (empty records, `has_more` false, `next_cursor` 0) is used by the bad-letter test, the second push, `a_refused_push_leaves_the_transport_page_empty`, and the discard-failure test. No assertion was weakened. Good-letter and legacy tests are unchanged.

The live receipt left behind when discard cannot run is the specified degradation, not a reopen of the P1. It is not indexed, and every later push of those bytes is refused. It is not what `docs/contract.md:357-364` says.

### P3

`docs/contract.md:361-364` still says the quarantine pair is discarded and nothing is left held. `tests/hosted.rs:632-636` pins the opposite when discard fails: the receipt stays live. The code comment at `hosted_index.rs:372-375` is the accurate rule. One sentence in the contract should say a failed discard leaves the receipt and the push is still a 400.

No server test pushes a second time while that receipt is still live. The retry is closed by the filer test `already_held_content_arriving_again_is_named_from_its_live_receipt` (`quarantine_test.rs:201-232`) composed with `refuse_held` ignoring discard's `Err`. Not a second P1.

No P1. No P2.

### Commands (pass 2)

| Command | Result | Wall |
| --- | --- | --- |
| `cargo test --test hosted --test hosted_index` | exit 0. hosted 16 passed, 0 failed. hosted_index 10 passed, 0 failed. | 4.45 s (rebuilt `vivi` from the pin) |
| `cargo check --all-targets` | exit 0. Same pin warnings as pass 1. | 3.18 s |

VERDICT: RESIDUAL
