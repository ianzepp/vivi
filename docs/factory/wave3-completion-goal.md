# vivi wave 3: completion

**Status**: planned — rounds 1-2 delivered (vivi main c85900db); this goal holds the rest of the wave-3 campaign

- **Written**: 2026-10-23, planner seat, read-only. The code was read at vivi local `main` `c85900db` and
  vivi-server local `main` `59411be`.
- **Who reads this**: a new session, maybe on another harness or model, with no memory of rounds 1-2. It
  delivers the work from this file alone. The session that wrote this file then checks the result against
  §6.
- **Ground truth**: code, git history and tests. Every `file:line` here was re-read at the commits above.
  Docs are stale by default; that includes this file once code moves. When this file and the code
  disagree, the code wins. Write the disagreement in your ledger.
- **Tags**: **[code]** read in source; **[ran]** run by the planner (see §1.4); **[unverified]** not checked.
  Paths are repo-relative. `vs:` marks a vivi-server path.

---

## 1. Context

### 1.1 What rounds 1-2 delivered

All of this is merged on vivi local `main` `c85900db` (375 commits after the v20.0.0 tag, unpushed):

- **The v1 letter format.** Need, want, task and pump-record letters carry TOML front matter between
  `+++` fences, then Markdown prose. Each new letter carries the header `X-Vivi-Format: 1`. One parser
  lives in `src/letter.rs` and `src/letter/` (`kinds.rs`, `work.rs`, `records.rs`, `plain.rs`, `sugar.rs`,
  `message.rs`, `read.rs`). Unknown keys are refused.
- **The compose gate.** `Mailspace::send` checks every send and reply
  (`src/mailspace/delivery.rs:44-65` calls `compose::gate`, `src/mailspace/delivery/compose.rs:35-56`). A
  faulty front is refused with a message that names the fault. A need without `done_when` is refused
  (`compose.rs:130-142`). A want without `question` is refused.
- **Plain prose stays plain.** Mail and chat with no front matter are stored as written, with
  `X-Vivi-Format: 1`. The parser reads "no front matter" as an empty front for mail and chat
  (`src/letter/kinds.rs:116-129`). A body that opens with `+++` is still checked strictly.
- **Wants are discussions.** `vivi want promote` is gone. A need names its want with `serves`
  (counts toward the want's state) or `explores` (research; does not count). A want's state is computed
  from its serving needs (`src/mailspace/want_state.rs`). `vivi pump withdraw` closes the need itself with
  verdict `withdrawn`.
- **v19 surfaces removed.** `pump status`, `pump closeout`, `pump adopt`, mind oversight, the boot pump
  view, `board --pump`, the old action path, the v19 tick work and their help text are gone.
- **Config.** `vivi config validate` refuses (error, exit 1) every key v21 never reads, with the hint
  `fix: run vivi config migrate --write`; `migrate --write` removes them. `vivi config set|unset` and
  `vivi pump target list|add|remove` exist. Config writes go through a guard
  (`src/mailspace/config_edit/guard.rs:53-75`): refused while the pump runs, a backup first, validate
  after, rollback on a new error.
- **Pump and config defects A1-A8** fixed (correction jobs carry `from`; dispatch refusals hold with one
  alarm; live target add; a brand-new `pump.db` starts stopped; and so on).
- **The `vivi mcp` client** (campaign D5; round-1 need name `W3-D2`) posts to `<server>/v1/mcp`
  (`src/sync/http.rs:437-440`), uses an API key only, and fills `project` into tool calls
  (`src/mcp/inject.rs`). The pool stdio bridge does the same (`src/pool/bridge.rs:166-182`).
- **Skills describe v21 only** (`skills/vivi/**`, `skills/tugboat/**`, README). `cargo test --test
  skills_cli` runs the examples.

vivi-server local `main` `59411be` (unpushed) holds round 1's C1/C2: it builds on the **published vivi
v20.0.0** tag (`vs:Cargo.toml:24`), `pump_gate` is replaced by `pump_rungs`, and `create_project` takes
rungs.

### 1.2 Rulings that bind the delivering session (do not reopen)

From the campaign brief (operator, 2026-10-23), verbatim or near-verbatim:

1. **Universal letter format, clean break.** Every recorded letter is TOML front matter (`+++` … `+++`)
   followed by Markdown prose. Old-format letters are archived history: anything that picks one up for
   execution gets a parse failure naming the problem. No conversion step. No dual-format period.
2. **A missing `done_when` is a hard error at send.**
3. **The MCP server is per vivi-server, not per project.** One connection per server. Every
   project-level tool takes a `project` argument. The project is never implied by the connection.
4. **Build with subagents, not the pump.** Then the operator pushes both repos and installs on hub.
5. **Standing rules.** Clean break unless a real external contract needs compatibility (the vivi ↔
   vivi-server wire is one, and it moves in lockstep). No silent holds: every stuck or dropped item raises
   one alarm or closes. Facts come from machines. **Version bumps, tags, pushes and hub installs need the
   operator's explicit go.**
6. **B4: headers carry transport only** (from, to, thread, kind, ids). Every content fact moves to front
   matter. No fact in both places. Settled with the plan: the transport headers that stay are RFC 5322
   routing and threading, `X-Vivi-Kind`, `X-Vivi-Format`, and the item-event protocol (`X-Vivi-Event`,
   `-Event-Of`, `-Event-At`, `-Event-Prev`, `-Event-Slug`, `-Event-State`, `-Event-Unit`, `-Event-Dep`,
   `X-Vivi-Delegate`). The rooms endpoint `/v1/rooms/{id}/mcp` is unchanged.
7. **Versions**: vivi **21.0.0**, vivi-server **2.0.0** (planned; nothing is bumped yet).
8. **Amendment W (wants and needs).** W1 `want promote` removed. W2 `serves = "<want>"`, checked to be a
   want. W3 a want's state is computed: open while any serving need is open; done once it has at least one
   serving need and all are closed (withdrawn counts as closed); a new serving need reopens it; no serving
   needs = open; `vivi want done` still works by hand and a later serving need wins; a reply does not
   reopen. W4 want show and the board list serving needs with state. W6 research needs use `explores`, not
   counted, `serves` XOR `explores`. W7 `pump withdraw` also closes the need (verdict `withdrawn`, syncs).
9. **Amendment V (retire v19; prose unwrapped).** V1 plain-prose mail is not wrapped; nothing shows an
   empty `+++` fence. V2 every v19 pump surface is removed, or disabled and hidden. A config key v21 does
   not read is refused or warned by validate, never silently accepted. V3 wants are not promotable. V4
   **skills describe v21 only**: no v19 or pre-v20 text, history or labels.
10. **W3A-16.** The pump's authority over a need is bound to one full address (locally the pump record's
    opener; remotely the earliest non-event pump-role post on the need thread).
11. **Forward progress over green** (operator, 2026-10-23). The repo need not be green at all times.
    Unrelated test reds are sideband fixes, never blockers. Only reds tied to the change in hand stop it.
12. **Test budget.** A full test-suite run may take up to 10 minutes. Over that is a finding to report,
    not a reason to raise the timeout.
13. **Out of scope** (campaign): hub admin scripts and sudoers; N8 items not listed (J1 status views
    beyond the runs/steps view, J2 multi-member identity, `reply_from = "pump"`); polish-ledger follow-ups;
    **the held Q-A planning code** (`src/pump/amend.rs`, `src/pump/plan.rs`, and their kin) stays as is;
    rewriting old Faber needs.

### 1.3 Where things are

| What | Where | State |
| --- | --- | --- |
| vivi | `~/work/ianzepp/vivi`, branch `main`, `c85900db` | `Cargo.toml` says `version = "20.0.0"`; 375 commits past tag `v20.0.0`; unpushed |
| vivi-server | worktree `~/work/ianzepp/vivi-server-w3/main`, branch `main`, `59411be` | `version = "1.11.0"`; depends on `vivi` tag `v20.0.0`; unpushed |
| vivi-server main checkout | `~/work/ianzepp/vivi-server` | stale branch `feat/vivi-18` with foreign worktrees: **do not use or clean** |
| Campaign brief, plan, ledger | session scratchpad of the coordinating session (`wave3-campaign.md`, `wave3-plan.md`, `wave3-ledger.md`) | read-only background; this file supersedes them for the remaining work |
| hub | vivi 20.0.0 installed; pump **paused** and **empty** (nothing running); hand role pool = studio (ready) | not touched by this goal |

Nothing in this goal is bumped, tagged, pushed or installed. §2.5 is the operator's checklist.

### 1.4 What the planner ran (evidence)

- vivi-server `59411be` exported with `git archive` into a scratch dir, `vivi` switched to a path
  dependency on `~/work/ianzepp/vivi` (`c85900db`). `cargo test --no-run`: 3 compile errors in `src/`
  and 6 struct-field errors in `tests/` (all listed in §2.2). After a throwaway patch of those,
  `cargo test --no-fail-fast` (11 s) failed **37 tests in 7 binaries**, listed in §2.2. The scratch copy is
  not committed anywhere. **[ran]**
- `target/debug/vivi pump --help` at `c85900db`: there is no `reset` subcommand. **[ran]**
- `target/debug/vivi monitor --help` panics (clap debug assert). **[ran]**

---

## 2. Phase 1 — unblock the release

Phase 1 is required before anything in Phase 2. When it is done (plus the operator decision in §2.0),
the operator can run §2.5.

### 2.0 One operator decision before Phase 1 starts

**The `/v1/mcp` gap.** vivi main's `vivi mcp` client and the pool stdio bridge already post only to
`<server>/v1/mcp` (`src/sync/http.rs:437-440`, `src/mcp/inject.rs`). vivi-server has no such route: it
serves `/v1/projects/{id}/mcp` and `/v1/host/mcp` (`vs:src/lib.rs:141,144`). So if vivi 21 is installed
with a vivi-server that lacks `/v1/mcp`, `vivi mcp tools|call` and every pool job that uses the MCP
bridge get a 404. The claude.ai connectors call the per-project URL directly, so they keep working until
the per-project route is removed. **[code]**; whether hub pools use the bridge today is **[unverified]**.

- **Option A (planner's recommendation; the default if the operator has not ruled):** deliver
  **W3-D3** (server endpoint, keys, grants, audit; §3.4) as part of Phase 1, before the release. Then
  vivi 21 and vivi-server 2.0.0 ship together and 2.0.0 is an honest major version.
- **Option B:** release after W3-C21, W3-C3 and W3-E2 only. `vivi mcp` and pool MCP bridges stay broken
  against hub until W3-D3 ships. vivi-server would then be 1.12.0, not 2.0.0.

The delivering session follows Option A unless the operator's message says B.

### 2.1 Phase 1 needs and order

| Need | Repo(s) | Size (hand tasks) | After |
| --- | --- | --- | --- |
| **W3-C21** vivi-server on vivi 21 | vivi-server | 5-6 | — |
| **W3-E2** `vivi pump reset` | vivi | 3 | — (parallel with C21) |
| **W3-C3** server-composed letters through vivi's gate | vivi (1-2) + vivi-server (4-5) | 6-7 | C21 merged |
| **W3-D3** server-level MCP endpoint (Option A) | vivi-server | 8-10 | C3 merged (shared `vs:src/mcp.rs`) |

### 2.2 W3-C21 — vivi-server builds and passes on vivi 21

**Goal.** vivi-server compiles against vivi main and its whole suite is green, with behaviour that
matches vivi 21. No new features.

**How it builds against unreleased vivi** (vivi-server's own pattern, `vs:AGENTS.md:104-111`, proved in
the plan):
- `vs:Cargo.toml:24` keeps `vivi = { git = "https://github.com/ianzepp/vivi", tag = "v20.0.0" }`.
- Each vivi-server worktree gets a **git-ignored** `.cargo/config.toml` (`vs:.gitignore` lists `/.cargo`):
  ```toml
  [patch."https://github.com/ianzepp/vivi"]
  vivi = { path = "<a detached vivi worktree at vivi local main tip>" }
  ```
  then `cargo update -p vivi` locally. Never stage `Cargo.lock` while the patch is active. Never commit
  `.cargo/`.
- Use a **detached vivi worktree** (for example `git -C ~/work/ianzepp/vivi worktree add --detach
  <dir> main`), not the vivi main checkout. Refresh it (`git -C <dir> checkout --detach main`) after each
  vivi merge.
- A patch works only over a **published** tag. Naming the unpushed `v21.0.0` in `Cargo.toml` fails.
- **Truth:** vivi-server local main compiles only with the patch until the operator pushes vivi
  `v21.0.0`. Each vivi-server merge commit message names the vivi commit it was tested against.
- **Release pin** (operator step, §2.5): after vivi `v21.0.0` is pushed, set `tag = "v21.0.0"`, remove the
  patch, `cargo update -p vivi`, rerun the gate.

**Compile breaks (vivi v20.0.0 → c85900db)** **[ran]**:

| Site | Break | Fix |
| --- | --- | --- |
| `vs:src/mcp/live/write.rs:5` | `vivi::mailspace::body_has_labeled_clause` removed | drop it and the `note` block at `write.rs:82-87` (vivi refuses a need without `done_when` now; the note is moot) |
| `vs:src/mcp/live/write.rs:77` | `SendRequest.depends_on` removed (deps live in front matter) | drop the field; `depends_on` goes into front matter in W3-C3 |
| `vs:src/mcp/live/read.rs:512` | `BoardOptions.with_pump` removed | drop it; drop the `pump` flag from the `board` tool schema (`vs:src/mcp.rs:548`) and description |
| `vs:tests/measure_index.rs:41`, `tests/pump_records.rs:91`, `tests/hosted_index.rs:149`, `tests/mcp_hosted.rs:152,460,894` | `SendRequest.depends_on` | drop the field; `mcp_hosted.rs:460` sends deps: move them into the body's front matter |

**vivi APIs vivi-server uses** (grep of `vivi::` in `vs:src` and `vs:tests`). Changed between v20.0.0 and
c85900db, in signature or behaviour:

| API | Change | vivi-server sites |
| --- | --- | --- |
| `mailspace::SendRequest` | no `depends_on`; the gate refuses a need without front matter `done_when`, a want without `question`, a task without front matter | `live/write.rs:65-78`; 6 test sites above |
| `mailspace::body_has_labeled_clause` | removed | `live/write.rs:5,82` |
| `mailspace::board::BoardOptions` | no `with_pump` | `live/read.rs:506-513`, `tests/mcp_hosted.rs:14` |
| `mailspace::board` output | wants carry `serving: [{handle, state, …}]` (`src/mailspace/board.rs:112-118`); passes through `serde_json::to_value` already | `live/read.rs:516`, `bound_board` `:535-570` |
| `pump::health::PumpHealth` | fields now: `verdict, reasons, as_of, control, shutdown_requested, writer, tick_secs, stale_after_secs, last_tick, status_age_secs, schema, exceptions, changed_since` (`src/pump/health.rs:68-83`); `units`, `merges`, `blocked`, `complete_open_needs` are gone | `vs:src/pump_health.rs`, `vs:src/mcp.rs:482` (description), `vs:docs/contract.md:493-497`, `tests/pump_health.rs:186-205` |
| `pump::control` fresh db | a brand-new `pump.db` starts **Stopped** (`src/pump/control.rs:70-75`) | `tests/pump_control.rs:205,228,240` assume Running |
| `pump::config::RungsConfig::set` | now a guarded write (W3E1-6): rolls back when a rung command has no effective pools | `vs:src/mcp/host/rungs.rs:76-108` (sets rungs before pools), `vs:src/mcp/live/pump.rs` `pump_rungs`, `tests/pump_rungs.rs:109` |
| `pump::readiness::assess` | new not-ready items (hand pool, pump-planner pool, merge pool, auditor role, missing `task`/`need`/`main` rungs) | `host/rungs.rs:183`, `tests/host_create_project.rs:323,387` |
| `mailspace::PumpRecord` | gained `letter_error`; legacy records are skipped | `tests/pump_records.rs:99` (compiles) |
| `PumpActionRequest` | now `requeue(unit, reason)`, `withdraw(reason)`, `replan()`, `answer(step, word, note)`, `unlock(entry, note)` (`src/mailspace/pump_action.rs:124-156`) | not used by vivi-server today (grep) |
| `boot --role`, `collect_with` | removed | not used by vivi-server (grep) |

Unchanged and fine (compiles, no behaviour change seen): `Mailspace` reads, `message::{build_compose_draft,
render_message, normalize_message_id}`, `storage::*`, `sync::{pull::file_records, push::push, lock,
state, transport}`, `registry`, `project_create`, `pool::{PoolConfig, config_bands}`,
`pump::{control::set_project_mode, health::HealthOptions, store::DB_FILENAME, state::State}`.

**Runtime failures on vivi 21** (37 tests, **[ran]**), grouped by cause:

| Cause | Tests |
| --- | --- |
| Fixture sends a need with prose `done_when:` (refused) or a want with no `question` | `tests/hosted_index.rs` 8 (all at `:150`); `tests/mcp_hosted.rs` 14 (`:117`, `:153`, `:893`); `tests/pump_records.rs` 5 (`:92`) |
| Rungs set before pools → guarded write rolls back | `tests/pump_rungs.rs` 4 (`:109`); `tests/host_create_project.rs` 2 (`:323`, `:387`) |
| Fresh project's pump is Stopped, test expects Running | `tests/pump_control.rs` 3 (`:205`, `:228`, `:240`) |
| Health test pins removed fields | `tests/pump_health.rs` 1 (`:193-205`) |

**Work.**
1. Fix the compile breaks above.
2. Fixtures: every need/want/task a test sends is v1 (use `vivi::letter::render` or the
   `vivi::letter::sugar::apply` helper, or a literal `+++` front). Do not weaken any assertion.
3. `create_project` rungs (`vs:src/mcp/host/rungs.rs:76-131`): write `pools` and `timeout` **before** the
   rung commands, or do all of it in one `vivi::mailspace::config_edit::guarded` write. Replace the raw
   `config_edit::apply` in `write_settings` (`:118`) with `guarded` so the server never writes config
   outside the guard.
4. `pump_rungs set` (`vs:src/mcp/live/pump.rs`): when the rung table has no effective pools, the tool
   returns vivi's guard error as is (it already names the fix). Update the tool description at
   `vs:src/mcp.rs:498`: "a set rung with no rung pools leaves the config invalid" is now false; the write
   is refused instead. Tests set `pools` first.
5. Pump control tests: start the pump explicitly (`set_project_mode(Running)`) where the test needs a
   running pump, and add one test that a fresh hosted project reads `stopped`.
6. Health: rewrite `tests/pump_health.rs:186-205` to the v21 shape. Rewrite the `pump` tool description
   (`vs:src/mcp.rs:482`): drop "unit counts, recent merges, blocked units, complete-but-open needs". Fix
   `vs:docs/contract.md:493-497` the same way.
7. Board: one test that a hosted want shows its `serving` needs with state through the `board` tool.
   Mention `serving` in the `board` description.
8. Stale v19/v20 text in vivi-server: `vs:src/mcp/live/write.rs:139` says "promote it to a need
   instead" → say "send a need that serves it (`serves`)". Grep vivi-server for `promote`, `closeout`,
   `with_pump`, `pump_gate`, `gate` and fix any text that names a removed vivi surface.

**Note on `pump_gate`.** The brief and a round-2 ledger note say the `pump_gate` tool still writes
`[pump.gate]`. That is stale: W3C-1 already removed it on vivi-server main (grep finds no `pump_gate` or
`pump.gate` in `vs:src`, `vs:tests`, `vs:docs/contract.md`). Nothing to do. **[code]**

**Tests that must exist.** Hosted send of a v1 need lands with front matter; a fresh hosted project
reads `stopped`; create_project with `rungs` + `pools` succeeds and readiness reflects them; board lists
`serving`; health JSON has no removed field.

**done_when:**
- `cargo test --no-fail-fast` in a vivi-server worktree with the path patch at vivi local main tip exits 0.
- `cargo build` of the same tree with `-D warnings` is not required, but the build has no new warning
  from vivi-server code.
- `grep -rn "body_has_labeled_clause\|with_pump\|complete_open_needs\|promote it" src tests docs` in
  vivi-server finds nothing.
- `scripts/hosted-smoke.sh` and `scripts/project-sync-smoke.sh` pass with
  `VIVI=<vivi worktree>/target/debug/vivi` (their MCP steps still use the per-project route; that moves in
  W3-D3).
- An audit file `w3-c21-audit.md` exists with verdict CLEAN or RESIDUAL and no open P1/P2.

### 2.3 W3-E2 — `vivi pump reset`

**Ground truth.** No `reset` exists (`vivi pump --help` **[ran]**; `src/cli/pump_command.rs`). Pump home
is `<project>/.vivi/pump` or `[pump] home` (`src/pump/config.rs:266`, `src/pump/control.rs:170`
`project_home`). "Running" = `lock_is_held(home)` (`src/pump/run.rs:538`). Contents today
**[code]**: `pump.db` + `-wal` + `-shm` (`src/pump/store.rs:43`), `lock`, `status.txt`, `log.txt`,
`exceptions.txt`, `state.json` / `state.json.imported` (`src/pump/run.rs:216-221`), `plans/` with
`rejected/` and `superseded/` (`src/pump/board.rs:67`, `src/pump/plan.rs:98,348`, `src/pump/amend.rs:261`),
`resplit/` (`src/pump/collect.rs:168`), `reserved.txt` (`src/pump/config.rs:114`), `sync-push.log`
(`src/pump/board.rs:117`), `targets/<id>/repo` (`src/pump/config.rs:204`), `merge-lanes/`.
`plans-held/` does not exist (the brief's list is stale).

**Correction to a ledger note.** The ledger says "job_release must be included in `state_rows::clear`".
Do **not** do that. `state_rows::clear` (`src/pump/store/state_rows.rs:10-26`) is the first step of
`PumpStore::save` (`src/pump/store.rs:381-389`), which rewrites the v19 `State` tables on every save.
`job_release` (`src/pump/store/job_release.rs:15`) is a request queue the running pump drains with
`take_job_releases`. Adding it to `clear` would drop pending releases on every save. Reset archives the
database file, so no table clearing is needed at all.

**Target behaviour.**
- `vivi pump reset [--project P] [--json]`.
- Refused while the pump runs: if `lock_is_held(home)`, exit non-zero with
  `the pump is running (lock held): run vivi pump stop and wait for it to exit, then reset`. A paused pump
  still holds the lock, so it is refused too.
- Archives into `<home>/../pump-archive-<UTC stamp YYYYMMDDTHHMMSSZ>/` (beside the pump home, as the
  campaign says), by rename: `pump.db`, `pump.db-wal`, `pump.db-shm`, `state.json`,
  `state.json.imported`, `plans/`, `resplit/`, `reserved.txt`, `exceptions.txt`, `status.txt`. Missing
  entries are skipped. Recommended to keep in place: `targets/` (integration clones, slow to remake),
  `merge-lanes/`, `log.txt` (append one line `reset: archived to <dir>`), `sync-push.log`, `lock`.
- Then creates a fresh database through `pump::control::initialize(home)`, which seeds `Stopped` because
  no db exists (`control.rs:70-75`). A leftover `STOP`/`PAUSE` marker is honoured as today.
- Prints the archive path, each moved entry, the new control mode (`stopped`), and the number of **open
  pump records** in the mailspace. Pump records are letters, not db rows: the next tick re-ingests every
  open record and starts fresh runs. The output says so and names `vivi pump withdraw` to stop that.
- Library function `vivi::pump::reset::archive(project: &Path) -> Result<ResetReport, VivariumError>`
  (no printing), so W3-D4's `pump_reset` tool calls the same code.

**Open questions (recommendations).**
- Archive `log.txt` too? Recommend no (keep the history readable in one place; append the reset line).
- A `--force` to reset a paused pump? Recommend no. The lock is the truth; stop first.

**Files.** New `src/pump/reset.rs` + `src/pump/reset_test.rs`; `src/pump.rs` (mod line);
`src/cli/pump_command.rs` (subcommand); `src/local_pump_command.rs` (printer); `skills/vivi/SKILL.md` pump
section (one entry, v21 wording); README pump table if it lists pump commands.

**Tests.** Archive moves exactly the listed entries and keeps `targets/`; a held lock refuses and moves
nothing; after reset `control::inspect` reads `stopped`; the report counts open pump records; CLI test in
`tests/pump_cli.rs` (or a new `tests/pump_reset_cli.rs`).

**done_when:**
- `vivi pump reset` on a stopped pump moves the listed entries into one `pump-archive-<stamp>/` folder and
  leaves a new `pump.db` whose control mode is `stopped`.
- `vivi pump reset` while the pump lock is held exits non-zero, names `vivi pump stop`, and changes no
  file.
- `vivi::pump::reset::archive` exists as a public library function with a unit test.
- `vivi pump --help` lists `reset`; `cargo test --test skills_cli` passes.
- Full vivi `cargo test --no-fail-fast` on the need branch: no failure tied to this change.

### 2.4 W3-C3 — every letter the server composes is a v1 letter

**Ground truth: every composer in vivi-server** **[code]**:

| Composer | Path | Today |
| --- | --- | --- |
| Hosted `send` | `vs:src/mcp/live/write.rs:37-90` → `Mailspace::send` | goes through vivi's gate already; takes only `kind, to, subject, body, depends_on`, so a need with no front matter is refused with no way to pass `done_when` except a hand-written `+++` body |
| Hosted `reply` | `vs:src/mcp/live/write.rs:91-113` → `Mailspace::reply` | gated by vivi |
| Hosted `done`/`reopen`/`mark`/`unmark`/`depend`/`memo`, `pump_send` | `vs:src/mcp/live/*` → vivi `Mailspace` methods | vivi writes them; they follow vivi (events and memos are B4's) |
| Durable/chat `send` | `vs:src/mcp/tools.rs:311-345` → `vs:src/mcp/letters.rs:173-216` `compose_send` | `build_compose_draft` + `X-Vivi-Kind` + one **`X-Vivi-Depends-On`** per dep (`letters.rs:202-205`); no front matter check; no `X-Vivi-Format` |
| Durable/chat `reply` | `vs:src/mcp/tools.rs:351` → `letters.rs:218-284` `compose_reply` | no check, no `X-Vivi-Format` |
| Durable event (`done`/`reopen`, …) | `letters.rs:295-382` `compose_event` | event headers; body = note |
| Durable `memo` save | `vs:src/mcp/tools.rs:753-792` → `compose_send(kind "memo")` + `Storage::ingest_message` | no check |
| Rooms chat | `vs:src/rooms.rs:427-441` → `compose_send(kind "chat")` / `compose_reply` | no `X-Vivi-Format` |
| Transport graph reader | `letters.rs:166` reads `x-vivi-depends-on`; `tools.rs:624,677` report it | reads a header vivi 21 no longer writes |
| Durable member push | `vs:src/projects.rs:1325` `ProjectStore::append` | no format check |
| Hosted member push | `vs:src/projects/hosted_index.rs:349` → `vivi::sync::pull::file_records` | vivi's filer; format gate comes with W3-B5 |

**The vivi half.** vivi's gate is private (`pub(super) fn gate`, `src/mailspace/delivery/compose.rs:35`).
Durable projects have no `Mailspace` on the server, so they cannot call it. Add one public, storage-free
entry in the vivi library and make the private gate call it (one implementation, no second copy):
- `vivi::letter::gate(kind: &str, body: &str) -> Result<Gated, LetterError>` (name is the delivering
  session's call): the format part of today's `gate` and `work` (need/want/task must open with a fence and
  parse; `pump` parses as a record; mail/chat bodies that open with `+++` must parse, prose passes). It
  returns the kind, `formatted: bool`, `depends_on`, and the parsed letter.
- `compose::gate` keeps the storage step (`canonical_wants`, `compose.rs:97-105`) after calling it.
- Optional helper `vivi::letter::FORMAT_HEADER` already exists (`src/letter.rs:238`); expose the version
  string `"1"` as a const too if none is public.
- `serves`/`explores` canonicalization stays on the vivi side. A durable server stores the handle as
  sent; vivi resolves it at read time (`src/mailspace/serves.rs`, ruling of W3B2-31).

**The vivi-server half.**
1. MCP `send` (hosted and durable) takes the sugar arguments vivi's CLI has: `done_when` (string or list),
   `target`, `write_scope` (list), `serves`, `explores`, `question`, `depends_on` (list). It builds the
   body with `vivi::letter::sugar::apply(kind, body, &Sugar{…})` (`src/letter/sugar.rs:51`). A body that
   opens with `+++` is the whole letter and the sugar arguments are refused beside it (vivi's rule).
   Update the `send` schema and description (`vs:src/mcp.rs:361-382`).
2. Durable `compose_send`/`compose_reply` (and rooms, which reuse them): run `vivi::letter::gate` on the
   body; write `X-Vivi-Format: 1` whenever vivi would (need, want, task, pump, mail, chat); **stop writing
   `X-Vivi-Depends-On`** (deps live in front matter). Memo letters keep today's shape until W3-B4.
   `compose_event` keeps today's shape until W3-B4.
3. Transport graph (`letters.rs:166`, `tools.rs:624,677`, `graph` tool): read `depends_on` from front
   matter via `vivi::letter::of_message`. Recommended: a legacy letter (no `X-Vivi-Format`) contributes no
   dependencies, as vivi itself does after B2 (vivi has no `X-Vivi-Depends-On` reader left; grep).
   Update `vs:docs/contract.md:334,411` and the `graph` description (`vs:src/mcp.rs:422`).
4. Durable member push (`ProjectStore::append`, `vs:src/projects.rs:1325`): call
   `vivi::letter::of_message(raw, "")`; refuse a letter that **claims** format 1 and fails to parse, or
   names an unknown format, with `422 letter_format` and the parser's message. **Accept legacy letters for
   now.** Reason: vivi 21 still writes event, memo, rule and note letters without `X-Vivi-Format` until
   W3-B4 lands; refusing legacy now would refuse vivi 21's own events. W3-B4 tightens this (see §3.1).
5. Docs: `vs:docs/contract.md` letter section states the v1 format, the sugar arguments, and the 422.

**Open questions (recommendations).**
- Should durable `send` require front matter for `mail`? No: prose mail passes (ruling V1).
- Should the server's `reply` accept a role reply envelope? Yes, as vivi's mail gate does.

**Files.** vivi: `src/letter.rs` (or new `src/letter/gate.rs`), `src/mailspace/delivery/compose.rs`,
tests beside them. vivi-server: `src/mcp.rs` (schema), `src/mcp/live/write.rs`, `src/mcp/tools.rs`,
`src/mcp/letters.rs`, `src/rooms.rs` (only if the composer signature changes), `src/projects.rs`,
`docs/contract.md`, `tests/mcp.rs`, `tests/mcp_hosted.rs`, `tests/projects_api.rs`, `tests/rooms.rs`.
Watch file size: `vs:src/mcp/letters.rs` is 1979 lines (tests inline); do not grow it further. Put new
code and tests in new modules if needed.

**Tests that must exist.**
- vivi: `letter::gate` unit tests per kind (need without `done_when` refused; prose mail passes; `+++`
  mail with an unknown key refused).
- vivi-server: hosted MCP `send` with `done_when` arguments stores a need whose raw bytes carry
  `X-Vivi-Format: 1` and front matter with `done_when`; the same through a durable project; a need with
  neither front matter nor `done_when` is refused with vivi's message; `depends_on` round-trips through
  front matter into `graph`; no composed letter contains `X-Vivi-Depends-On`; a durable push of a broken
  format-1 letter gets `422 letter_format`; a legacy push is still accepted (until B4).
- Cross-check (the release proof): a need sent through hosted MCP `send`, then `vivi pump send --need
  <handle>` and `vivi pump once` on a test project → intake starts a run (no `flow-letter-invalid`). This
  can be a vivi-server integration test using the vivi library, or a smoke step.

**done_when:**
- `vivi::letter::gate` (or the chosen name) is public and `compose::gate` calls it; vivi `cargo test
  --no-fail-fast` shows no failure tied to the change.
- `grep -rn "X-Vivi-Depends-On" src` in vivi-server finds only test fixtures that model legacy letters.
- Every vivi-server composer listed above writes `X-Vivi-Format: 1` for need, want, task, pump, mail and
  chat letters (a test per composer reads the raw bytes).
- A need sent through hosted MCP `send` with `done_when` is accepted by a v21 pump's intake (test or
  smoke log).
- vivi-server `cargo test --no-fail-fast` (patched to vivi main) exits 0.
- Audit file `w3-c3-audit.md`: CLEAN or RESIDUAL, no open P1/P2.

### 2.5 Release checklist (operator-gated; the delivering session does NOT do this)

The delivering session stops after Phase 1 is merged to both local mains and reports. Then the operator,
in one go:

1. **vivi**: bump `Cargo.toml` to `21.0.0`, `cargo update -p vivi`-style lock refresh, `cargo build
   --locked`, full `cargo test`, one commit (bump + lock), tag `v21.0.0`, push `main` and the tag.
2. **vivi-server**: set `vs:Cargo.toml` `tag = "v21.0.0"`, remove the local `.cargo` patch,
   `cargo update -p vivi`, `cargo test`, the three smokes (`two-checkout-smoke.sh`,
   `project-sync-smoke.sh`, `hosted-smoke.sh`), bump to `2.0.0` (Option A) or `1.12.0` (Option B), one
   commit, tag, push. Update `vs:AGENTS.md:104-111` (tag line) and `vs:CHANGELOG.md`.
3. **Release notes** (vivi `docs/release-v21.0.0.md` or the repo's usual place): the v1 letter format;
   `done_when` hard error; old letters are history; **drain or reset a pump host before upgrading**
   (in-flight runs on legacy needs are not re-checked: `src/pump/flow/intake.rs:196` skips a need with a
   live run); config keys v21 refuses and `vivi config migrate --write`; a hand role needs a pool;
   `vivi pump reset`; and, with Option A, `/v1/mcp` + key reissue.
4. **Hub install**: `sudo -n vivi-install vivi 21.0.0`, then `sudo -n vivi-install vivi-server <version>`.
5. **Hub config** (the copy checked 2026-10-23 had exactly these 12 refused keys):
   `ssh -t hub 'cd / && sudo -u vivi vivi config migrate --write --project /mnt/vivi/projects/faberlang'`
   removes `[pump] planner, plan, reserved_file, fix_prefix`; `[pump.board] planners_max, plan_timeout`;
   `[pump.dispatch] kinds, default_kind, max_attempts, per_tick, overdue_grace, last_resort`.
6. **Main rungs**: hub has `task = './scripta/test --stage 1'` and `need = './scripta/test --stage 4b'`
   but no `main` for targets `default`, `gradus`, `inferentia`, `sandbox`; the pump holds until set
   (W3A-12). **Open operator decision: the command.** Recommended `./scripta/test --stage 4b`, for example
   `sudo -u vivi vivi pump rungs set main './scripta/test --stage 4b' --target default --project
   /mnt/vivi/projects/faberlang` per target (or once at workspace level without `--target` if every
   target shares it). The pump must be paused or stopped (the guard refuses a running pump).
7. **Leftover `gate` role** (kind gate, read-only): optional
   `sudo -u vivi vivi role set gate --status retired --project /mnt/vivi/projects/faberlang`.
8. `sudo -u vivi vivi config validate --project /mnt/vivi/projects/faberlang` exits 0 (or 3 with a
   not-ready list the operator accepts).
9. With Option A: mint new keys on `/v1/mcp` (`key_mint` with `HOST_OPERATOR_TOKEN`, or the self-service
   `POST /v1/projects/{id}/keys`), re-point the claude.ai connector(s) at `<server>/v1/mcp`, and re-key
   any pool `bridge.key` (how hub pools hold bridge keys is **[unverified]**).
10. The hub pump is empty, so no drain or reset is needed. Resume the slow ramp when ready.
11. Sync the canonical skills repo (`~/work/ianzepp/skills/tugboat/` holds its own copy of the tugboat
    skill and `check-task-body.py`) with `vivi/skills/tugboat/` (operator follow-up from round 2).

---

## 3. Phase 2 — the rest of the admitted scope

Order (each is one need with one branch per repo it touches):

| Need | Campaign item | Repo(s) | Size | After |
| --- | --- | --- | --- | --- |
| **W3-D3** | D1 + D2 (endpoint, project arg, keys, grants, audit) | vivi-server | 8-10 | C3 (Phase 1 under Option A) |
| **W3-B4** | B4 (headers transport only; event/memo/rule/mail letters) | vivi (12-16) + vivi-server (2-3) | 14-19 | Phase 1 |
| **W3-B5** | B3's sync-receive half / B5 (quarantine) | vivi (5-7) + vivi-server (1) | 6-8 | B4 |
| **W3-D1** | D3 prerequisite (lib surface) | vivi | 5-6 | Phase 1 (parallel with B4) |
| **W3-E3** | E3 (split ownership lanes) | vivi | 5-6 | Phase 1 (parallel) |
| **W3-D4** | D3 tool set + D4 write guards | vivi-server | 10-12 | D1, D3, E2, E3 |
| **W3-D5** | D5 (`vivi mcp` client) | vivi | 0-1 | verify only (done in round 1) |

Name mapping, to avoid confusion: campaign **D1** (one endpoint) and **D2** (keys, grants, audit) are
need **W3-D3**. Campaign **D3** (tool set) and **D4** (write guards) are need **W3-D4**. Campaign **D5**
(client) was round-1 need **W3-D2** (done). Need **W3-D1** is the vivi lib extraction the tools need.

What of D is done: the client half (D5) entirely; the vivi write-guard helper (E1 delivered
`config_edit::guarded`, `guard_file`, `GuardedWrite`, used by the CLI). What is left: everything on the
server (endpoint, keys, grants, audit, the tool set, routing server writes through the guard) and the
vivi lib extraction.

### 3.1 W3-B4 — headers carry transport only; every writer goes through the gate

**Ground truth** **[code]**. Already moved by B2/B3: `X-Vivi-Depends-On`, `X-Vivi-Parent`,
`X-Vivi-Need`, `X-Vivi-Pump-*` (grep finds no writer or reader in `src/`). Content headers still written:

| Header | Writers | Readers | Target |
| --- | --- | --- | --- |
| `X-Vivi-Verdict`, `-Repo`, `-Branch`, `-Commit` | `src/mailspace/item_events/emit.rs:78-86,340-344,425-439` | `item_events/property.rs:65-68` (fold), `src/sync/link.rs:83-88` (`has_verdict`) | done/reopened event front matter `CloseFront {verdict, branch, commit, repos}` (`src/letter/plain.rs:62-71`) |
| `X-Vivi-Tag` | `src/mailspace/delivery.rs:195` (`save_memo` `:174`) | `src/mailspace/memos.rs:17` | memo front matter `tags` (`plain.rs:20`) |
| `X-Vivi-Rule-Scope`, `X-Vivi-Supersedes` | `src/mailspace/rules.rs:283-287` (`ingest_rule` `:265`) | `rules.rs:532-534` | rule front matter `scope`, `supersedes` (`plain.rs:34-47`) |

Writers that bypass the gate (no `X-Vivi-Format`, no parse check) **[code]**:

| Writer | Path | Callers |
| --- | --- | --- |
| `Mailspace::note_reply` | `src/mailspace/reply.rs:54-93` (`build_compose_draft` + `X-Vivi-Kind: mail` + extras) | move notes `src/mailspace/delivery.rs:881`; pool result notes `src/mailspace/pool_record.rs:202` |
| `save_memo` | `src/mailspace/delivery.rs:174-200` | CLI memo, MCP memo |
| `ingest_rule` | `src/mailspace/rules.rs:265-290` | `vivi rule` |
| lifecycle events | `item_events/emit.rs:399-440` `compose_event_letter` | done/reopened/absorbed/… |
| property events | `item_events/property_emit.rs:63-85,156-190` `compose_property_letter` | marks (`marks.rs:14`), bound (`backlog.rs:297`), dependency-added/removed (`depend.rs:24`), delegation (`delegation.rs:12-15`), state (`lifecycle.rs:184`) |
| `task_from_source` | `src/mailspace/lifecycle.rs:297` | `vivi task from` |

**Target behaviour.**
- Every letter vivi writes carries `X-Vivi-Format: 1` and passes `letter::parse_kind` for its kind before
  ingest. A writer bug is an error at write time, never a bad letter on disk.
- Close metadata (verdict, repo/repos, branch, commit) is front matter of the done/reopened event;
  memo tags and rule scope/supersedes are front matter. Those seven headers are no longer written.
- The fold (`property.rs`), `sync/link.rs` `has_verdict`, memo tag search and rule readers read front
  matter for format-1 letters.
- Event, note, memo and rule letters with an **empty** front store their prose as written, with no `+++`
  fence (consistent with ruling V1: nothing shows an empty fence). `parse_kind` for `Event`, `Memo` and
  `Rule` must then read a fenceless body as an empty front, as it does for mail and chat
  (`src/letter/kinds.rs:106-139`). A body that opens with `+++` is checked strictly.
- vivi-server follows in the same need: durable `compose_event` (`vs:src/mcp/letters.rs:295-382`) and
  memo save (`vs:src/mcp/tools.rs:753-792`) write `X-Vivi-Format: 1` and close metadata as front matter;
  then `ProjectStore::append` (`vs:src/projects.rs:1325`) **refuses legacy letters too** (`422
  letter_format`, "the server accepts only format-1 letters"), which W3-C3 left open.
- A test asserts no writer emits a moved header (grep-style test over composed bytes for every writer).

**Settled.** The event protocol headers stay (ruling 6). No dual write: a fact is in front matter or in a
header, never both.

**Open questions (recommendations).**
1. **Old close metadata.** Existing done events carry `X-Vivi-Verdict` etc. as headers. Recommend: the
   fold reads front matter only for format-1 letters and ignores those headers on legacy letters, as B2 did
   for `X-Vivi-Depends-On`. Already-folded rows in a mailspace stay (the fold persisted them at ingest); a
   fresh clone shows no verdict on old closes. Item state (open/done) is unaffected because
   `X-Vivi-Event` stays. If the operator wants old verdicts visible on fresh clones, the alternative is a
   read-only legacy branch keyed strictly on "no `X-Vivi-Format`" (display only, never execution).
2. **`note_reply` extras.** `extras: &[(String, String)]` lets callers add headers (`reply.rs:60,87`).
   Recommend: remove the parameter; any caller fact becomes front matter of a mail note
   (`MailFront`'s `[pump]` table or a new note key). List each caller's extras first.
3. **Move-note body.** Recommend the move note stays prose mail (no front).

**Files likely touched.** vivi: `src/mailspace/reply.rs`, `delivery.rs`, `pool_record.rs`, `memos.rs`,
`rules.rs`, `lifecycle.rs`, `item_events/{emit,property,property_emit}.rs`, `delegation.rs`, `marks.rs`,
`backlog.rs`, `depend.rs`, `src/sync/link.rs`, `src/letter/{kinds,plain}.rs`, many tests (event fixtures
in `src/sync/property_*_test.rs`, `item_events/*_test.rs`, `tests/item_events_cli.rs`). Watch
`src/mailspace/delivery.rs` (~989 lines; must stay under 1000). vivi-server: `src/mcp/letters.rs`,
`src/mcp/tools.rs`, `src/projects.rs`, tests, `docs/contract.md`.

**Tests that must exist.** Round trip per writer (compose → raw bytes → `of_message` parses, kind right,
front right); property convergence tests (`src/sync/property_*_test.rs`) green; a close with verdict and
commit folds the same values from front matter; memo tag search finds a tagged memo; rule supersedes
works; a raw letter with a moved header and `X-Vivi-Format: 1` is refused (unknown fact in header is not
read, and the front lacks it: decide and pin); server durable push of a legacy letter → 422.

**done_when:**
- `grep -rn '"X-Vivi-\(Verdict\|Repo\|Branch\|Commit\|Tag\|Rule-Scope\|Supersedes\)"' src` in vivi finds
  no writer (readers only inside a documented legacy-display branch, if the operator picks that option).
- Every writer in the bypass table above produces bytes with `X-Vivi-Format: 1` that `letter::of_message`
  parses (one test per writer).
- No stored note, memo, rule or event with an empty front shows a `+++` fence in `vivi mail thread` text
  or `--json` (probe on a temp mailspace).
- vivi-server durable `ProjectStore::append` refuses a letter with no `X-Vivi-Format` (`422
  letter_format`); vivi-server suite green.
- Full suites green on both need branches (no failure tied to the change); audit CLEAN/RESIDUAL with no
  open P1/P2.

### 3.2 W3-B5 — sync receive: quarantine, never silent

**Ground truth.** `Filer::file_record` (`src/sync/filer.rs:97-145`) checks base64, hash, envelope,
forged From, governance, then files. It has **no letter-format check**. Refusals go to
`report.refused` via `refuse` (`filer.rs:520-525`) and are not stored. `PullReport`
(`src/sync/pull.rs:40-...`, `failure()` at `:210`). Hosted vivi-server files member pushes through the
same code (`vivi::sync::pull::file_records`, `src/sync/pull.rs:377`; `vs:src/projects/hosted_index.rs:349`).
The pump's board pass runs `vivi sync pull` as a child process (`src/pump/board.rs:84-95`) and only logs
`sync pull rc=`. Pump alarms use `raise_once` (`src/pump/flow/tick.rs:233`). There is no sync quarantine
today. Name clash: `src/pump/quarantine.rs` is unrelated v19 merge-quarantine leftover code; use the
`sync` namespace.

**Target behaviour** (plan §2.7, re-checked):
- In `file_record`, after the transport checks and before `file_new`: `letter::of_message(bytes,
  handle)`. `Ok` → file. `LetterError::Legacy` → file as history (unchanged). Any other error →
  **quarantine**.
- Held at `.vivi/quarantine/<content_sha256>.eml` (exact bytes) plus `<sha>.json` `{message_id, from, kind,
  sequence, reason, at}`. Files, not a new table. The cursor advances; the letter is held, not dropped.
- One alarm: `PullReport.quarantined` lists them; the pull summary prints `N quarantined: <id>
  (<reason>)`; `failure()` makes `vivi sync pull` exit non-zero. A re-pull of the same content finds the
  file and does not alarm again. On a pump host, after the board pass's pull, the pump raises
  `sync-letter-quarantined` once per content id (`raise_once`). `vivi doctor` sync section
  (`src/doctor/sync_section.rs`) reports the count while any file is present.
- Clearing: `vivi sync quarantine list | retry <id> | drop <id>`. `retry` re-runs the filer on the held
  bytes (after an upgrade). `drop` moves the pair to `.vivi/quarantine/dropped/`. No silent path.
- vivi-server hosted: `file_records` reports quarantined letters; the hosted push answer surfaces them as
  refused so the pushing member learns at once (decide: hosted server refuses rather than quarantines,
  per plan §2.7; recommend refuse, since a server copy must not hold an unreadable letter).

**Open questions (recommendations).**
- Quarantine a format-1 letter with an unknown front key from a **newer** member? Yes; that is the case
  it exists for (version skew).
- Pump alarm source: parse the child's output, or scan `.vivi/quarantine/*.json` after the pull? Recommend
  scanning the directory (machine fact, no text parsing).

**Files.** `src/sync/filer.rs`, `src/sync/pull.rs`, new `src/sync/quarantine.rs` (+ test),
`src/local_sync_command.rs`, `src/cli/sync_command.rs` (new `Quarantine` subcommand; today: Push, Pull,
Resync, Fetch), `src/doctor/sync_section.rs`, `src/pump/board.rs`, skills (`skills/vivi/SKILL.md` sync
section). vivi-server: `src/projects/hosted_index.rs` + a test.

**done_when:**
- A synced format-1 letter with a bad front is stored under `.vivi/quarantine/` with its JSON, is not
  filed, and `vivi sync pull` exits non-zero naming it; a second pull does not alarm again (test).
- A synced legacy letter is filed as history (test).
- `vivi sync quarantine retry <id>` files a held letter once the parser accepts it; `drop` moves it to
  `dropped/` (tests).
- On a pump host the pump raises `sync-letter-quarantined` exactly once per content id (test).
- `vivi doctor` shows the quarantine count (test).
- vivi-server hosted push of a bad format-1 letter is refused and reported (test).
- Full suites green; audit CLEAN/RESIDUAL, no open P1/P2.

### 3.3 W3-D1 — the vivi library surface the server tools need

**Ground truth.** CLI command bodies are `mod`s of the binary (`src/main.rs:14-44`), not the library.
Logic the server tools need that still lives only in bin glue, or prints instead of returning **[code]**:

| Logic | Today | Lift to |
| --- | --- | --- |
| config show / validate / migrate | `src/local_config_command.rs:78-110` (show), `:166-272` (validate + `missing_rung_issues`, `retired_key_issues`, `issues_json`), `:273-391` (migrate, `migrate_inputs`, `legacy_planner_charter`, `write_charters`, `write_backup`) | `vivi::pump::config::{show, validate, migrate}` (or `vivi::config::*`) returning values |
| pool list / show (with bands, harness models, free slots) | `src/local_pool_config_command.rs:19-93`, `row` `:251-277`, `live_free` `:279` | `vivi::pool::config::{list_view, show_view}` |
| flow show (runs and steps of a need) | `src/local_flow_command.rs:48-153` | `vivi::pump::flow::view(project, kind, need)` |
| pump actions | lib, but take CLI structs and print: `vivi::pump::requeue::run(project, &PumpRequeueCommand)` (`src/pump/requeue.rs:37`), `withdraw::run`, `replan::run`, `flow::answer::run`; `post_unlock` in bin (`src/local_pump_command.rs:20-33`) | value-returning entry points with plain arguments (`requeue(project, steps|job)`, `withdraw(project, needs, reason)`, `replan(project, need)`, `answer(project, step, word, note)`, `unlock(project, need, entry, note)`) |
| pump log tail | `src/local_pump_command.rs:113-127` over `pump::logview` | confirm `pump::logview` returns lines with a bound; lift the filter |
| pump reset | W3-E2 delivers it as a lib function | — |

Already library (no work): `config_edit::{set_key, unset_key, guarded}`, `pump::config::targets::{list,
add, remove}`, `RungsConfig::{show, set}`, `Mailspace::{list_role_views, role_view, add_role, set_role,
read_charter, set_charter, pump_send, pump_list, pump_show}`, `pump::health::health`,
`pump::control::set_project_mode`.

**Target.** Each lifted function returns a value (no printing). The CLI becomes a printer over it. CLI
output stays byte-identical: existing CLI tests are the oracle.

**done_when:**
- Each function in the table has a library entry point with a unit test.
- The CLI commands call them; `tests/config_cli.rs`, `tests/pool_cli.rs`, flow and pump CLI tests pass
  unchanged (no expected-output edit).
- Full vivi suite green; audit CLEAN/RESIDUAL, no open P1/P2.

### 3.4 W3-D3 — the server-level MCP endpoint, keys, grants, audit

**Ground truth (vivi-server)** **[code]**:
- Routes `vs:src/lib.rs:112-171`: `POST /v1/projects/{id}/mcp` (`:141`), `/v1/rooms/{id}/mcp` (`:142`, same
  handler), `/v1/host/mcp` (`:144`, `create_project` only, `HOST_OPERATOR_TOKEN`). JSON-RPC in
  `vs:src/mcp.rs:40-305` (`Surface` `:70`), tool list `:319-563`.
- Keys `vs:src/keys.rs`: per project `DATA_DIR/projects/<id>/api_keys.json` (`:107`), ≤32 live keys
  (`:27`), SHA-256 digests, mint/list/revoke/verify (`:137-235`), resolution only inside one known
  project (`resolve_project_caller` `:260`). No global key index. No stored level: operator is derived per
  call (`vs:src/mcp/live/pump.rs:126-150` `authorize`). Rate limit `vs:src/safety.rs:30`. **No audit log.**
- `ProjectMeta` has no name field (`vs:src/projects.rs:98-121`).
- An earlier design note (`vs:docs/phase-03-project-mailspace-hub.md:907-913`: a global endpoint
  lets a leaked key plus an injected `project` reach another tenant) is answered only by the grant check.
  Treat the grant check as security-critical.

**Target behaviour (settled by ruling 3; details from plan §3.1-3.3):**
- `POST /v1/mcp` (Streamable HTTP, JSON-RPC as today). `initialize` instructions name the `projects` tool.
- Removed: `POST /v1/projects/{id}/mcp` and `POST /v1/host/mcp` (clean break). Kept:
  `POST /v1/rooms/{id}/mcp` (settled).
- Auth: `Authorization: Bearer <server key>`. Identity tokens are not accepted on `/v1/mcp`. The HTTP
  letter wire (`/v1/projects/{id}/items…`) keeps identity tokens and keys as today.
- Every project tool has a required `project` (id or name). Missing → tool error `project is required`
  (the text vivi's fake server pins, W3D2-6). Name resolves only among the key's granted projects (hosted:
  project root directory name, else mailspace `config.name`; durable/chat: id only). Two grants with one
  name → error naming both ids.
- `projects` tool (no args): `[{project_id, name?, mode, member, identity, level}]` per live grant; a host
  grant shows as `{host: true}`.
- One key store `DATA_DIR/keys.json` (0600, atomic tmp+rename like `vs:src/keys.rs:117-128`), loaded at
  boot into an index `digest → key`. Principals with keys (digests only) and grants
  `{project_id, member, identity, level: member|operator}` plus `host: bool`.
- Per call: key → principal; for a project tool, the grant for the named project, else `this key has no
  grant on <project>` (same text for unknown projects; pinned by vivi's fake server). The grant's member
  must still be a registered identity; `operator` level must still meet today's rule (host's own member,
  acting role kind operator), re-checked per call.
- Host tools (project create/list, lane create, server version, key/grant admin) need `host: true` and take
  no `project`.
- Bootstrap: `HOST_OPERATOR_TOKEN` acts as a built-in host principal on `/v1/mcp` (host tools only).
- Self-service stays: `POST /v1/projects/{id}/keys` with an identity token mints a server key whose
  principal is `<member>@<project_id>` with one member grant (operator when today's rule holds).
- Reissue: old `projects/<id>/api_keys.json` are not read. At boot each is renamed
  `api_keys.json.retired` with one log line per project naming the count of dead keys.
- Audit: append-only `DATA_DIR/audit.jsonl` (0600), one line per write tool call and per key/grant change:
  `{at, key_id, principal, project_id?, tool, outcome: ok|refused|error, summary}`. Reads are not logged.
- Rate limit: the existing `RateLimiter`, keyed by (project_id or `host`, key_id).
- In this need, the **existing** tools move to `/v1/mcp` with the `project` argument (overview, list,
  show, thread, send, reply, done, reopen, memo, graph, mark, unmark, depend, board, pump, pump_control,
  pump_rungs, pool_bands, pump_send, pump_list, pump_show; host: create_project) plus `projects` and the
  host key tools. New tools are W3-D4.

**Open questions (recommendations).**
- Keep per-project self-service key minting? Yes (plan §3.3); `vivi sync api-key mint` keeps working and
  its answer adds `grants`.
- Durable projects lose identity-token MCP access (keys only on `/v1/mcp`). Accept (clean break); check
  the two-checkout smoke uses `vivi sync api-key mint` keys.

**Files.** `vs:src/lib.rs`, `src/mcp.rs`, `src/keys.rs` (rewrite), new `src/grants.rs`, `src/audit.rs`,
`src/mcp/host.rs` (fold into `/v1/mcp`), `src/main.rs`, `src/projects_api.rs` (key route answers grants),
every `tests/mcp*.rs`, `tests/keys.rs`, `tests/host_create_project.rs`, the smokes (`scripts/*smoke.sh`
use `/v1/projects/$PID/mcp`: `hosted-smoke.sh:126,144`, `mcp-smoke.sh:59`, `two-checkout-smoke.sh:230`),
`docs/contract.md`, `README.md`.

**Tests that must exist.** Call without `project` refused; ungranted project refused with the same text
as an unknown one; operator tool refused on a member grant; host tool refused without host; one audit line
per write; old key file renamed at boot; `vivi mcp tools` and `vivi mcp call overview` from vivi main work
against the real server (an end-to-end smoke step); a negative test per tool family that a key granted on
project A cannot read or write project B.

**done_when:**
- `POST /v1/projects/{id}/mcp` and `/v1/host/mcp` return 404; `POST /v1/mcp` serves every existing tool
  with a required `project`.
- The cross-tenant negative tests exist and pass.
- `vivi mcp call overview` (vivi main binary) against a locally run vivi-server returns the overview for
  the project the key is granted on (smoke log).
- All smokes pass with `VIVI=<vivi worktree>/target/debug/vivi`.
- vivi-server suite green; audit CLEAN/RESIDUAL, no open P1/P2; **one adversarial (black-hat style) pass
  on the grant check** recorded in the audit file.

### 3.5 W3-D4 — the full tool set and write guards (server)

**Target.** Each tool calls the same vivi library function as its CLI command (no second
implementation). Updated from plan §3.4 for what rounds 1-2 removed (`pump_status` is gone with
`pump status`; requeue takes step tasks or `--job`, not units):

| Tool (+ `project`) | vivi function |
| --- | --- |
| pump_log `{file: log|exceptions, lines ≤ 500, filter?}` | `pump::logview` (W3-D1 bound) |
| pump_action `{need?, action: withdraw|requeue|replan|answer|unlock, steps?, job?, step?, word?, entry?, reason?, note?}` | W3-D1 entry points (withdraw closes the need, W7) |
| flow_show `{kind, need}` | W3-D1 `pump::flow::view` |
| config_show / config_validate / config_migrate `{write?}` | W3-D1 lifted functions (migrate write guarded) |
| config_set / config_unset `{key, value?}` | `config_edit::{set_key, unset_key}` (guarded) |
| role_list / role_show / role_add / role_set (band or model + effort, pool, status, cadence) | `Mailspace::{list_role_views, role_view, add_role, set_role}` (writes guarded, as the CLI does after E1) |
| charter_show / charter_set | `Mailspace::{read_charter, set_charter}` (set guarded) |
| pool_list / pool_show | W3-D1 views (bands with model + effort) |
| pool_bands (exists) | move its allow-list check (`vs:src/mcp/live/pool_bands.rs:116-144`) into vivi; write through the guard |
| pool_allow_set `{pool, models}` | `config_edit` pool key write with type check (guarded) |
| target_list / target_add / target_remove | `pump::config::targets::{list, add, remove}` (guarded) |
| pump_rungs (exists) | already `RungsConfig`; confirm guarded |
| pump_reset | W3-E2 `pump::reset::archive` (refused while running) |
| host: project_create (exists), project_list, lane_create (see W3-E3), server_version | `project_create::create`; server registry + `vivi::registry::load`; E3 lane function; server |

**Write guards (campaign D4).** Every config, role, pool, target, charter and rung write from the server
goes through `vivi::mailspace::config_edit::guarded` / `guard_file`
(`src/mailspace/config_edit/guard.rs:53-75`): refused while the project's pump runs (lock held and mode
running; a paused pump is writable), a backup first (last 10 kept), validate after, rollback on a new
error. No server write uses raw `config_edit::apply`.

**Open question.** `lane_create` on hub: the server runs as a user that cannot write the git-owned lanes
folder (admission finding). Recommend the host tool does only the projects-owner half (attach an existing
bare repo as a target) and answers the exact CLI command for the lanes-owner half (see W3-E3).

**done_when:**
- Every tool in the table exists on `/v1/mcp`, has a test that it calls the vivi function (same result as
  the CLI on the same fixture), and every write tool has a refused-while-running test and an audit line.
- `grep -rn "config_edit::apply" src` in vivi-server finds nothing.
- vivi-server suite and smokes green; audit CLEAN/RESIDUAL, no open P1/P2.

### 3.6 W3-E3 — lanes when folders have split owners

**Ground truth.** `project_create::create` (`src/project_create.rs:139`, `create_at` `:147`) does both
halves in one call: bare repo `<lanes_root>/<name>.git` via `git init --bare --shared=group`
(`:185,252`), mailspace, pool values, optional seed commit, registry, rollback. No ACL handling, no target
table, no clone, no way to add a lane to an existing project. CLI `src/cli/project_command.rs:9-51` has
only `Create`. On hub the lanes folder `/mnt/vivi/lanes/<group>` belongs to `git` (ACL controlled) and
projects to `vivi`; no single user can do both halves. Whether hub's lanes use **default ACLs** on the
group folder (inherited by `mkdir`) is **[unverified]**: if they do, no ACL copy code is needed.
`vivi pump target add` exists (E1, `src/pump/config/targets.rs:77`).

**Target behaviour (recommendation; the campaign forbids "a second tool for each half").**
- One command: `vivi project lane add <project-root> <target-id> [--lanes-root R] [--git-url-prefix P]
  [--seed]`. It does every half the caller can do:
  - lanes half (caller can write the lanes root): create `<R>/<target-id>.git` bare,
    `--shared=group`, copy group and ACL from an existing sibling repo (`getfacl`/`setfacl` on Linux;
    skipped with a warning elsewhere), optional seed commit.
  - projects half (caller can write the project): clone into the pump target path and add
    `[pump.targets.<id>]` through `pump::config::targets::add` (guarded).
  - Prints the half it could not do, as the exact command for the other user.
- `vivi project create` gets the same split behaviour: it does what the caller can do and prints the rest
  (no `--repo-only` flags needed). The rollback rule stays: undo only what this run created.
- Library functions for both halves (W3-D4's `lane_create` calls the projects half).

**Open questions.** Inspect hub's lanes ACLs before writing ACL code (operator or a read-only probe:
`getfacl /mnt/vivi/lanes/<group>`). If default ACLs exist, skip the ACL copy.

**Files.** `src/project_create.rs` (split into halves sharing validation), `src/cli/project_command.rs`,
`src/local_project_command.rs`, tests with temp dirs (ACL test behind `cfg(target_os = "linux")`), skills
project section.

**done_when:**
- In a temp tree where one test user can write only the lanes root, `vivi project lane add` creates the
  bare repo and prints the projects-half command; where it can write only the project, it clones and adds
  the target and prints the lanes-half command; where it can write both, it does both (tests).
- A failure in either half leaves nothing it created (test).
- Full vivi suite green; audit CLEAN/RESIDUAL, no open P1/P2.

### 3.7 W3-D5 — `vivi mcp` client (verify only)

Done in round 1 (need `W3-D2`, 8 slices, merged). The client posts to `/v1/mcp`, uses an API key only
(`--name`/`--key-file` removed), fills `project` for tools whose schema declares it, and the stdio bridge
injects the configured project (`src/mcp/inject.rs`). **Left:** prove it against the real server once
W3-D3 lands (W3-D3's smoke step covers it). No code is expected. If the proof fails, the fix belongs to
whichever side is wrong.

### 3.8 Docs and skills in every need

Ruling V4 holds for new work: each need updates `skills/vivi/**` (and README where it lists commands) for
what it adds or changes, in v21 wording, in the same need. `cargo test --test skills_cli` stays green.
vivi-server needs update `vs:docs/contract.md`, `README.md`, `AGENTS.md` and `CHANGELOG.md` for what they
change.

---

## 4. Follow-ups that are NOT in scope

Each item is **follow-up, not campaign scope**. Do not fix them inside a need unless the need's own change
breaks them. File them as their own needs if the operator wants.

| Item | Where | Source |
| --- | --- | --- |
| Pump never sets `st.board.sync_pending`, so it never pushes flow letters itself | `src/pump/board.rs` | W3V-12 note |
| Pump health verdict ignores `flow-*` exceptions | `src/pump/health.rs` | V audit P3-5 |
| `AttemptFields.merged_at` always `None` (report merged columns never fill) | `src/pump/attempt_read.rs` (report columns) | W3V-19, V audit P3-7 |
| A wrong-type value under a refused `[pump.workspace]` key still stops every command, migrate included (typed `PumpWorkspace` field still parsed) | `src/pump/config_workspace.rs` | V audit pass 2 N1 |
| `config_workspace_test.rs` keeps vacuous tests | `src/pump/config_workspace_test.rs` | V audit N2 |
| `vivi pump requeue --job` loads the v19 `State` for held-job data | `src/pump/requeue.rs` | W3V-14 |
| Shared answer words name only the first role; word table drift-tested one way | `src/letter/kinds.rs:186-201` | B3 audit N1, N2 |
| A faulty judge `residual` is stored as an auditor reply | pump flow collect | B3 audit N3 (pre-existing) |
| `tested:` written twice in the repair need body | `src/pump/flow/main_rung/red.rs:237` | B3 audit P3-6 |
| `role = "tasks"`, `kind = None` path with no production caller; FakeMail `marked_replies` differs from the real port | compose / test fake | B3 audit P3-7, P3-8 |
| Clippy red tree-wide (~338 errors) although `src/lib.rs` has `#![deny(clippy::pedantic)]` | whole tree | ledger (round 1) |
| Flaky `pump::control_test::stopping_a_live_current_pump_writes_control_and_legacy_signal` and `…old_schema…` (`lock.try_lock` WouldBlock, `:73`, `:88`) | `src/pump/control_test.rs` | seen 4 times; sideband |
| `vivi monitor --help` / `--once` clap panic: `--project` used by both `projects` and `project` | `src/cli/monitor_command.rs` | [ran] |
| `vivi pool create` makes slots serially (~1.3 s each) | `src/pool/local_slots.rs:288-294` | sideband finding |
| `unused_must_use` warnings (an ignored `Result`, e.g. on `unclaim`) and other compiler warnings (deprecated `fetch_update` at `src/mailspace/pump_note.rs:30`, unused `mut` at `item_events/property_emit.rs:49`) | various | W3B3-25 note, [ran] |
| Held Q-A planning code warns dead (`amend.rs` `HASH_SCHEME`, split code) | `src/pump/amend.rs`, `plan.rs` | **out of scope by ruling** (held) |
| `letter::is_false` possibly unused | `src/letter.rs:263` | W3V-17 note |
| File sizes: `migrate_test.rs` ~1130, `run_test.rs` ~1165, `tick_test.rs` ~998, `delivery.rs` ~989 lines | vivi | audits |
| `pool set` stores untyped strings and relies on rollback | `src/mailspace/config_edit/pool.rs` | E1 note |
| A7 residual: stale `last_error` after a revert to the running config fingerprint | `src/pump/run/reload.rs` | W3A-7 |
| Commit `05247fca` subject says board `parse_rows` gone; it is not (no action possible) | history | V audit N3 |
| Design and history docs naming removed surfaces (`docs/cross-mailspace-minds-acceptance.md`, `docs/mailspace-agent-cycle-improvement-requests.md`, `docs/pump-*`) | `docs/` | W3V-20 (stale by default) |
| Canonical skills repo copy of tugboat skill / `check-task-body.py` | `~/work/ianzepp/skills/tugboat/` | round 2 (operator) — also listed in §2.5 |

---

## 5. How the delivering session works

**Process (the pattern rounds 1-2 used).**
1. One **need branch** per need per repo: `need/w3-<name>` cut from that repo's local `main` (vivi:
   `~/work/ianzepp/vivi`; vivi-server: `~/work/ianzepp/vivi-server-w3/main`).
2. Slice each need into small tasks (one writer of a file at a time; writer and reader of one fact in one
   task). One **worktree per task** (vivi: `.claude/worktrees/w3-<task>`; vivi-server:
   `~/work/ianzepp/vivi-server-w3/w3-<task>`, outside the repo), branch `w3/<task>`, cut from the need
   branch tip.
3. Implementers run **focused checks only** (`cargo test <filter>`, `cargo check --all-targets`,
   `cargo fmt`). Several implementers running the full suite at once timed out at 600 s in round 2.
4. Land each task on its need branch with a **merge commit** (`git merge --no-ff w3/<task>`). Run
   `cargo fmt` and fix what it reports before landing.
5. When every task of a need has landed: one **full suite** on the need branch,
   `cargo test --no-fail-fast` (vivi takes about 4 minutes, 230-270 s measured on a quiet machine;
   vivi-server about 15 s to 2 minutes). Bound it at 10 minutes. Record rc, duration, pass/fail counts.
   One full suite at a time per machine.
6. One **independent audit** per need (a separate agent that did not implement it): correctness,
   done-when evidence, no test weakened, stale text. Verdict CLEAN, RESIDUAL or BLOCK with P1/P2/P3
   findings. P1/P2 are fixed inside the need (new tasks), then a second audit pass. P3s go to the ledger.
7. Then **`git merge --no-ff need/w3-<name>`** into that repo's local `main`. After a merge, run the full
   suite on `main` once (the "main rung").
8. vivi-server work builds against vivi through the `.cargo/config.toml` path patch (§2.2), pointed at a
   detached vivi worktree at vivi local `main`. A vivi-server need that needs new vivi code waits until
   that vivi code is merged to vivi local `main`.

**Rules.**
- **Never `git stash`.** Never `git reset --hard`, `git clean`, `git checkout --`, `branch -f`, or force
  anything on `main` (hooks block them; recover with additive commits).
- Path-limited commits: `git add -- <paths>` then `git commit -- <paths>`. Never stage `Cargo.lock` of
  vivi-server while the path patch is active. Never commit `.cargo/`.
- Commit identity: `Ian Zepp <ian.zepp@protonmail.com>`. End each commit message with
  `Co-Authored-By: <your model name> <noreply@anthropic.com>`.
- **No push, no tag, no version bump, no install.** Not on vivi, not on vivi-server, not on hub.
- Foreign dirt in a tree (another agent's uncommitted work) is never erased: classify it, work around it.
- Forward progress over green: an unrelated red (for example the known `pump::control_test` flake) is a
  sideband item, not a blocker. A red tied to the need blocks the need.
- A test is never weakened to get green. When a test pins behaviour a ruling removed, change the test to
  the ruled behaviour and say so in the commit.
- Slow is a finding: a full suite over 10 minutes is reported with the measurement, not rerun with a
  bigger timeout.
- Report merges as **count + short hash + subject** per repo (for example `vivi: 3 merges — a1b2c3d4
  merge need/w3-e2: vivi pump reset; …`), or say why there are none.
- Keep a **ledger** (append-only Markdown) of tasks dispatched, landed commits, test runs (rc, duration,
  counts), audit verdicts, findings, deviations from this file, and operator questions.

---

## 6. Verification protocol (what the checking session will do)

### 6.1 Evidence the delivering session must leave

For each need, in one completion note (or ledger section) per need:
- the need branch, its task commits (short hash + subject) and the merge commit on `main`;
- each full-suite run: command, tip commit, rc, duration, passed/failed/ignored counts, and any failure
  with its classification (tied to the need, or unrelated sideband);
- the audit file path (`w3-<need>-audit.md`) and each pass's verdict and P1/P2 list with their fixes;
- deviations from this file and why (code wins over this file);
- open questions answered, with the answer chosen.

### 6.2 Checks per need

The checker reruns these on the final local mains (vivi `main`, vivi-server `main`), not on the
delivering session's word:

| Need | Probe |
| --- | --- |
| W3-C21 | In a fresh vivi-server worktree of `main` with the path patch to a detached vivi worktree at vivi `main`: `cargo test --no-fail-fast` rc 0. `grep -rn "body_has_labeled_clause\|with_pump\|complete_open_needs" src tests docs` empty. `VIVI=… scripts/hosted-smoke.sh` and `scripts/project-sync-smoke.sh` pass. |
| W3-E2 | Temp project: `vivi pump reset` on a stopped pump → one `pump-archive-<stamp>/` holding `pump.db`; new `pump.db` control `stopped` (`vivi pump health` or `inspect`); `targets/` untouched. With the lock held (`flock` on `<home>/lock` from a test helper or a running `vivi pump run`) → non-zero exit, no file moved. `vivi pump --help` lists `reset`. |
| W3-C3 | Hosted fixture: MCP `send` `{kind: need, done_when: ["x"], …}` → raw letter has `X-Vivi-Format: 1` and `done_when = ["x"]` front matter; `vivi pump send --need <h>` + `vivi pump once` → a run starts, no `flow-letter-invalid` exception. Durable fixture: same send → raw bytes formatted, no `X-Vivi-Depends-On`; push of a broken format-1 letter → `422 letter_format`. |
| W3-D3 | Local vivi-server: `/v1/projects/<id>/mcp` → 404; `/v1/mcp` without `project` → `project is required`; key granted on A calling a tool on B → `this key has no grant on <B>`; vivi main `vivi mcp call overview --project-id <A>` (or `[sync] project`) answers; `DATA_DIR/audit.jsonl` gains one line per write; an old `api_keys.json` is renamed `.retired` at boot. |
| W3-B4 | Temp mailspace: close a need with verdict and commit, save a tagged memo, add a rule with supersedes, mark a need → each raw letter has `X-Vivi-Format: 1`, parses with `of_message`, carries the fact in front matter, and none carries the seven moved headers. `vivi mail thread` shows no empty `+++` fence. vivi-server durable push of a letter without `X-Vivi-Format` → 422. |
| W3-B5 | Two temp mailspaces syncing through the fake server (or vivi-server): push a format-1 letter with an unknown front key from A (bypassing A's gate with a raw ingest helper); pull on B → `.vivi/quarantine/<sha>.eml` + `.json`, pull exit non-zero naming it; second pull silent; `vivi sync quarantine drop <id>` moves it to `dropped/`. |
| W3-D1 | `cargo test --test config_cli --test pool_cli` (and flow/pump CLI tests) pass with no expected-output change in the diff (`git diff <base>..main -- tests/` shows no output edits for those). Lib tests for each lifted function exist. |
| W3-D4 | Each new tool called over `/v1/mcp` on a fixture returns what the CLI returns on the same fixture; each write tool refused while the pump runs; `grep -rn "config_edit::apply" src` in vivi-server empty. |
| W3-E3 | The three ownership cases of §3.6 in temp dirs (tests), and on Linux the ACL copy test. |
| W3-D5 | Covered by the W3-D3 probe. |

### 6.3 Whole-campaign checks

- vivi local `main`: `cargo test --no-fail-fast` rc 0 (or only known sideband flakes, each named), under
  10 minutes; `cargo fmt --check` clean; `cargo test --test skills_cli` green.
- vivi-server local `main` (patched to vivi `main`): `cargo test --no-fail-fast` rc 0 and the three
  smokes pass.
- `git log --oneline` on both mains shows one no-ff merge per need, each with an audit file and a green
  full-suite record in the ledger.
- Nothing pushed: `git -C ~/work/ianzepp/vivi status -sb` and the vivi-server worktree show `ahead` of
  `origin`, and `git ls-remote --tags origin` has no `v21.0.0`.
- Skills: `grep -rn "promote\|pump status\|closeout\|PUMP-PLAN-BEGIN\|X-Vivi-Depends-On" skills README.md`
  in vivi finds no v21-current instruction that names a removed surface.
