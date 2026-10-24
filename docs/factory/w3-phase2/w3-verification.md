# Wave 3 verification (independent checker)

Date: 2026-10-24. Checker did not implement any wave-3 need. No repo file was edited. The only write is this report. Temp trees live under `/tmp/w3-verify/`.

Final line: **VERIFIED**

## State checked

| Repo | Path | Tip checked | `origin/main` |
| --- | --- | --- | --- |
| vivi | `/Users/ianzepp/work/ianzepp/vivi` | `c3db3c04` docs: phase-2 ledger closeout line | `470225f4` = `v21.0.0^{}` |
| vivi-server | `/Users/ianzepp/work/ianzepp/vivi-server-w3/main` | `75780e0` merge w3/vouch-revert | `99006cd` = `v2.0.0^{}` |

The brief named server tip `7bdea4c`. That commit is an ancestor of the tip that was actually checked out. The two commits after it (`d3c06f8`, `75780e0`) change only `scripts/two-checkout-smoke.sh` (format-1 `vivi sync vouch`; the raw-wire workaround is gone). Product Rust is the same as `7bdea4c`.

Pin worktree `/Users/ianzepp/work/ianzepp/vivi-server-w3/vivi-pin` is `4b8186be`. `git diff 4b8186be..c3db3c04` is docs only (phase-2 ledger + 7 audit files, +1558). Non-docs diff is empty. The gitignored patch is:

```
[patch."https://github.com/ianzepp/vivi"]
vivi = { path = "/Users/ianzepp/work/ianzepp/vivi-server-w3/vivi-pin" }
```

`Cargo.lock` on the server worktree was already dirty (one deletion: the `source = "git+...tag=v21.0.0#470225f4..."` line). Hash-object before and after this checker's cargo runs: `205392634d06d171471e809a0c17139a5bd430d7`. Not staged. `.cargo/` not staged. vivi `git status` is clean.

Ahead of origin equals `tag..HEAD` and nothing else: vivi 58, vivi-server 33. Both `behind` counts are 0. `git ls-remote --tags origin`: vivi's newest tag is `v21.0.0`; server's newest is `v2.0.0`. No later tag on either origin.

Phase-1 merges are ancestors of the release tags: vivi `9ecffd9c` (E2) and `f289ace8` (C3); server `5989c26` (C21), `ff1adb7` (C3), `c58dd7c` (D3). Phase-2 no-ff merges on main after the tags: vivi `e02f59c6` (D1), `ac02d796` (B4), `5b291c7c` (E3), `634c5181` and `c28474b2` (B5), `4b8186be` (D4-vivi). Server `b5b69fe` (B4), `afdbd47` (B5), `9bd68a2` (D4), plus restore commits `21ba137` and `7bdea4c`, then the vouch-revert merge.

## W3-C21

- `cargo test --no-fail-fast` in the patched server main: **rc 0, 18 s**. 29 targets, 352 passed, 0 failed, 3 ignored. Log `/tmp/w3-verify/server-suite.log`.
- `rg "body_has_labeled_clause|with_pump|complete_open_needs" src tests docs` in the server repo: **empty**.
- `VIVI=/Users/ianzepp/work/ianzepp/vivi/target/debug/vivi bash scripts/hosted-smoke.sh`: **rc 0, 4 s, pass=23 fail=0**.
- `scripts/project-sync-smoke.sh`: **rc 0, 6 s, pass=84 fail=0**. Includes `A vouches for B` via `vivi sync vouch`.

## W3-E2

Binary: `/Users/ianzepp/work/ianzepp/vivi/target/debug/vivi`. Temp project `/tmp/w3-verify/e2b/proj` (init already writes `[pump]`; a second `[pump]` was not appended).

- `vivi pump reset`: rc 0. Archive `/tmp/w3-verify/e2b/proj/.vivi/pump-archive-20261024T205615Z` holding `pump.db`, `-wal`, `-shm`, `state.json`, `state.json.imported`, `plans`, `resplit`, `reserved.txt`, `exceptions.txt`, `status.txt`. Stdout ends `control: stopped`.
- `vivi pump health`: `control  stopped`.
- `targets/` and `merge-lanes/` were not in the moved list. A second reset one second later made a distinct archive `pump-archive-20261024T205616Z` (the same-second collision from the E2 P2 is not re-hit).
- Lock held (`fcntl.flock` on `.vivi/pump/lock`): reset rc 1, stderr `error: message error: the pump is running (lock held): run vivi pump stop and wait for it to exit, then reset`. Tree unchanged.
- `vivi pump --help` lists `reset` (`Archive the pump home and start fresh; refused while the pump runs`).
- `tests/pump_reset_cli.rs` in the full suite: **3 passed, 0 failed**.

## W3-C3

Hosted project `/tmp/w3-verify/srv2/host`, server on an ephemeral port, registered with `vivi pump service add --no-pump --serve`.

- MCP `send {kind: need, done_when: ["x"], ...}` returned handle `c59622a5`. Raw blob has `X-Vivi-Format: 1` and front `done_when = ["x"]`. No `X-Vivi-Depends-On`.
- `vivi pump send --need c59622a5`: rc 0, pump record `b202d145`.
- After the project was made ready (harness table, role models, rungs, and `git init` of `.vivi/pump/repo`), `vivi pump once`: rc 0. `flow_runs` row `c59622a5|1|live`. The only exception is `flow-run-error` / `main-rung` (`ls-remote` exit 128, dummy repo has no `origin`). **No `flow-letter-invalid`.** The ls-remote line is the dummy clone, not the letter.
- Earlier `pump once` attempts held while the project was not ready (`flow_runs` empty, no exception). That is hold-until-ready, not a failed letter.
- Library cross-check in the server suite: `a_pumped_need_the_server_composed_starts_a_run` **ok**.
- Durable push of a broken format-1 letter (`/tmp/w3-verify/srv/broken.txt`): HTTP 422 `letter_format`, message `front matter line 3: unknown field `bogus`, expected one of `done_when`, `target`, `depends_on`, `write_scope`, `priority`, `serves`, `explores``.
- Same fixture, no `X-Vivi-Format` (`legacy.txt`): HTTP 422 `letter_format`, `the server accepts only format-1 letters: this one has no X-Vivi-Format header`. (This is also the B4 durable half.)
- Server suite: `a_broken_or_unknown_format_claim_is_refused_and_stores_nothing` **ok**, `a_legacy_letter_without_a_format_header_is_refused` **ok**.

## W3-D3

Live server, two boots.

- `POST /v1/projects/does-not-matter/mcp`: **HTTP 404, empty body**. `POST /v1/host/mcp`: **HTTP 404, empty body**. Reconfirmed 2026-10-24T21:10:14Z on port 60824 (`/tmp/w3-verify/d3re2`).
- `POST /v1/mcp` overview with no project: `project is required` (`/tmp/w3-verify/srv/noproj.txt`).
- Key granted on A, overview of B `rwjqh-1nz8s-kc9j6-a80t3-t737y`: `this key has no grant on rwjqh-1nz8s-kc9j6-a80t3-t737y`.
- `VIVI_MCP_SERVER` + `VIVI_MCP_API_KEY` + `vivi mcp --project-id cmy5g-p16zh-kmexh-sp7fn-tv54z call overview`: JSON with that `project_id` and `you.address` `operator@f946df71.local`.
- `DATA_DIR/audit.jsonl` on that boot: line 1 `self_service_mint`, line 2 `send` (`send c59622a5`). One line per write. The overview read is not a third line. A later `config_set` while running added a `refused` line (D4).
- Planted `projects/oldproj/api_keys.json` renamed at boot. Log: `retired 1 legacy api key(s) of project oldproj (api_keys.json renamed to api_keys.json.retired)`. Live name is `api_keys.json.retired` only. Reconfirmed on the second boot above.
- Server suite `tests/mcp_server.rs`: **17 passed**, including `a_key_granted_on_a_cannot_reach_b_by_read_or_write` and `one_audit_line_per_write_call_and_none_for_reads`. `tests/grants` path includes `retire_legacy_project_keys_renames_once_and_counts` **ok**.

## W3-D5

Covered by the D3 granted-key call and by mcp-smoke `PASS: vivi mcp call overview names the project` (pass=10 fail=0). No separate D5 binary.

## W3-B4

Temp mailspace `/tmp/w3-verify/b4/proj`. Ten blobs, all `X-Vivi-Format: 1`. Scan for the seven moved headers (`X-Vivi-Verdict`, `-Repo`, `-Branch`, `-Commit`, `-Tag`, `-Rule-Scope`, `-Supersedes`): **zero hits**.

| Letter | Front facts | Notes |
| --- | --- | --- |
| Need close | `branch`, `commit` | CLI `need done` has `--branch` and `--commit` and no `--verdict` (`vivi need done --help`). |
| Task close | `verdict = "fixed"`, `branch`, `commit` | CLI `task done --verdict`. |
| Memo | `tags = ["ops"]` | |
| Rule + supersede | `scope`, `supersedes = "msg_..."` | Successor of `ef927bef`. |
| Mark `f20d2fa9` slug `ready` `--for mind` | no front fence | `X-Vivi-Event: marked`, `X-Vivi-Event-Slug: ready`, body `marked`. A kindless mind writes no letter (`may_close` is false); after `role set mind --kind mind` the letter appeared. |

`vivi mail thread` of prose mail `81287705` (body `plain prose mail`): no `+++` fence.

`of_message` was not invoked from a throwaway bin (that would compile into the suite target). The same shapes passed in the suite: `letter::message_test` `a_format_one_need_reads_its_front_and_prose`, `a_format_one_letter_with_a_moved_content_header_is_refused_per_family`, `mailspace::delivery::tests::a_close_with_verdict_carries_it_as_front_matter`, `mailspace::item_events::tests::a_letter_carries_verdict_only_as_close_front_matter`, `mark_letters_are_fenceless_format_one_events_and_still_fold`. All **ok**.

Durable push without `X-Vivi-Format`: HTTP 422 `letter_format` (quoted under C3).

## W3-B5

The real server 422s an unknown front key before a pull can see it (the broken-format probe above). The two-mailspace case is the in-process fake server.

- Lib test `sync::quarantine::tests::an_unknown_front_key_letter_is_held_named_and_not_filed`: **ok**. It holds `.eml` + `.json`, fails the pull naming the sha, and the second pull is silent (`failure()` is none). A resync replay names it again and still files nothing (the B5 P1 rule).
- Companion lib tests in the same module, including `discard_moves_the_pair_to_dropped_and_ids_are_checked`: **ok**.
- CLI `tests/quarantine_cli.rs`: **6 passed**, including `drop_moves_the_pair_under_dropped`. Those CLI tests plant the pair rather than syncing through a server. A hand-built CLI sync of an unknown key through vivi-server was not attempted, because the server refuses the push.

## W3-D1

Full suite (not a second invocation) ran these integration tests green, and `git diff v21.0.0..HEAD` on the oracle files is empty:

| Target | Result |
| --- | --- |
| `tests/config_cli.rs` | 26 passed |
| `tests/pool_cli.rs` | 20 passed |
| `tests/flow_cli.rs` | 3 passed |
| `tests/pump_cli.rs` | 15 passed |
| `tests/pump_answer_cli.rs` | 3 passed |
| `tests/pump_unlock_cli.rs` | 3 passed |
| `tests/pump_rungs_cli.rs` | 4 passed |
| `tests/pump_target_cli.rs` | 5 passed |

`tests/pump_reset_cli.rs` is new (E2), not an oracle edit. Lib tests for the lifts ran inside the same suite (config show/validate/migrate, pool `list_view`/`show_view`, `pump::flow::view`, requeue/withdraw/replan/answer/unlock, `logview::view`). The flow view test `the_view_lists_a_need_runs_with_their_steps_in_order` is **ok**.

## W3-D4

- `tests/config_write.rs`: **7 passed**, including `config_set_and_unset_match_the_library` and `writes_are_refused_while_the_pump_runs_and_a_paused_pump_is_writable`.
- `tests/pool_bands.rs`: **8 passed** (`set_and_unset_write_through_the_guard`).
- `tests/pump_action.rs`: **10 passed**, including `pump_reset_is_refused_while_running_and_while_paused` and the library-twin match tests (`withdraw_matches_the_library_and_closes_the_need`, `replan_answer_and_unlock_match_the_library`, `requeue_steps_and_job_match_the_library`, `pump_reset_matches_the_library`).
- `rg "config_edit::apply" src` in vivi-server: **empty**.
- Live refusal: with the pump lock held and mode running, MCP `config_set` of `pump.tick` was refused. Audit line: `"tool":"config_set","outcome":"refused","summary":"configuration error: the pump is running: run vivi pump pause first"`.

## W3-E3

`tests/project_cli.rs`: **7 passed**, including the three ownership cases:

- `split_ownership::a_read_only_lanes_root_creates_the_projects_half_and_prints_the_lanes_command`
- `lane_add::lane_add_attaches_the_lane_and_prints_it`
- `lane_add::the_printed_command_performs_the_other_half`

Linux ACL test `linux_copies_group_and_acl_from_the_lexicographic_sibling` is in `src/project_create/lane_test.rs` at line 243, under `#[cfg(target_os = "linux")]`. Helpers through line 388 are gated the same way. The test calls `linux_require_acl_tools()` and asserts if `getfacl` / `setfacl` / `chgrp` are missing (the pass-1 vacuous success is gone). This Mac is `aarch64-apple-darwin`, Homebrew rustc 1.99.1, no rustup toolchain directory. The test is not in the macOS binary. The ledger's `x86_64-unknown-linux-gnu` typecheck was **not re-run**. The E3 audit pass 2 (`docs/factory/w3-phase2/w3-e3-audit.md`) records the same limit. Known deviation, not a new gap.

## §6.3 whole campaign

### vivi main

- `cargo test --no-fail-fast`: **rc 101, 217 s** (under 10 min). 75 targets, **4829 passed, 1 failed, 11 ignored**. Lib target: 4273 passed, 1 failed, 4 ignored, 88.74 s. Log `/tmp/w3-verify/vivi-suite.log`.
- The one failure is `pool::local_git::tests::the_lock_holds_across_processes` (`local_git_test.rs:230`): after `drop(held)`, the child exited 3 instead of 0 (`it can once the holder lets go`). The pre-classified sideband `pump::control_test::stopping_a_live_current_pump_writes_control_and_legacy_signal` **passed** in this run.
- Isolated rerun: `cargo test --lib -- --exact pool::local_git::tests::the_lock_holds_across_processes` **rc 0, 0.09 s**. `git log v21.0.0..HEAD -- src/pool/local_git.rs src/pool/local_git_test.rs` is empty. Classified below. Not re-run as a second full suite.
- `cargo fmt --check`: **rc 0**.
- `tests/skills_cli.rs` inside the full suite: **6 passed, 0 failed**. Not run a second time.

### vivi-server patched main

- Suite rc 0, 18 s, as under C21.
- Four smokes, `VIVI` pointed at vivi main's debug binary, serial, after the suite:

| Script | rc | wall | result |
| --- | --- | --- | --- |
| `scripts/hosted-smoke.sh` | 0 | 4 s | pass=23 fail=0 |
| `scripts/project-sync-smoke.sh` | 0 | 6 s | pass=84 fail=0 |
| `scripts/two-checkout-smoke.sh` | 0 | 66 s | pass=46 fail=0 skip=0 |
| `scripts/mcp-smoke.sh` | 0 | 1 s | pass=10 fail=0 |

§6.3 names three smokes. The fourth (`mcp-smoke.sh`) is what the phase ledgers recorded, and it passed. No `vivi-server` process left afterwards.

### Merges, audits, origin

Both mains show one no-ff merge per need (two parents), listed under State. Audit files are on vivi main:

- `docs/factory/w3-phase1/`: ledger, `w3-c21-audit.md`, `w3-c3-audit.md`, `w3-d3-audit.md`, `w3-d3-adversarial.md`, `w3-e2-audit.md`
- `docs/factory/w3-phase2/`: ledger, `w3-b4-audit.md`, `w3-b4-server-audit.md`, `w3-b5-audit.md`, `w3-b5-server-audit.md`, `w3-d1-audit.md`, `w3-d4-audit.md`, `w3-e3-audit.md`

Spot-check of three needs against §6.1 (branch, task commits, merge, suite record, audit verdict and P1/P2, deviations). Git agrees with the ledger lines.

**D1.** Need branch `need/w3-d1`. Tasks `f656044a`, `c2340f20`, `265c8d10`, `91972705`, `40ffa7a1`. Main merge `e02f59c6` (2 parents). Ledger: suite on `fcd0c900` rc 0, 238.6 s, "0 failures"; main rung rc 0, 233 s. Passed/ignored counts are not written. Audit `w3-d1-audit.md`: **CLEAN**, P1 none, P2 none.

**E3.** Tasks `fe72143a`, `203d3695`, `18b32fdc`, `7fe9a071`. Main merge `5b291c7c` (2 parents). Ledger: suite on `ce53b9dc` rc 0, 231.7 s; main rung rc 0, 209.5 s. Counts omitted. Audit pass 1 **BLOCK** (P2: Linux ACL test could pass without copying). Pass 2 **RESIDUAL**, P2 closed in `7fe9a071`, P1 none. Linux execution still recorded as not done. Matches the source read above.

**B5.** Tasks `7be95c4a`, `60fffcd8`, `4ee6a88f`, `49f86914`, `bee7e7e8`, then P1 fix `64c251f7`. Main merges `634c5181` and `c28474b2` (both 2 parents). Ledger: need-branch suite rc 0 ("no FAILED lines", duration and counts not given); rung 4 rc 0, 223 s; rung 5 recorded as green-with-sideband (4265 passed / 1 failed, the named control flake, accepted). Audit `w3-b5-audit.md`: **RESIDUAL**, no P1/P2. Server half went BLOCK then pass 2 RESIDUAL (P1 closed). Files `w3-b5-audit.md` and `w3-b5-server-audit.md` are on main.

§6.1's "passed/failed/ignored counts" bullet is only partly present. The merges, task hashes, verdicts, and P1/P2 fixes that were claimed do match git and the audit files.

### Skills grep

`rg "promote|pump status|closeout|PUMP-PLAN-BEGIN|X-Vivi-Depends-On" skills README.md` hits only the word `closeout` inside `skills/tugboat/**` (Hand closeout, campaign closeout, `check-task-body.py`). No `pump closeout`, no `want promote`, no `pump status`, no `PUMP-PLAN-BEGIN`, no `X-Vivi-Depends-On`. `skills/vivi` and `README.md` have no hits. Not a v21 instruction naming a removed surface.

## Deviations (classified)

Superseded-doc (not a failure):

1. Goal §6.3 "Nothing pushed" and "no `v21.0.0`" predates the authorized 2026-10-24 release. Both mains are ahead of origin by exactly the post-tag commits (58 and 33). No tag beyond `v21.0.0` / `v2.0.0`.
2. Phase-2 ledger closeout still says final tips `4b8186be` / `7bdea4c`, "ahead by ~14 / ~20", and "Nothing pushed". Code tip on vivi is `c3db3c04` (docs after `4b8186be`). Server tip is `75780e0`.
3. The same closeout still describes the two-checkout raw-wire vouch workaround as present and revertible later. `75780e0` is that revert. The smoke that uses `vivi sync vouch` passed (46/0).
4. Goal header still says status planned and "nothing is bumped yet". `Cargo.toml` is vivi `21.0.0` and vivi-server `2.0.0`, and those tags are on origin. The file's own rule says code wins.

Known limit, not a new gap:

5. Linux ACL copy test exists, is cfg-gated, and fails if the tools are missing. It did not execute here. No rustup, so the ledger typecheck was not reproduced. E3 audit pass 2 says the same.

Unrelated sideband, not a need failure:

6. vivi `cargo test --no-fail-fast` exited 101 because `pool::local_git::tests::the_lock_holds_across_processes` failed once under the parallel lib suite and passed in isolation (0.09 s). The file is unchanged since `v21.0.0`. The named `control_test` flake did not fire. Goal §5: an unrelated red is a sideband, not a blocker. It is not the flake §6.3 names, so the raw rc is not 0. It is not tied to a wave-3 diff.

Evidence shape, not a product gap:

7. Ledger suite lines often omit passed/ignored counts and the literal command. Spot-check of D1, E3, and B5 still matches git for branches, task commits, merges, and audit verdicts.

Probe surface, not a product gap:

8. `vivi need done` cannot attach a verdict. Branch and commit were written on a need close. Verdict was written on a task close and is pinned by the close-letter lib tests that passed.
9. B5's unknown-key letter cannot be pushed through the live server (422). The fake-server lib test is the probe the goal allows ("fake server or vivi-server"). CLI `quarantine drop` passed in `quarantine_cli`.
10. Hosted `pump once` also logged `flow-run-error` for `git ls-remote origin` on a seed clone with no remote. The need's `flow_runs` row is `live`. That error is the fixture.

Discarded (checker error, not the product):

11. The first probe script appended a second `[pump]` onto a mailspace that `init` already writes. Those failures (`duplicate key pump`) are not product failures. The rerun without the extra table is what this report uses.

Left as found:

12. Server `Cargo.lock` stays the one-line path-patch deletion. Not staged.

## VERIFIED
