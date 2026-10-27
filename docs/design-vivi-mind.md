# Delivery: `vivi mind`, a persistent hub-wide supervisor

Status: **delivered supervisor**. The spike this document was first written
for (2026-10-25) has been followed by the delivery units recorded below, and
this document now describes the implemented action contract, not a plan.
Live evidence is labeled where it exists and comes from the e2e sandbox;
no production deployment is claimed, and no release has been cut — version
bumps and release notes remain the operator's separate call.

To stand it up on a box, follow
[`vivi-mind-quickstart.md`](vivi-mind-quickstart.md).

## Interpreted Unit

The Linux hub runs one pump per registered project. When the operator (Ian)
is away, a pump can get stuck: a wedged writer, a locked flow run, a step held
for an answer nobody gives, a red main, a repair loop raising alarms, or a
plan job pending on zero free slots. Today it stays stuck until someone looks.

`vivi mind` is one long-lived service per hub with two jobs:

1. **Supervise.** Watch every registered pump and get stuck pumps unstuck,
   with judgment: fix the problem within its authority, or pause the pump and
   escalate to the operator.
2. **Converse.** Host the operator's design conversations about the pump, in
   the same session the supervisor uses, so the context carries over.

It must be independent of the pump. A deadlocked or crashed pump must not take
the mind down, so the mind is **not** a pump cadence job. Cadence jobs are also
forced read-only (`finish_cadence_spec` in `src/pump/cadence.rs` sets
`spec.read_only = true`), so they could not unstick anything anyway.

Delivered shape: a hardened service that resumes one pi session across
restarts and SSH drops, wakes on a heartbeat and on real stuck signals, and
acts only through the mail server's MCP tools within an operator-chosen
authority stage and budget. Execution without a configured server is a
deliberate dry run, not an accident: with no `[mcp]` table every permitted
action is logged as what would run, and nothing runs.

## Normalized Spec

### Commands

The service verbs mirror `vivi pump service`.

| Command | What it does | Status |
| --- | --- | --- |
| `vivi mind` / `vivi mind attach` | Attach a line REPL to the running mind's one session. Lines are prompts. `/status`, `/pause`, `/resume`, `/compact`, `/history` and `/quit` are verbs. | Real (socket client) |
| `vivi mind status [--json]` | Authority, paused, busy, session id, and last wake. Falls back to the state file when no mind is listening. | Real |
| `vivi mind log [-n N]` | Tail of `<mind home>/mind.log`. | Real |
| `vivi mind pause` / `resume` | Stop or restart autonomous wakes and mutating actions. Attach keeps working. Goes through the socket, or through the state file when the mind is down. | Real |
| `vivi mind stop` | Ask the running mind to exit. The service manager decides whether to restart it. | Real |
| `vivi mind start` | Starting is the service manager's job. | Stub (explanatory error) |
| `vivi mind compact` / `history [--limit N]` | Forwarded to the pi session. | Real over the socket, checked live against pi 0.73.1. |
| `vivi mind service add\|remove <root>` | Watch or ignore a project that is already registered. This edits the mind's own ignore list and never touches the host registry. | Real |
| `vivi mind service list` | Registered projects and whether the mind watches each. | Real |
| `vivi mind service status` | Whether a mind is listening, plus every path it uses. | Real |
| `vivi mind service run` | The foreground service, for a service manager. | Real loop, run live on the e2e sandbox against pi 0.73.1. |
| `vivi mind service unit` | Print an example hardened systemd unit. Installs nothing. | Real (template) |
| `vivi mind service install\|uninstall` | OS service install. | Stub (explanatory error) |

`vivi mind probe [--json]` is a diagnostic: one read-only probe pass plus the
wake decision, with no running mind, no turn, and no write.

### Architecture

```text
             operator (ssh)                       hub, user `vivi`
  ┌───────────────────────────┐       ┌──────────────────────────────────────┐
  │ vivi mind  (thin client)  │──────▶│ <mind home>/mind.sock  (0600, owner) │
  └───────────────────────────┘ unix  │                                      │
                                      │ vivi mind service run                │
                                      │  ├─ wake loop (60 s tick)            │
                                      │  │   probe ─▶ wake::decide           │
                                      │  ├─ turn lock ─▶ pi --mode rpc       │──▶ 127.0.0.1:18180
                                      │  │              (one session)        │    vivi proxy
                                      │  └─ envelope ─▶ authority gate       │
                                      │                 ─▶ actuator          │──▶ one MCP tool call
                                      │    (no [mcp]: DryRun log)            │    per permitted action
                                      └──────────────────────────────────────┘
```

The modules are listed under Repo-Aware Baseline.

#### Persistence and the one session

All context lives in **one** persistent pi harness session. Pi sessions are
JSONL files under `--session-dir`, resumable with `--session <id>`, and pi has
compaction built in. The scenario this has to support: the operator drops SSH,
comes back ten minutes later, runs `vivi mind`, and is back in the same
session. No tmux, and no choosing between sessions.

- The mind records the session id in `state.json` the first time it starts
  (pi's `get_state` reports `sessionId`) and passes `--session <id>` on every
  restart once the session file exists (pi writes it at the first message).
- If pi crashes, `respawn_if_exited` restarts it on the same session. The mind
  process itself stays up.
- Attach is stateless. Each `vivi mind` is a new socket connection to the same
  session (covered by `prompts_stream_through_one_session_across_reconnects`).

#### Attach and the access gate

Attach goes through a Unix socket at `<mind home>/mind.sock`. Nothing listens
on the network.

- The socket is chmod `0600`, and a parent directory the mind creates is
  `0700`. The file mode is the access gate.
- As a second check, the server compares the peer uid (`SO_PEERCRED`) with the
  socket file's owner.
- `bind()` refuses to start while another mind answers on the socket. It
  removes a stale socket file, and it never removes a non-socket file.
- The protocol is LF-delimited JSON. Requests are
  `{"op":"prompt","text":..}`, `status`, `pause`, `resume`, `stop`, `compact`
  and `history`. Replies are `text`, `status`, `ok` and `error`, and every
  request ends with `{"event":"done"}`.

#### Models

The mind reaches models only through the hub's vivi proxy on loopback
(`127.0.0.1:18180`, `proxy::config::DEFAULT_LISTEN`) and holds no model keys.

- `mind.toml` refuses a `proxy_url` that is not loopback.
- `ProcessPi` starts pi with a cleared environment: `PATH`, `HOME` (the mind
  home) and `VIVI_PROXY_URL` only, and with `--no-tools`, so pi's own bash
  and edit tools are off and the envelope is the only action path.
- pi reaches the proxy through a custom provider in
  `<mind home>/.pi/agent/models.json` (pi's `HOME` is the mind home) whose
  `baseUrl` is the proxy route and whose `apiKey` is a placeholder; `model`
  in `mind.toml` names it as `<provider>/<id>`. The quickstart has the file.

#### Runs as, and hardening

The mind runs as the same `vivi` user the vivi-server runs as. It takes no
browser or web inputs, and its network is limited to loopback.

`vivi mind service unit` prints `src/mind/vivi-mind.service.example`, an
example only. Its main directives:

- `User=vivi` and `UMask=0077`
- `ProtectSystem=strict` and `ProtectHome=read-only`, with `ReadWritePaths`
  limited to the mind home and the project roots
- `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`, with
  `IPAddressDeny=any` and `IPAddressAllow=localhost`, so the proxy is the only
  way out
- `NoNewPrivileges` and an empty capability set
- `SystemCallFilter=@system-service` and the usual kernel `Protect*` set
- `MemoryDenyWriteExecute=no`, because pi runs on a JIT runtime

Nothing installs it.

### Wakes

The wake loop ticks every 60 s and calls `wake::decide`. The mind wakes when
either:

- **The heartbeat is due.** The mind is treated as a seat whose last durable
  signal is its last finished wake, and `role_schedule::evaluate(heartbeat,
  last, now)` decides. `never`, `due` and `overdue` wake. The default
  heartbeat is `30m`.
- **A pump signal is new.** A project is alarming when any of these hold:
  - `pump health` says `degraded` or `stalled`;
  - a live flow run waits at a lock-class node of its shipped flow (the check
    `vivi pump unlock` uses): `locked` in the build flow counts a locked need,
    `unrepaired` in the repair flow sets red main and the repair alarm;
  - a `flow-envelope-failed` exception was raised in the last hour;
  - a pool job has been pending for more than 30 minutes;
  - a step is `running` with no dispatched job while its pending job has
    waited more than 3 minutes (`step_jobs` maps jobs to steps);
  - the tail of `<pump home>/log.txt` (last 64 KiB) shows a still-pending
    job's dispatch deferred 3 or more times, or a tick step such as
    `dispatch-jobs` failing 3 or more times with one failure in the last 50
    lines (dispatch and pool config errors land here);
  - steps are held for an operator answer (no reader yet: always zero).

  A signal wakes the mind only if its fingerprint was not seen by the last
  wake. The fingerprint carries the newest envelope failure's time, not a
  windowed count, so an ageing count does not re-wake. A pump that stays
  stuck is looked at again on the heartbeat, not on every tick.

It stays asleep when the mind is paused, the last wake is still in flight,
the last wake failed and its retry time (`cadence::backoff_secs(attempt)`)
has not passed, or the rolling-hour cap (`max_wakes_per_hour`, default 6) is
reached. A wake left open by a mind that died mid-turn is closed as failed
when the service starts, so it cannot block wakes forever.

The registered projects come from the host registry,
`<config_dir>/vivi/pump-service.toml` (`crate::registry`), filtered to
`pump = true` and minus the mind's ignore list.

**Why not `pump health` alone.** On the e2e sandbox (2026-10-25), `vivi pump
health` and the MCP `pump` tool reported `ok` for a pump with a need locked
at its operator gate and a plan job pending on zero slots. The probe
therefore reads `pump.db` read-only (`PumpStore::open_read_only`, schema
checked) for live runs and their steps, exceptions, and pool jobs. A debug
build of `vivi mind probe` against that sandbox reported verdict `ok` with
`locked=2` (need2 `2025e1c1` and need3 `2f28cb94`, both waiting at `locked`
in the build flow) and decided to wake. It made no model call, wrote nothing,
and did not touch the sandbox's processes.

A second live stall: a step sat `running` for about four minutes with zero
dispatches, because every dispatch was refused with a pool config error
(`harness.models holds an empty slug ...`), and `pump health` still said
`ok`. That is what the idle-step and dispatch-failure signals catch. When
the probe was re-run (16:38 EDT) the pool had a real model, so it reported
`idle=0 deferring=0`. Unit tests cover the signals, using fixtures shaped
like the pump's own log lines. A running step with **no job at all** cannot
be aged, because `pump.db` keeps no step timestamps (U5).

## Authority, staged

The operator sets the stage explicitly in `mind.toml` (`authority = ...`) and
opens each one deliberately. Each stage includes the powers of the stages
below it. This build opens stages 1–3; the config loader refuses 4 and 5
(their names are settled so the config shape will not change later).

| Stage | Opens | Status |
| --- | --- | --- |
| 1 `observe` (default) | The eight project reads; letters and escalations to the operator. | Delivered. |
| 2 `unstick` | Pause/resume a pump; requeue, replan, answer, unlock within budgets; reply on a need's thread; set a need's priority; withdraw a need; retune a role's model or a pool's cap or band. | Delivered. |
| 3 `file-fixes` | File a new need or a fix need to a watched project through the server's `send`, and dispatch a filed need. | Delivered. |
| 4 `upgrade-on-approval` | `upgrade`: take a release the operator cut. | Defined, refused by the loader. The action cannot be planned in this build. |
| 5 `self-release` | `self-release`: cut releases itself. | Defined, refused by the loader. The action cannot be planned in this build. |

### The envelope

The model never runs anything itself. A turn ends with a TOML envelope
between `+++` lines (a `+++ TOML` open marker is accepted), the shape cadence
head roles already use:

```text
+++
[[action]]
kind = "requeue"
project = "/work/app"
need = "3f9a1c2e"
step = "b71d04aa"
note = "gate flaked on a network timeout; first requeue"
+++
```

The mind decides what happens to each `[[action]]`:

- `deny_unknown_fields`: a field a kind never reads is refused, as is an
  unknown key. A blank string field is an absent one.
- Handles and answer words must be short `[A-Za-z0-9_-]` tokens that do not
  start with `-`, so flag and shell injection is refused.
- `project` must be a watched root — for **every** action, reads and
  letters included. There is no project-free action.
- Notes are capped at 500 characters, and an envelope at 8 actions.
- Budget is spent as permitted actions pass the gate, before they run, so two
  requeues of one need — or two filings to one project — in one envelope
  count twice. A later server-side refusal does not refund the spend.
- The gate's verdicts are logged and replayed to the session as
  `Action results:` (see The operating sequence).
- `ActionRequest::argv()` still renders the `vivi pump ...` line that
  pause/resume/requeue/replan/answer/unlock correspond to, as a diagnostic.
  It has no production caller: execution is MCP tool calls, not the CLI.

### Stage / action / accepted-field matrix

This table matches `authority::Action` (stage and budget), the field
validator (`actions::fields`), and the filing checks (`actions::filing_fields`).
`project` is required for every kind and is not repeated. `prompt/menu.rs`
publishes the same matrix to the model, and a test compares menu and
validator so they cannot drift.

| Kind | Required besides `project` | Optional | Budget bucket | Server tool |
| --- | --- | --- | --- | --- |
| **Stage 1 `observe`** | | | | |
| `read-status` | — | `need`, `status`, `limit` | — | `pump_list` |
| `read-log` | `file` | `lines`, `filter` | — | `pump_log` |
| `read-need` | `need` | — | — | `need_show` |
| `read-need-thread` | `need` | `limit`, `before` | — | `need_thread` |
| `read-task-thread` | `step` | `limit`, `before` | — | `task_thread` |
| `read-flow` | `flow` | `need` | — | `flow_show` |
| `read-config` | — | — | — | `config_show` |
| `read-pool` | `pool` | — | — | `pool_show` |
| `letter-to-operator` | — | `need`, `note` | — | `send` (`kind=mail`, to `operator`) |
| `escalate` | — | `need`, `note` | — | `send` (`kind=mail`, to `operator`) |
| **Stage 2 `unstick`** | | | | |
| `pause-pump` | — | — | — | `pump_control` `pause` |
| `resume-pump` | — | — | — | `pump_control` `resume` |
| `requeue` | `need`, `step` | `note` | need | `pump_action` `requeue` |
| `replan` | `need` | — | need | `pump_action` `replan` |
| `answer` | `need`, `step`, `word` | `note` | need | `pump_action` `answer` |
| `unlock` | `need` | `entry`, `note` | need | `pump_action` `unlock` |
| `reply-need` | `need`, `note` | — | need | `reply` |
| `set-priority` | `need`, `priority` | — | need | `pump_priority` `set` |
| `withdraw-need` | `need`, `note` | — | need | `pump_action` `withdraw` |
| `set-role-model` | `role`, `model` | — | project | `role_set` |
| `set-pool-cap` | `pool`, `model`, `cap` | — | project | `config_set` |
| `set-pool-band` | `pool`, `band`, `model`, `reasoning` | — | project | `pool_bands` `set` |
| **Stage 3 `file-fixes`** | | | | |
| `file-need` | `to`, `subject`, `body`, `done_when`, `target` | `write_scope` | project | `send` (`kind=need`) |
| `file-fix-need` | same as `file-need`, but the watched root must be named `vivi` and `target` must be `vivi` | `write_scope` | project | `send` (`kind=need`) |
| `dispatch-need` | `need` | `note` | need | `pump_send` |
| **Stages 4–5 (refused)** | | | | |
| `upgrade`, `self-release` | — | — | — | never planned: the loader refuses their stages |

### Field and content bounds

Validation is per field, per kind; the first offending field is refused and
the refusal reaches the session on the next turn:

- `need`, `step`, `word`, `entry`, `role`, `to`, `pool`, `target`, `before`:
  plain handles — ASCII alphanumeric, `-` or `_`, at most 64 characters, no
  leading `-`.
- `model`: a model slug (ASCII alphanumeric plus `. _ : / @ + -`).
- `note`: at most 500 characters. `subject` (filing): at most 200.
- `body` (filing): at most 16 000 characters, and must not begin with `+++`
  (no front matter inside a filed body).
- `done_when`: 1–8 non-blank clauses of at most 500 characters each.
- `write_scope`: at most 32 paths, each at most 256 characters, relative and
  climbing no higher than the project root (no `..`).
- `limit` (reads): 1–100. `lines` (read-log): 1–500.
- `status`: `open` \| `done` \| `all`. `flow`: `build` \| `research` \|
  `release` \| `repair`. `file`: `log` \| `exceptions`.
- `priority`: 0 or more (0 is a real level: none). `cap`: 0 to
  `u32::MAX` (0 is a real cap).
- `band`: `P0`–`P5`. `reasoning`: 1–32 characters of ASCII alphanumeric,
  `.`, `_` or `-`.
- Envelope: at most 8 `[[action]]` tables; unknown keys refused.

### The operator grant

The mind acts the way any MCP client does: it presents its own server key on
every call, and the server re-checks per call that the key carries the named
project's **operator grant** (minted by the host's own member and acting as a
role of kind operator). A member token or any other key is refused by the
server, not by the mind. Because authorship is the authenticated grant, the
mind sends no `from` on letters, replies, filings or dispatches — the server's
grant record decides who the letter is from. A permitted call the server
refuses anyway comes back as the tool's error text, bounded, and is shown to
the session on the next turn; the budget it spent is not refunded.

### One MCP call per action

`McpActuator` turns one permitted action into exactly one tool call; absent
optional fields are left out of the arguments rather than sent as nulls, so
server defaults hold (`config_show` and `pool_show` never ask for
`resolved`). Filing and dispatch calls carry `project` as informational
context, like every other call. `upgrade` and `self-release` are refused
before any call ("not wired to the server yet"); their stages never open in
this build.

With **no** `[mcp]` table the actuator is `DryRun` on purpose: each permitted
action is logged as a `would ...` description (the same operation name and
fields the real call would carry, plus `project=` and the note), and nothing
runs. `DryRun` also feeds nothing back, so a dry-run wake ends after its
first turn.

### Budgets: need versus project

One window rule (`per_need_actions` per `per_need_window`, defaults 3 / 24 h)
covers two buckets:

- **Per need.** The need-scoped moves — `requeue`, `replan`, `answer`,
  `unlock`, `reply-need`, `set-priority`, `withdraw-need`, `dispatch-need` —
  draw down the bare need handle's own window.
- **Per project.** Filing (`file-need`, `file-fix-need`) and role/pool tuning
  (`set-role-model`, `set-pool-cap`, `set-pool-band`) draw down one shared
  window per watched project, on a reserved `project:<root>` ledger key that
  cannot collide with a handle.
- **Unbudgeted.** The reads, `letter-to-operator`, `escalate`, `pause-pump`
  and `resume-pump` spend nothing.

### Pause is the brake

A paused mind takes no autonomous wakes and no mutating actions. Reads and
letters still pass.

#### Upgrade guardrails for later stages (document only)

When stages 4 and 5 are built, they should follow these rules:

- **Published releases only.** Upgrade only from published GitHub releases of
  `ianzepp/vivi`, and verify the archive against its published `.sha256`
  before switching.
- **The mind upgrades itself last,** through the service manager. It upgrades
  the pumps first, then restarts itself through the supervisor; it never
  replaces its own binary in place.
- **An external watchdog** rolls back: a separate timer unit checks the new
  version's health and switches back to the previous versioned install
  (`vivi versions`) if the mind does not come up healthy.
- **One upgrade in flight** at a time, hub-wide.
- **A cooldown** between upgrades, and no upgrade while any pump is mid-merge.

### The operating sequence

One wake runs this sequence (`service::tick` and `settle_wake`):

1. **Probe and decide.** Every watched project is probed; `wake::decide`
   picks heartbeat or new signals, or the mind sleeps.
2. **Prompt.** The wake prompt carries the stage, the reasons, the project
   blocks (health verdicts and reasons, fenced), the signal legend, and the
   action menu for the stage. An undelivered report from a previous wake is
   prepended, so nothing the session planned is lost across wakes.
3. **Turn.** The session answers with prose plus an envelope between `+++`
   lines — or none, which ends the wake with no action.
4. **Parse.** The last `+++` block is parsed. A malformed envelope is not a
   crash: its bounded failure (clipped to the action-result allowance,
   markers defused) is retained and reported as `- envelope: unparseable`.
5. **Validate, gate, spend.** Each `[[action]]` passes the field matrix, the
   watched-project rule, the stage and pause gates, and its budget bucket;
   a permitted action is charged before it runs.
6. **Actuate.** Every permitted action becomes exactly one server tool call
   (or a dry-run description when no `[mcp]` is configured). Receipts and
   failures are logged and attached to the plan.
7. **Feed back.** When the actuator feeds back (the MCP actuator does; the
   dry run does not) and the outcome has something to tell — receipts,
   refusals, or a parse failure — the session gets a follow-up turn carrying
   the action report, up to `[mcp] follow_ups` times (default 3, at most 8;
   `0` ends the wake after the first turn). Each follow-up may plan more
   actions, and the loop repeats.
8. **Retain.** Whatever the wake ends with and was never delivered becomes
   the next wake's pending report; a follow-up turn that fails leaves the
   report it was carrying outstanding. A valid empty envelope leaves no
   report.

The report is fenced as untrusted at every boundary: permitted actions render
as `- Kind: permitted`, refusals as `- Kind: refused` with the reason in a
fenced block, receipts in a fenced `action-result` block and failures in
`action-error`, each within the shared 16 000-character allowance; the
health fields above stay at the 2 000-character bound.

### What the actions mean

**`read-status` versus `read-log`.** `read-status` is records-only:
`pump_list` over the pump's records (status lines, health, open steps),
filtered by optional `need`, `status` and `limit`. It never returns log
text. Logs come through `read-log`, which tails `log` (`log.txt`) or
`exceptions` (`exceptions.txt`) with an optional line count (1–500) and
text filter.

**Threads, cursors and bounded evidence.** `read-need-thread` and
`read-task-thread` return the **newest page** of one work item's thread; the
receipt's header carries the page facts first — the item's handle, state and
newest event, how many messages of how many are shown, whether older letters
exist, and the `next_before` cursor. Reading older pages means re-asking
with `before=<cursor>`. A task thread puts actual attempt records first —
letters whose `[pump]` note is `attempt`, with their outcome, failing
command and excerpt — before bounded prose; ordinary replies and job
summaries are never presented as attempts. Every body is cut to an equal
share of the allowance with a visible `[body cut: ...]` or
`[N messages omitted: handles ...]` mark, and the whole receipt is fenced
and defused at the prompt boundary, so a hostile thread cannot close its
fence or forge an envelope.

**Filing, then handle, then priority, then dispatch.** `file-need` and
`file-fix-need` are one observable `send` of `kind=need` carrying `to`,
`subject`, `body`, the `done_when` clauses, `target` and the optional
`write_scope`. The send's answer carries the **returned handle**; filing
names no need of its own. The loop is: file → take the returned handle →
rank it with `set-priority` → send it to the pump with `dispatch-need`. No
priority or dispatch follows the send automatically — a later turn consumes
the handle first. `file-fix-need` additionally fixes this codebase only: the
watched project root must itself be named `vivi` and `target` must be `vivi`;
`file-need` may file to any watched project.

**Unlock entries.** `unlock` restarts a locked need's run. With `entry`
omitted the flow's default first entry applies: `plan` for a build flow,
`fix1` for a repair flow. `entry = "rework1"` continues the need's existing
branch, preserving what its merged build slices produced; any other entry —
`research`, `slice` — starts fresh branch handling, so the evidence should
be inspected first (`read-need-thread`, `read-task-thread`). `note` records
the ruling.

**Replies never release locks.** `reply-need` posts a reply letter on a
need's thread: it supplies evidence or a ruling, but it does not release a
lock — releasing a lock is `unlock`'s job.

**Guarded tuning is an explicit pause/resume bracket.** `set-role-model`,
`set-pool-cap` and `set-pool-band` are project-level tuning and must not
carry a need. When the running pump must not race the change, the mind
brackets the change itself: `pause-pump` before, `resume-pump` after. The
server has its own guard — guarded config writes are refused while the pump
runs, with a backup first and a rollback when the write adds a new config
error — so the bracket is belt, not suspenders. `resume-pump` is for a pump
the mind itself paused.

**Caps are one narrowly quoted key.** `set-pool-cap` is a single `config_set`
on the pool's per-model cap key, `pools."<pool>".caps."<model>"`, with the
pool and the model each a quoted segment of its own so a model such as
`vendor/model.v1` stays one segment. The validated integer cap travels as
TOML text, and zero is a real cap. Nothing else in the config is reachable
through this action.

**Bands are one guarded `pool_bands action=set`.** `set-pool-band` carries
the typed fields (`pool`, `band` P0–P5, `model`, optional `reasoning`) and
replaces that band's whole choice entry on the server — never `config_set`,
never an unset-then-set pair. The server accepts only a model the pool
already offers in its `harness.models` and refuses the set otherwise (the
error names the allowed list). An absent `reasoning` is left out so the
server's own default holds. Only the bands table of the one named pool can
move.

## Untrusted input

Letters, logs, pump health reasons, hand outputs, tool receipts and tool
errors are untrusted, because they can carry prompt injection.

- `prompt::fence` wraps every such string in `<<<untrusted source="..">` ...
  `untrusted>>>`.
- It defuses any fence marker or `+++` line inside the text, so the text
  cannot close its fence or forge an envelope.
- It clips ordinary health fields to 2 000 characters; action results,
  action errors and retained parse failures get the shared 16 000-character
  allowance, so a diagnostic page survives whole.
- Clipping ends with a visible `[cut]` mark and keeps whole characters.

The narrow, validated action surface is the second line of defence, and the
stage gate is the third.

## Repo-Aware Baseline

Upstream `main` at v21.3.2 (`08558899`) has no mind. The pieces it reuses:

- `crate::registry` (`pump-service.toml`, `load_at`, `lock_file`,
  `lock_file_waiting`, now `pub(crate)` for the mind's state lock).
- `role_schedule::evaluate` and `state_label`; `pump::cadence::backoff_secs`.
- `pump::health::health()` (read-only verdict and reasons).
- `PumpStore::open_read_only`, `live_runs`, `steps_of`, `load_snapshot`
  (exceptions and pool jobs), the shipped flows' lock-class nodes, and the
  exception codes `flow-envelope-failed` and `flow-need-locked`.
- `proxy::config::DEFAULT_LISTEN` (`127.0.0.1:18180`).
- The mail server's MCP tools, under the mind's own key: `pump_control`,
  `pump_action`, `pump_list`, `pump_log`, `send`, `reply`, `need_show`,
  `need_thread`, `task_thread`, `flow_show`, `config_show`, `pool_show`,
  `role_set`, `config_set`, `pool_bands`, `pump_priority`, `pump_send`.

The mind adds, under `#[cfg(unix)]`, `src/mind.rs`, `src/mind/*.rs`
with `_test.rs` siblings, `src/cli/mind_command.rs`, the `Command::Mind`
wiring in `src/cli.rs`, `src/main.rs` and `src/local_mailspace_command.rs`,
and `tests/mind_cli.rs`.

| Module | Role |
| --- | --- |
| `config` | `<config_dir>/vivi/mind.toml`, beside `pump-service.toml`: the authority stage enum, heartbeat, proxy URL (loopback only), pi command, model, budgets, the `[mcp]` server table, and paths. `VIVI_MIND_HOME` overrides the home. |
| `authority` | The action surface (27 kinds), each kind's stage and budget bucket, and `permit` / `permit_project`: a pure gate over stage, pause and the two budget buckets. |
| `wake` | `decide()` and `finish()`: heartbeat, backoff, in-flight block, rolling-hour cap, signal de-duplication. |
| `probe` | `Probe` trait. `HealthProbe` = `pump::health` verdict plus the read-only `pump.db` reads and the `log.txt` tail described in Wakes. |
| `projects` | Watched roots: the registry's pumped entries, less the mind's ignore list. |
| `store` | `<mind home>/state.json`, updated under a sidecar lock: session id, wake records, budget ledger, pending report, pause flag. The mind's own state, **never** the pump's `state.extra`. |
| `actions` | The TOML action envelope, `plan()` (validate, gate, spend), the retained `EnvelopeOutcome`, `argv()` (diagnostic), and the `Actuator` trait: `DryRun` describes, the MCP actuator executes. |
| `mcp_actuator` | One permitted action → exactly one server tool call; action-aware, bounded rendering of receipts (structured for diagnostic reads, plain bounded text otherwise) and failures. |
| `prompt` | Wake prompts with untrusted fencing, the follow-up action report, and the per-stage action menu mirrored against the validator. |
| `pi` | The `PiSession` trait, the assumed JSONL codec, `ProcessPi` (subprocess, every wait bounded) and `FakePi`. |
| `socket` | The attach protocol, `bind()`, `serve()` (request lines capped while read), peer-uid check, `Client`. |
| `service` | `tick()`, one wake end to end — envelope, gate, actuation, follow-up loop, pending-report retention — testable with fakes; `serve_forever()`, the real loop. |
| `commands` | CLI dispatch, the attach REPL, and `probe`. |

Real and unit-tested: config (including the shipped example), the gate and
both budget buckets, the wake scheduler, registry reading, the state store,
envelope parsing and per-kind validation, the filing bounds and fix policy,
the action→tool-call mapping and receipt rendering, the follow-up loop and
pending-report retention, prompt fencing and the menu, the pi codec and
`ProcessPi` against a scripted `sh` child, the socket server and client, one
full `tick()` with fakes, the store reads of the probe, and the CLI surface
(temp `VIVI_CONFIG_DIR` and `VIVI_MIND_HOME`).

Exercised live on the e2e sandbox (pi 0.73.1, DeepSeek Flash through the
proxy, during the spike and its from-scratch rebuild): `ProcessPi` (the RPC
records were corrected in `2d47a516`), `serve_forever`, autonomous wakes,
attach, and session resume across mind restarts. Not yet exercised live:
`respawn_if_exited` (a pi crash under a running mind). The MCP execution
path is pinned by unit tests against a scripted tool caller; it has not been
exercised live against the hosted server.

Stubbed: `vivi mind start` and `service install|uninstall` (explanatory
errors), held steps in the probe (no reader yet: always zero), ageing a
running step that has no job, and stages 4 and 5.

## Delivery record (follow-up units)

The spike's follow-up graph, as it landed. Sanity gates were the named
`cargo test --lib` filters plus `cargo fmt --check` and
`cargo test --test hygiene`; live claims are only the sandbox ones above.

```text
unit: U1-pi-protocol            status: delivered
  prompt, history and session_id verified live (2d47a516); compact and
  history round-trip over the socket and were checked live against pi 0.73.1
  (see Commands); the scripted tests mirror the observed records

unit: U2-pi-wiring              status: delivered
  pi reaches the proxy through <mind home>/.pi/agent/models.json and runs
  with --no-tools. Still open: refusing pi extension UI requests

unit: U3-actuator               status: delivered (as the MCP actuator)
  G0 was decided for the server route: one permitted action becomes exactly
  one MCP tool call under the mind's own key, with bounded receipts and
  failures; refusals never call. DryRun stays the deliberate no-[mcp]
  fallback; argv() survives as a diagnostic render

unit: U4-letters                status: delivered
  letters and escalations go through the server's send tool as kind=mail to
  the operator of the named project; the grant owns authorship

unit: U5-held-and-orphan-steps  status: partially delivered
  idle-step, stale-pending and deferral signals are in; steps held for an
  operator answer still have no reader (always zero), and a running step
  with no job at all cannot be aged because pump.db keeps no step timestamps

unit: U6-watchdog               status: open
  no liveness signal yet; Restart=on-failure covers crashes only

unit: U7-service-install        status: open
  service install|uninstall remain explanatory stubs; `service unit` prints
  the hardened example

unit: U8-attach-policy          status: open (spike behaviour stands)
  an attached prompt queues behind the turn lock; no steer/suppress decision

unit: U9-file-fixes             status: delivered
  file-need / file-fix-need are one send kind=need each, counted against
  the project budget, with the vivi-only fix policy; dispatch-need is one
  pump_send; the loop is file -> returned handle -> set-priority -> dispatch

unit: U10-docs-release          status: docs delivered; release open
  this document describes shipped behaviour; the release note waits for
  release-prep, which is the operator's call
```

## Checkpoints And Gates

- **G0, ownership: resolved.** Every action goes through the mail server's
  `/v1/mcp` route under the mind's own key; the mind writes nothing into a
  project directly. The probe still needs **read** access to each pump home
  and to `/proc/locks` for health.
- **G1, unit sanity.** Each unit's `sanity` command, `cargo fmt --check`, and
  `cargo test --test hygiene`.
- **G2, live observe run.** Observed on the e2e sandbox during the spike
  (2026-10-25, dry-run escalation, session resume across restarts and SSH
  drops). Not repeated since; no production evidence is claimed.
- **G3, unstick live.** Not claimed: a permitted unstick executed through
  the real server has not been demonstrated live; the path is unit-tested.
- **Release gate.** Full `cargo test`, `cargo clippy --all-targets` with no
  new diagnostics in mind files, docs updated, then release-prep — still the
  operator's call, not yet taken.

## Validation

Spike validation (2026-10-25, debug build, box) — historical:

- `cargo fmt --check`: clean.
- `cargo test --lib mind::`: all mind unit tests pass.
- `cargo test --test mind_cli`: 6 passed (temp dirs only). `cargo test --test hygiene`: 13
  passed.
- `cargo clippy --all-targets -- --cap-lints warn`, filtered to mind files
  and `src/registry.rs`: zero diagnostics. Unfiltered clippy fails on `main` itself (about 446
  pre-existing pedantic diagnostics), so the filter is the honest gate.
- Full suite: every integration target passes. The lib target has failures
  only in `pool::bootscript` and `pool::job` runner tests, which also fail on
  a baseline `origin/main` worktree on the same box (a similar but not
  identical set, so environment-dependent and flaky).
- `vivi mind probe` against the e2e sandbox: flags need2 while health says
  `ok` (see Wakes).
- From scratch (2026-10-25): the sandbox was deleted and rebuilt from a fresh
  clone using only the repository's docs, which produced
  `vivi-mind-quickstart.md` and the example `mind.toml`. The rebuilt mind
  watched both projects, flagged only the stuck one, woke on its own, logged
  a dry-run escalation, and kept one session across attaches and a restart.

The delivered action contract above is pinned by the mind unit tests
(actions, authority, mcp_actuator, service, prompt, config); the sandbox
paragraphs are the only live evidence, and they are labeled as spike-era.

## Companion Skill Plan

- **factory**: ran the units above as plan, implement, verify, review and
  commit phases, with a deferred-findings ledger; the release decision
  stayed with the operator.
- **polish**: a per-file pass over every touched mind file before each PR,
  `polish(scope):` commits with `Polish-Primary` / `Polish-Supporting`
  trailers, and `.polish-inspected.json` for clean files.
- **delivery**: this document stays the contract; amend it when a decision
  below lands instead of re-deriving the architecture.

## Open Questions

1. **File ownership of project `.vivi` folders — resolved in outline.**
   Writes route through the server, so the mind needs no project write
   access anywhere; its key file lives in the mind home. What remains open
   is read access: one mind reads every registered project's pump home and
   `/proc/locks` for health, and whether every project accepts that (or
   wants a registry-level `mind = false` opt-out instead of the mind-local
   ignore list) is undecided. Related: should a project's letters ever be
   quoted into a wake about another project, given that one session spans
   them all?
2. **Attach during an autonomous turn: lock or queue?** The delivered
   behaviour queues: the session sits behind one turn lock, and an attached
   prompt waits, with a "waiting" notice, until the wake ends. A wake that
   comes due during an attached conversation also waits.

   The alternative is pi's `streamingBehavior: "steer"`, which injects the
   operator's message into the turn in flight. That is better for "stop, don't
   do that", but riskier: the operator's text and untrusted wake context mix
   in one turn.

   A related question: should an attached operator suppress autonomous wakes
   entirely until they detach?
3. **Pi RPC protocol details.** Verified live against pi 0.73.1
   (`2d47a516`, and the from-scratch sandbox rebuild):
   - `--session <id>` takes the id and exits ("No session found") when no
     file `<stamp>_<id>.jsonl` exists, so the mind passes a recorded id only
     when its file is there;
   - `get_state` reports `sessionId` and `sessionFile`;
   - message `content` is a string or an array of `{type,text}` blocks;
   - a run ends with `agent_end`, also after a provider error; pi never
     emits `agent_settled`;
   - pi reaches the proxy through a custom provider in
     `<mind home>/.pi/agent/models.json` with a placeholder key;
   - `--no-tools` turns off pi's own tools in RPC mode, and the mind passes it.

   Still open: extension UI requests, which the mind should refuse.
4. **A watchdog for the mind itself.**
   - `Restart=on-failure` covers crashes. A wedged mind (a hung pi turn past
     `TURN_TIMEOUT`, or a stuck socket) needs a liveness signal: systemd
     `WatchdogSec` with `sd_notify`, or a heartbeat file checked by a separate
     timer unit.
   - Who tells the operator that the mind is down: the pump, vivi-server, or
     an external check?
5. **Smaller questions.**
   - Should `resume` be limited to pumps the mind itself paused? The menu
     says so, but no marker exists in the pump; enforcement is the mind's
     own discipline.
   - Should the per-need budget also cap distinct needs per wake?
   - Should letters to the operator have a hub-level channel, instead of
     landing in each watched project's mailspace addressed to its operator?

## Review asks

- The action contract (matrix, bounds, budgets, operator grant, one call per
  action) is the thing to review: it is now the shipped surface.
- The stage table: is `pause` correctly in `unstick` rather than `observe`?
- Is the de-duplication by fingerprint enough, or should a pump that stays
  stalled get a faster re-wake than the heartbeat?
- U6 (watchdog) and U7 (service install) are the open delivery units; is
  that the right order for what is left?
