# W3-C3 audit — every letter the server composes is a v1 letter

verdict: RESIDUAL

- vivi-server: `need/w3-c3` @ `6cde814` (base `main` @ `5989c26`), worktree
  `~/work/ianzepp/vivi-server-w3/need-w3-c3`, patched to the vivi pin @ `f289ace8`.
- vivi: local `main` @ `f289ace8` (vivi half, C3 commit `75ddb4a7`). Read-only.
- Authority: `vivi/docs/factory/wave3-completion-goal.md` §2.4.
- P1: 0 · P2: 0 · P3: 2 (+1 observation). Nothing blocking merge.

## Findings

### P3-1 Composer variant without a raw-bytes test: chat reply

`compose_reply` gates the body as `"chat"` when the parent is a chat letter
(`src/mcp/letters.rs:239-244`, moved in this branch) and then writes the format
header through the same `set_format_header` helper. No test reads the raw bytes
of a chat reply: `tests/mcp.rs:510-519`
(`a_durable_reply_carries_the_format_header_when_its_body_is_prose`) replies to a
**need**, so it pins the mail branch; `tests/rooms.rs` (the chat-reply wiring at
`src/rooms.rs:431-437`) asserts nothing about stored bytes. The goal's
done_when asks "a test per composer reads the raw bytes" and lists Rooms chat
as a composer row. Risk is small — the `"chat"` prose branch of `letter::gate`
is pinned in vivi (`letter::gate_test::a_prose_chat_passes_as_written_and_formatted`)
and the durable chat **send** pins `set_format_header` end to end
(`tests/mcp.rs:488-499`) — but the chat-kind choice inside `compose_reply` is
unverified at byte level. One rooms or mcp test replying to a chat letter and
reading `X-Vivi-Format: 1` closes it.

### P3-2 Touched file outside the goal's declared file list

`scripts/two-checkout-smoke.sh:251-252` (commit `e2f9f6f`, W3C3S-C) rewrites one
comment: "the letter's X-Vivi-Depends-On header" → "the letter's front matter
(`depends_on`)". §2.4's Files list names src/docs/tests paths only, not
scripts/. The change is comment-only, factually required by the header's
removal, and the script's behavior is unchanged. Not a `src` scope leak
(audit item 7's literal trigger), but it is a deviation from the declared list.

### Observation O-1 vivi `src` still greps 5× for `X-Vivi-Depends-On`

The audit brief said "none expected" in vivi; the goal's own done_when only
requires vivi-server clean and no vivi reader left. All 5 hits are inline test
modules, each legitimate:

- `vivi/src/mailspace/dependencies_test.rs:553` — fixture where front-matter
  `depends_on` wins over a stray legacy header (pins the new rule).
- `vivi/src/mailspace/dependencies_test.rs:566` — legacy letter names no refs.
- `vivi/src/mailspace/delivery/compose_test.rs:54,112` — negative assertions
  (`assert!(!eml.contains("X-Vivi-Depends-On"))`).
- `vivi/src/sync/refs_test.rs:64` — legacy header is no longer a reference.

No production reader or writer of the header remains in either repo. Not a
defect.

## Checks

### 1. done_when greps — PASS

- vivi-server (`need/w3-c3`): `grep -rn "X-Vivi-Depends-On" src` → **0 hits**
  (rc 1). Stronger than the done_when (which allowed legacy test fixtures in
  `src`). Remaining repo-wide hits are all legitimate: negative assertions
  (`tests/mcp.rs:481,499,519,543,701`, `tests/mcp_hosted.rs:299`), one legacy
  fixture + comment (`tests/mcp.rs:692-705`), and a historical audit doc
  (`docs/audit-command-tree-2026-10-11/audit-work.md`). rc 0, <1 s.
- vivi (`main` @ `f289ace8`): 5 hits, all inline test modules — see O-1.
  rc 0, <1 s.

### 2. Composer coverage — PASS (one P3 gap)

Every row of §2.4's composer table, code read + raw-bytes pin located:

| Composer | Gate path (read) | Raw-bytes pin |
| --- | --- | --- |
| Hosted send | `src/mcp/live/write.rs:60-66` `sugared_body` → vivi `Mailspace::send` → `letter::gate` | `tests/mcp_hosted.rs` `a_hosted_send_builds_front_matter_from_the_sugar_arguments` (asserts `X-Vivi-Format: 1`, `done_when = [...]`, `target = "repo"`) |
| Hosted reply | `Mailspace::reply` → vivi gate (no server change needed) | vivi `src/mailspace/delivery/compose_test.rs:250-267` `a_prose_reply_is_stored_as_written_and_formatted` |
| Hosted done/reopen/mark/unmark/depend/memo, pump_send | vivi writes them (events/memos are B4's) | exempt per §2.4 table |
| Durable/chat send | `src/mcp/tools.rs:338-342` → `letters.rs compose_send` → `gate_body` (`letters.rs:175-178`), `set_format_header` (`letters.rs:186-193`) | `tests/mcp.rs:454-483` (sugar need), `485-499` (chat) |
| Durable/chat reply | `compose_reply` gates before compose (`letters.rs:239-244`) | `tests/mcp.rs:501-519` — **mail branch only; chat branch = P3-1** |
| Durable event | `compose_event` untouched (`letters.rs:312`) | exempt until W3-B4 ✔ |
| Durable memo save | `compose_send(kind "memo")`; `gate("memo", …)` falls to the pass-through arm (`vivi src/letter/gate.rs:50-56`) → `formatted: false` → no header | `tests/mcp.rs:521-543` `a_memo_letter_keeps_its_shape_with_no_format_header` (asserts `X-Vivi-Kind: memo`, `!contains X-Vivi-Format`) |
| Rooms chat | `src/rooms.rs:437,440` reuse `compose_send`/`compose_reply` (1-line signature change) | send leg via shared-fn pin above; reply leg = P3-1 |
| Transport graph reader | `src/mcp/tools.rs:671-686` `depends_of` reads front matter via `of_message`; legacy (`LetterError::Legacy`) and broken claims contribute none, no error | inline `tools.rs:1080-1137` `graph_dependencies_come_from_front_matter` (need/task/want + legacy + broken + unknown-format) and `tests/mcp.rs:667-770` end-to-end (blocked_by, legacy ready, close unblocks) |
| Durable member push | `src/projects.rs:1031-1038` `check_letter_format` (accepts `Legacy`, refuses claims that fail to parse or name an unknown format), called at `projects.rs:1360` after `find_replay`, before the vouch | `tests/projects_api.rs:581-648` (422 `letter_format` × unknown key / no done_when / format 2, nothing stored, re-push still refused), `650-668` (parses → 201), `673-716` (legacy accepted, replay keeps duplicate/conflict) |
| Hosted member push | returns early at `projects.rs:1345` `append_hosted` | exempt until W3-B5 ✔ |

### 3. vivi half on main — PASS

- `vivi::letter::gate` is public (`vivi/src/letter.rs:7` `pub mod gate`),
  storage-free; `compose::gate` calls it and keeps only the storage step
  (`serves`/`explores` canonicalization + re-render;
  `vivi/src/mailspace/delivery/compose.rs:41-62`).
- Commit `75ddb4a7` is a pure move: the removed `work`/`checked`/`checked_prose`
  /`no_front`/`parse` bodies reappear in `src/letter/gate.rs` with identical
  text (the `no_front` strings are byte-identical). The old `deps` error arm
  ("not a work letter") was unreachable — `deps` ran only after parsing a
  Need/Want/Task — so dropping it is behavior-neutral. `rerender` narrowed to
  `Front::Need`, its only reachable case. No behavior change found.
- Gate unit tests per kind exist and pass: 14 in `src/letter/gate_test.rs`
  (need without fence / without `done_when` refused; want/task without fence
  refused; front-matter need formatted+parses; task deps extracted; prose
  mail/chat pass formatted; fenced mail with unknown key refused; action
  front; role reply envelope; pump record parses formatted; prose pump
  refused; unknown kind unformatted).

### 4. Cheap proofs re-run — PASS

All durations measured with `/usr/bin/time -p`, warm `target/`. Predictions
stated before each run: trio "<2 min", vivi gate "<1 min". Actuals well under.

- vivi-server `cargo test --test mcp --test mcp_hosted --test projects_api --
  --test-threads=4`: rc 0, real **8.70 s**. Per binary: mcp 31 passed / 1
  ignored / 0 failed (6.14 s); mcp_hosted 28 passed / 0 failed (1.33 s);
  projects_api 38 passed / 0 failed (1.13 s).
- vivi (main checkout) `cargo test --lib gate`: rc 0, real **10.48 s**, 128
  passed / 0 failed / 4021 filtered.
- vivi `cargo test --lib letter::gate` (exact module): rc 0, 14 passed / 0
  failed, <1 s after build.
- Release cross-check by name, vivi-server
  `cargo test --test mcp_hosted a_pumped_need_the_server_composed_starts_a_run --
  --exact`: rc 0, 1 passed, real 0.23 s. The test sends a need through hosted
  MCP `send` with the `done_when` sugar, files the pump record
  (`ms.pump_send`), runs one `vivi::pump::flow::tick::tick` over the hosted
  root, and asserts a LIVE run exists with no `flow-letter-invalid`.

### 5. No test weakened — PASS

`git log --oneline 5989c26..need/w3-c3` = 10 commits (A/B/C/D/E each with a
--no-ff merge). `git diff 5989c26..need/w3-c3 -- tests/` removes 10 lines; every
removal maps an old pin of removed behavior to the new ruled behavior:

- `tests/mcp.rs` `send_list_show_thread_done_graph`: body `"done_when: the
  connector works"` → sugar arg `"done_when"`; the raw-prose containment
  assertion became a front-matter assertion
  (`contains("done_when = [\"the connector works\"]")`) — stricter, pins the
  new wire.
- `tests/mcp.rs` `graph_blocks_on_an_open_dependency`: child `kind: "task"`
  with prose body → `need` with `done_when` + `depends_on` (a task without
  front matter is now refused by the gate — the repin is forced); assertions
  grew (blocked_by/depends_on equality, legacy-ready, close-unblocks,
  child_after depends_on).
- `tests/mcp.rs` `send_item` helper: adds `done_when: "it is done"` — forced
  by the gate.
- `tests/mcp_hosted.rs:233` `a_need_body_without_done_when_is_refused`:
  `contains("done_when")` → `contains("need has no done_when")` — stricter.
- `tests/mcp_hosted.rs:931` hosted transport-tools send: adds
  `done_when: "y"` — forced by the gate.

All other test changes are additions (durable sugar/chat/reply/memo/gate/whole-
letter tests; hosted sugar/depends/mail tests; projects_api 422 suite). No
assertion removed without a stronger replacement; no `#[ignore]` added.

### 6. Docs truth — PASS

`docs/contract.md` (diff +30/+2−2): the new "Letter format (v1)" section and
the two `graph` rows match the code —

- sugar args list `done_when, target, write_scope, serves, explores, question,
  depends_on` = `src/mcp.rs:372-395` schema and `parse_sugar`
  (`src/mcp/tools.rs:876-887`); whole-letter refusal and "mail and chat take no
  sugar arguments" = `sugared_body` (`tools.rs:925-944`).
- format-header set (need/want/task/mail/chat + hosted pump record; memos and
  events none yet) = `vivi src/letter/gate.rs:44-56` (pump parses formatted;
  memo/rule/unknown unformatted).
- 422 rule (claims format 1 and fails to parse, or unknown format → 422
  `letter_format` with the parser's message; legacy accepted; replay not
  re-judged) = `check_letter_format` (`projects.rs:1031-1038`) + call order
  (`find_replay` at 1355 before the check at 1360) + `src/projects_api.rs:83-85`.
- graph front-matter rule (legacy and broken claims contribute none, no error)
  = `depends_of` (`tools.rs:671-686`) and its inline test.

### 7. Scope leaks — PASS (one P3 outside `src`)

`git diff --stat 5989c26..need/w3-c3` touches exactly:
`src/mcp.rs`, `src/mcp/letters.rs`, `src/mcp/live/write.rs`, `src/mcp/tools.rs`,
`src/projects.rs`, `src/projects_api.rs` (3 lines: the `LetterFormat` → 422
mapping, the natural home beside the other `ProjectError` arms; allowed by the
brief), `src/rooms.rs` (1 line: composer signature), `docs/contract.md`,
`tests/mcp.rs`, `tests/mcp_hosted.rs`, `tests/projects_api.rs`, plus
`scripts/two-checkout-smoke.sh` (P3-2). No `src` file outside the allowed set.
`letters.rs` shrank its header-parsing surface (`depends_on` field removed) and
grew by 39 lines including an inline test — within the "do not grow it further"
watch (net +39 including tests, acceptable).

## Notes

- Worktree dirt: `M Cargo.lock` (uncommitted) — expected artifact of the
  `.cargo/config.toml` path patch to the vivi pin; not staged, not committed,
  untouched by this audit.
- vivi pin worktree `~/work/ianzepp/vivi-server-w3/vivi-pin` sits at `f289ace8`
  = vivi `main` tip, so the tested bytes are the audited vivi half.
- The parent's claims re-verified here: A (sugar args, `src/mcp.rs` +
  `sugared_body`), B (composers gated, header writer removed — grep 0),
  C (graph front matter), D (422 letter_format), E (pump cross-check test
  passes). Full-suite rc 0 claims were not re-run (out of the cheap-proof
  budget); the targeted trio + gate suites above are green at the audited tips.
