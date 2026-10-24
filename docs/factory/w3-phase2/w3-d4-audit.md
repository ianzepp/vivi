# W3-D4 independent audit — the full tool set and write guards (vivi-server + vivi half)

Auditor: fresh agent, no implementation role. Read-only; the only file written is
this report. Audited 2026-10-24.

- Server branch: `need-w3-d4` at `454905c` (base `b5b69fe`, 14 commits, 7 task
  commits D4-1…D4-7 all landed as merges). Worktree
  `/Users/ianzepp/work/ianzepp/vivi-server-w3/need-w3-d4` (uncommitted `M
  Cargo.lock` present — local path-patch build side effect, not committed; the
  diff range contains no `Cargo.lock`/`.cargo` change).
- Pin: `.cargo/config.toml` patches `vivi` to `vivi-server-w3/vivi-pin` at
  `4b8186be` = vivi local `main` ✓ (and `4b8186be` is the merge of the vivi half).
- Vivi half: `801723e6` ("pool: lift the harness.models allow-list check;
  guarded set_allow write"), merged to vivi `main` as `4b8186be` ✓.
- Authority: `docs/factory/wave3-completion-goal.md` §3.5 (tool table, write
  guards paragraph, lane_create recommendation, done_when), §6.2 W3-D4 probe row,
  §5 step 6 verdict scale.

## 1. Every table tool exists on /v1/mcp and calls the vivi function

All 25 table rows are registered: reads `pump_log`, `flow_show`, `config_show`,
`config_validate`, `pool_list`, `pool_show`, `role_list`, `role_show`,
`charter_show`, `target_list` (`src/mcp/live/views.rs`); writes `config_migrate`,
`config_set`, `config_unset`, `pool_allow_set`, `target_add`, `target_remove`
(`src/mcp/live/config_write.rs`), `role_add`, `role_set`, `charter_set`
(`src/mcp/live/role_write.rs`), `pool_bands` (`src/mcp/live/pool_bands.rs`),
`pump_rungs` (`src/mcp/host/rungs.rs`); `pump_action`, `pump_reset`
(`src/mcp/live/pump_action.rs`); host `server_version`, `project_list`,
`create_project` (`src/mcp/host.rs`), `lane_create` (`src/mcp/host/lane.rs`).
All route through `live::operator_tool` (`src/mcp/live.rs:65-95`) on the
`/v1/mcp` surface.

Mapping spot-verified in code for 20+ tools, including every write family:

| Tool | vivi function (verified at) |
| --- | --- |
| pump_log | `pump::logview::view` (views.rs:68) |
| flow_show | `pump::flow::view` (views.rs:84) |
| config_show / config_validate | `pump::config::show` / `validate` (views.rs:105, 133) |
| pool_list / pool_show | `pool::config::list_view` / `show_view` (views.rs:145, 164) |
| role_list / role_show / charter_show | `Mailspace::{list_role_views, role_view, read_charter}` (views.rs:175, 190, 212) |
| target_list | `pump::config::targets::list` (views.rs:234) |
| config_migrate | `pump::config::migrate` dry / `config_edit::guard_file(migrate write=true)` (config_write.rs:36, 156) |
| config_set / config_unset | `config_edit::set_key` / `unset_key` (config_write.rs:58, 73); both internally `guarded` (vivi `config_edit/keys.rs:116,127`) |
| pool_allow_set | `pool::config_allow::set_allow` (config_write.rs:94), guarded (vivi `config_allow.rs:73`) |
| target_add / target_remove | `targets::add` / `remove` (config_write.rs:112, 131), both `config_edit::guarded` (vivi `targets.rs:79,94`) |
| role_add / role_set | `Mailspace::add_role` / `set_role` under `config_edit::guard_file` (role_write.rs:49, 74) |
| charter_set | `Mailspace::set_charter` under `guard_file` on the charter file (role_write.rs:94) |
| pool_bands set/unset | `config_edit::guarded` + `set_band` / `unset_band` (pool_bands.rs:71-77) |
| pump_rungs set | `RungsConfig::set` → `config_edit::guarded` (host/rungs.rs:116) |
| pump_action | `pump::withdraw::withdraw`, `requeue::requeue`, `replan::replan`, `flow::answer::answer`, `unlock::unlock` (pump_action.rs:85, 110, 123-149) |
| pump_reset | `pump::reset::archive` (pump_action.rs:67) |
| lane_create | `project_create::lane_add::add_split`, projects half (host/lane.rs:146) |
| create_project / project_list / server_version | `project_create::create` / server registry / crate version (host.rs) |

Twin-fixture tests: read closely — `config_set_and_unset_match_the_library`
(tests/config_write.rs:308), `pool_allow_set_matches_the_library…` (:390),
`target_add_and_remove…` (:465), `role_add_matches_the_cli` /
`role_set_matches_the_cli` / `charter_set…` (tests/role_write.rs:298, 393, 483),
`withdraw_matches_the_library…` / `requeue_steps_and_job…` (tests/pump_action.rs:429, 470),
`pump_reset_matches_the_library` (:805), `set_and_unset_write_through_the_guard`
(tests/pool_bands.rs:473), lane attach (tests/host_create_project.rs:727). They
are not vacuous: each runs the library function on a byte-copied twin fixture,
then the tool on the server fixture, and asserts both the returned values and
the resulting on-disk state equal (whole `mailspace.toml` text, backup files,
pending action rows, event states, `git remote get-url`), plus the audit line
count. The tool→library leg is proven directly; the library==CLI leg is
structural (the CLI calls the same functions — spot-verified for role
add/set in vivi `src/local_role_command.rs:21,43,126`; config/pool/target legs
are W3-D1's audited lift). No test spawns the vivi CLI binary (see P3-4).

## 2. Every write tool: refused-while-running test + audit line; guard path

- config_write.rs:517 `writes_are_refused_while_the_pump_runs_and_a_paused_pump_is_writable`
  — covers `config_migrate{write}`, `config_set`, `config_unset`,
  `pool_allow_set`, `target_add`, `target_remove`: message "the pump is
  running", config unchanged, one refused audit line each; then paused+lock
  held → writable, one ok line each.
- role_write.rs:526 — same for `role_add`, `role_set`, `charter_set` (charter
  files also asserted unchanged).
- pool_bands.rs:473 — same for set/unset, plus a backup is kept.
- pump_rungs.rs (`a_set_is_refused_while_the_pump_runs…`) — same for `set`.
- pump_action.rs:843 `pump_reset_is_refused_while_running_and_while_paused` —
  both modes refused with the library message, nothing archived.
- Guard paths: every family uses `guarded` / `guard_file` / `set_key` /
  `unset_key` / `set_allow` (table above). `pool_bands` moved off raw
  `config_edit::apply` to `config_edit::guarded` (diff verified). Guard
  semantics confirmed in vivi `config_edit/guard.rs:65-95,153-163`: refuses
  when the lock is held **and** mode is running (paused writable), backup
  first, validate after, rollback on a new error.

## 3. `grep -rn "config_edit::apply" src` (server worktree)

```
$ cd /Users/ianzepp/work/ianzepp/vivi-server-w3/need-w3-d4
$ grep -rn "config_edit::apply" src
$ echo $?
1
```

Empty ✓. All `config_edit` references in server `src` are `guarded`,
`guard_file`, `set_key`, `unset_key`, `set_band`, `unset_band` (verified by
listing every `config_edit` hit).

## 4. lane_create semantics

Per the settled recommendation: projects half only. `attach`
(host/lane.rs:57-68) validates name/target, requires both roots to exist, and
**refuses before calling `add_split` when the bare repo is missing**
(`refuse_missing`, :119-143), answering the exact lanes-owner command plus
readiness. When the repo exists, `hold_lanes` (:207-224) drops the lanes
root's write bits for the library call if this process can write it, re-probes,
and restores on drop. `add_split`'s own writable probe then selects the
projects half (clone + guarded `targets::add`).

Soundness of the write-bit trick:
- Fail-safe against ineffective revokes: the re-probe after `set_permissions`
  is a real `mkdir`, so root (ignores mode bits) and POSIX-ACL write grants are
  detected → the tool refuses ("this process can still write it") rather than
  risking the lanes half. The library probe also only counts `PermissionDenied`.
- Races: a concurrent permission restore between the re-probe and the library
  call can at worst make `add_split` see a writable root and take the both-halves
  path — which refuses an existing repo, and the repo's existence was checked
  before the hold. Worst case is an error, not a write under the lanes root.
  A wrong write would additionally require the repo to vanish mid-call.
- Kill window: a hard kill between revoke and `Drop` leaves the lanes root
  without write bits until its owner restores them; restore failure inside
  `Drop` is only logged. Error paths and panics unwind through `Drop`. On hub
  this path never runs (the server cannot write the lanes root; probe returns
  `Untouched`). → P3-2.
- Tests: attach (:727) proves nothing is created under the lanes root, the
  write bit is restored (`assert_lanes_writable`), the target write is guarded
  with a backup, and `remaining.command` equals the expected string; the
  refusal path (:841) pins the same command from the server-side formatter.
  Both formatter copies (server `lanes_owner_command` and the library's
  `remainder_command`) are pinned against the same expected string, so drift
  fails a test. Currently byte-identical.

## 5. The vivi half (`config_allow`)

- `validate_allow` (vivi `src/pool/config_allow.rs:32-51`) reproduces the old
  server check with the **verbatim** messages: "model '{model}' is not in pool
  '{pool}' harness.models; allowed: {list}" and "(none: the pool has no harness
  models)" — identical to the removed server text (diff of
  `src/mcp/live/pool_bands.rs` vs base confirms what was deleted).
- `set_allow` is guarded (`config_edit::guarded` → `set_pool_key`,
  config_allow.rs:64-77); slug/duplicate/unknown-pool refusals happen before
  anything is written, and `set_pool_key` itself refuses an unknown pool inside
  the guard (vivi `config_edit/pool.rs:139-148`), so the pre-check outside the
  lock is belt-and-braces, not a TOCTOU hole.
- 8 companion tests in `src/pool/config_allow_test.rs` (companion-file
  convention, `#[path]` wiring): pass/none/harness/empty-list for
  `validate_allow`; replace-through-guard (asserts raw TOML text), bad slug,
  duplicate, unknown pool for `set_allow`. Ran them: 8 passed.
- Conventions: errors in `VivariumError`, no panics, 106/167-line files,
  re-exported at the config path (`pool/config.rs`). No CLI surface changed, so
  no skill/README update owed under §3.8.

## 6. No test weakened; conventions; docs

`git diff b5b69fe..454905c -- tests/` removes exactly five lines, all
mechanical: three `let state = app_state(base.join("data"), None, None);`
hoisted into a `data` field (for `audit_lines`), one `use std::path::PathBuf;`
widened, one `let mut state = …`. Everything else is additive (new files and
new tests inside existing files). The vivi half adds a test file, changes no
existing test. Commit identity and `Co-Authored-By` present on all 14 server
commits and the vivi commit; no `Cargo.lock`/`.cargo` in either diff.

Docs: `docs/contract.md` enumerates every tool with argument and return tables
matching the implemented schemas (spot-checked `config_*`, `pool_*`, `role_*`,
`charter_*`, `target_*`, `pump_*`, `lane_create`, `server_version`,
`project_list`, and the `resolved`/`--pid`/`--host`/dry-run/audit rules);
`CHANGELOG.md` has five accurate D4 entries; `README.md` host-token row now
names the host tools; `docs/phase-04-hosted-projects.md` replaced its stale
"only config the server edits" claim with the real tool set. `AGENTS.md` does
not enumerate tools, so it needed no change.

## 7. Cross-tenant sanity

The gate is structural and tool-name-independent: on `/v1/mcp`,
`server.rs:305-330` requires `project`, resolves it through the key's grants
(`resolve_grant`/`live_grant`, server.rs:408-461), and refuses with the pinned
text `this key has no grant on <B>` **before** any tool dispatch — the D4 tools
are reachable only through that same `dispatch_tool`. The pinned text is tested
for a read (`overview`) and a write (`send`) with a key granted on A naming B,
and for an unknown project (tests/mcp_server.rs:338-371, :1171). No
D4-tool-specific no-grant test exists (P3-3, method note); D4-side negative
coverage that does exist: non-hosted project refusal for `pump_action`/
`pump_reset` with audit (tests/pump_action.rs:924-980), and non-operator-key
refusals for every write family (config_write.rs:573, role_write.rs:587,
pool_bands/pump_action/pump_rungs equivalents).

## 8. Runs performed (burgus, warm target; no full suite, per instructions)

- `cargo test --test mcp --test mcp_server --test config_write --test role_write
  --test pump_action --test pool_bands --test mcp_views --test host_create_project`
  — exit 0; eight binaries, **109 passed, 0 failed, 1 ignored** (per-binary:
  7, 19, 32+1i, 17, 10, 8, 10, 6); 6.7 s warm after build.
- `cargo check --all-targets` — rc 0, 4.0 s; warnings only from the pinned vivi
  dependency (`vivi-pin` dead-code/deprecation, pre-existing), none in server code.
- `cargo fmt --check` — rc 0.
- vivi half on vivi `main` `4b8186be`: `cargo test --lib pool::config_allow` —
  8 passed, 0 failed, 0.03 s.

## Deliberate rulings — verified as documented, not defects

1. `resolved: true` refused on `config_show`/`pool_show`
   (views.rs:246-252 `RESOLVED_REFUSED`; tested mcp_views.rs:345-355).
2. `role_set` omits `--pid`/`--host` (role_write.rs:13-19; schema has neither;
   contract.md:470 documents why).
3. `pump_action` not refused while running (no lock check; pinned positively by
   `withdraw_posts_while_the_pump_lock_is_held`, pump_action.rs:777-801).
4. `pump_reset` refused running **and** paused (pump_action.rs:843-880).

Minor schema note, not a defect: the authority table sketches `pump_action
{need?}`; the implementation and contract use `needs` (string or list) for
withdraw, matching the library's `withdraw(root, needs: &[…], reason)`.

## Findings

- **P3-1** `flow_show`'s populated-runs JSON path is unpinned on the tool side:
  `runs_json`/`state_word` (views.rs:255-279) are a hand copy of the CLI's
  (vivi `local_flow_command.rs`), byte-identical today, but
  `flow_show_answers_the_flow_view` (mcp_views.rs:302-331) exercises only the
  no-runs and `runs: null` shapes — never a run with steps. A drift in the
  populated shape would pass tests.
- **P3-2** lane_create's `LanesHold` revoke mutates a possibly-shared lanes
  root's mode bits for the duration of the library call; a hard kill in that
  window leaves the root read-only until the owner restores it (restore failure
  only logged, host/lane.rs:289-303). Never triggers on hub (server cannot
  write the lanes root). Fail-safe in every analyzed race.
- **P3-3** no D4-tool-specific cross-tenant test; the shared no-grant gate is
  pinned with `overview`/`send` and is dispatch-order-verified in code (§7).
- **P3-4** twin fixtures compare tool vs library function, never the CLI
  binary; "same result as the CLI" holds transitively (tool == library ==
  CLI). This matches the campaign Target's definition of equivalence; recorded
  for the ledger.

Done_when: every table tool exists with a calls-the-function test (twin
fixtures, non-vacuous) and every write tool has a refused-while-running test
and an audit line ✓; `grep config_edit::apply` empty ✓; mandated checks green
✓; no test weakened ✓; docs current ✓. P1/P2: none.

VERDICT: CLEAN
