# Delivery: `vivi mind`, a persistent hub-wide supervisor

Status: **delivered on this branch**. This document was written 2026-10-25 as
the spec for an exploratory spike; the spike has since landed as the
delivered supervisor: the staged action contract, the bounded envelope
pipeline, and the MCP actuator are real and unit-tested. This document now
describes the delivered implementation. Everything labeled with a date is a
record of that day's run, not a claim about deployment: **no live deployment
is claimed**, and MCP execution of the delivered actuator is claimed only as
unit-tested. G2 and G3 (below) have not been run against the delivered
actuator.

To stand it up on a box, follow
[`vivi-mind-quickstart.md`](vivi-mind-quickstart.md).

Release decision: still **defer-release**. This documentation pass is not a
release: no version bump, no tag, no release notes yet.

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

Done means: a hardened service on the hub that resumes one pi session across
restarts and SSH drops, wakes on a heartbeat and on real stuck signals, and
acts only through the server's MCP tools within an operator-chosen authority
stage and budget.

## Normalized Spec

### Commands

The service verbs mirror `vivi pump service`. Statuses describe this branch.

| Command | What it does | Status |
| --- | --- | --- |
| `vivi mind` / `vivi mind attach` | Attach a line REPL to the running mind's one session. Lines are prompts. `/status`, `/pause`, `/resume`, `/compact`, `/history` and `/quit` are verbs. | Delivered (socket client), over fakes in tests; run live on the 2026-10-25 sandbox against pi 0.73.1. |
| `vivi mind status [--json]` | Authority, paused, busy, session id, and last wake. Falls back to the state file when no mind is listening. | Delivered |
| `vivi mind log [-n N]` | Tail of `<mind home>/mind.log`. | Delivered |
| `vivi mind pause` / `resume` | Stop or restart autonomous wakes and mutating actions. Attach keeps working. Goes through the socket, or through the state file when the mind is down. | Delivered |
| `vivi mind stop` | Ask the running mind to exit. The service manager decides whether to restart it. | Delivered |
| `vivi mind start` | Starting is the service manager's job. | Stub (explanatory error) |
| `vivi mind compact` / `history [--limit N]` | Forwarded to the pi session. | Delivered over the socket; checked live against pi 0.73.1 on 2026-10-25. |
| `vivi mind service add\|remove <root>` | Watch or ignore a project that is already registered. This edits the mind's own ignore list and never touches the host registry. | Delivered |
| `vivi mind service list` | Registered projects and whether the mind watches each. | Delivered |
| `vivi mind service status` | Whether a mind is listening, plus every path it uses. | Delivered |
| `vivi mind service run` | The foreground service, for a service manager. | Delivered loop; run live on the 2026-10-25 sandbox against pi 0.73.1, before the MCP actuator landed. |
| `vivi mind service unit` | Print an example hardened systemd unit. Installs nothing. | Delivered (template) |
| `vivi mind service install\|uninstall` | OS service install. | Stub (explanatory error) |

`vivi mind probe [--json]` is a read-only diagnostic: one probe pass plus the
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
                                      │    (MCP actuator; with no [mcp]      │    per permitted action
                                      │     a DryRun only describes)         │    to the hub server
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

The mind runs as the same `vivi` user the hub server runs as. It takes no
browser or web inputs, and its own network is limited to loopback: the proxy
for models, the hub server for actions, and the attach socket for operators.

`vivi mind service unit` prints `src/mind/vivi-mind.service.example`, an
example only. Its main directives:

- `User=vivi` and `UMask=0077`
- `ProtectSystem=strict` and `ProtectHome=read-only`, with `ReadWritePaths`
  limited to the mind home (the mind writes no project files)
- `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`, with
  `IPAddressDeny=any` and `IPAddressAllow=localhost`
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
  - steps held for an operator answer (no reader yet: always zero — see
    Preserved limitations).

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

A second live stall, same day: a step sat `running` for about four minutes
with zero dispatches, because every dispatch was refused with a pool config
error (`harness.models holds an empty slug ...`), and `pump health` still said
`ok`. That is what the idle-step and dispatch-failure signals catch. When
the probe was re-run (16:38 EDT) the pool had a real model, so it reported
`idle=0 deferring=0`. Unit tests cover the signals, using fixtures shaped
like the pump's own log lines. A running step with **no job at all** cannot
be aged, because `pump.db` keeps no step timestamps (see Preserved
limitations).

### The action contract

The operator sets the stage explicitly in `mind.toml` (`authority = ...`) and
opens each one deliberately. Each stage includes the powers of the stages
below it.

| Stage | Opens | Status |
| --- | --- | --- |
| 1 `observe` (default) | The eight reads (below), `letter-to-operator`, `escalate`. | Delivered. With `[mcp]`, letters are `send` calls; without it they are only described. |
| 2 `unstick` | `pause-pump`, `resume-pump`, `requeue`, `replan`, `answer`, `unlock`, `reply-need`, `set-priority`, `withdraw-need`, and the tuning moves `set-role-model`, `set-pool-cap`, `set-pool-band`. | Delivered: gate, budgets, field matrix and tool mapping unit-tested; execution needs `[mcp]`. |
| 3 `file-fixes` | `file-need`, `file-fix-need`, `dispatch-need`. | Delivered: filing is one `send kind=need`; dispatch is one `pump_send`. |
| 4 `upgrade-on-approval` | `upgrade`: take a release the operator cut. | Not implemented. The config loader refuses the stage; the tool mapping refuses the kind. |
| 5 `self-release` | `self-release`: cut releases itself. | Not implemented. Refused the same way. |

The rules every action follows:

- **Only server tools.** The mind runs no CLI command and never edits
  `.vivi`, git, or config directly. Each permitted action is exactly one MCP
  tool call (the table below); the server decides what the key may do, and a
  refusal comes back as the tool's error text, which the session sees on its
  next turn. `ActionRequest::argv()` — the spike dry-run's command-line
  mapping — has no production caller anymore; it is kept as a diagnostic.
- **The model runs nothing itself.** A turn ends with a TOML envelope
  between `+++` lines (a `+++ TOML` tag is accepted), and the mind decides
  what happens to each `[[action]]`:
  - `deny_unknown_fields`. A blank field is an absent one.
  - Handles and answer words must be short plain tokens that do not start
    with `-`, so flag and shell injection is refused.
  - `project` must be a watched root — every action, reads and letters
    included. The server sees the root's directory name, the way a hosted
    project is named.
  - Notes are capped at 500 characters, and an envelope at 8 actions.
  - Budget is spent as actions pass the gate, so two requeues of one need —
    or two filings to one project — in one envelope count twice.
  - The gate's verdicts are logged and shown to the session on its next
    turn (Operating sequence).
- **The operator grant.** The mind presents the bearer key from
  `[mcp] key_file` on every call; the server resolves the key's grant per
  call and refuses what it does not cover, whatever the gate already
  allowed. Nothing the mind sends or files carries a `from`: the
  authenticated grant owns authorship.
- **Pause is the brake.** A paused mind takes no autonomous wakes and no
  mutating actions. Reads and letters still pass.

#### Field and content bounds

Every field is length- or value-capped before it nears the gate. Oversized
content is refused by name; malformed content (a negative count, an escaping
path) is refused with the field named so the turn can fix it.

| Field | Rule |
| --- | --- |
| envelope | at most 8 `[[action]]` tables; further entries are refused |
| `note` | at most 500 characters |
| `need`, `step`, `word`, `entry`, `role`, `to`, `pool`, `target`, `before` | plain handle: 1–64 characters, `[A-Za-z0-9_-]`, no leading `-` |
| `model` | pool model slug (ASCII alphanumeric plus `. _ : / @ + -`) |
| `flow` | `build` \| `research` \| `release` \| `repair` |
| `file` (`read-log`) | `log` \| `exceptions` |
| `status` (`read-status`) | `open` \| `done` \| `all` |
| `band` | `P0`–`P5` |
| `reasoning` | 1–32 characters, ASCII alphanumeric or `. _ -` |
| `limit` (reads and threads) | 1–100 |
| `lines` (`read-log`) | 1–500 |
| `priority` | 0 or more; 0 is a real level (none) |
| `cap` | 0 through 4294967295 |
| `subject` (filing) | at most 200 characters |
| `body` (filing) | at most 16000 characters, and must not open with `+++` |
| `done_when` (filing) | at most 8 clauses, each at most 500 characters, none blank |
| `write_scope` (filing) | at most 32 paths, each at most 256 characters, relative, no `..` climb |

#### The stage/action/field matrix

This is the matrix `authority` and `actions::fields` enforce; the wake's
action menu (`prompt::menu`) mirrors it, and a unit test compares the two so
they cannot drift. Required fields must be present; a present field the kind
never reads is refused; a blank required field is simply missing.

| Kind | Required | Optional | Stage |
| --- | --- | --- | --- |
| `read-status` | — | `need`, `status`, `limit` | observe |
| `read-log` | `file` | `lines`, `filter` | observe |
| `read-need` | `need` | — | observe |
| `read-need-thread` | `need` | `limit`, `before` | observe |
| `read-task-thread` | `step` | `limit`, `before` | observe |
| `read-flow` | `flow` | `need` | observe |
| `read-config` | — | — | observe |
| `read-pool` | `pool` | — | observe |
| `letter-to-operator` | — | `need`, `note` | observe |
| `escalate` | — | `need`, `note` | observe |
| `pause-pump` / `resume-pump` | — | — | unstick |
| `requeue` | `need`, `step` | `note` | unstick |
| `replan` | `need` | — | unstick |
| `answer` | `need`, `step`, `word` | `note` | unstick |
| `unlock` | `need` | `entry`, `note` | unstick |
| `reply-need` / `withdraw-need` | `need`, `note` | — | unstick |
| `set-priority` | `need`, `priority` | — | unstick |
| `set-role-model` | `role`, `model` | — | unstick |
| `set-pool-cap` | `pool`, `model`, `cap` | — | unstick |
| `set-pool-band` | `pool`, `band`, `model`, `reasoning` | — | unstick |
| `file-need` / `file-fix-need` | `to`, `subject`, `body`, `done_when`, `target` | `write_scope` | file-fixes |
| `dispatch-need` | `need` | `note` | file-fixes |
| `upgrade` / `self-release` | — | — | refused stages |

`file-fix-need` carries the vivi-only fix policy on top: the watched project
root must itself be named `vivi` and `target` must say so. `file-need` may
file to any watched project.

#### One call per action

Each permitted action becomes exactly one server tool call; unset arguments
are left out so the server's own defaults hold.

| Action | Tool | Arguments |
| --- | --- | --- |
| `read-status` | `pump_list` | `project`, `need?`, `status?`, `limit?` |
| `read-log` | `pump_log` | `project`, `file`, `lines?`, `filter?` |
| `read-need` | `need_show` | `project`, `handle` |
| `read-need-thread` | `need_thread` | `project`, `handle`, `limit?`, `before?` |
| `read-task-thread` | `task_thread` | `project`, `handle` (the step), `limit?`, `before?` |
| `read-flow` | `flow_show` | `project`, `kind`, `need?` |
| `read-config` | `config_show` | `project` (`resolved` is never sent) |
| `read-pool` | `pool_show` | `project`, `name` |
| `letter-to-operator` / `escalate` | `send` | `kind=mail`, `to=operator`, subject `mind: <kind> (need <handle>)` when a need is linked, body = `note` |
| `reply-need` | `reply` | `project`, `handle`, `body` = `note` |
| `pause-pump` / `resume-pump` | `pump_control` | `project`, `action=pause\|resume` |
| `requeue` | `pump_action` | `action=requeue`, `steps=[step]`, `reason` = `note?` |
| `replan` | `pump_action` | `action=replan`, `need` |
| `answer` | `pump_action` | `action=answer`, `step`, `word`, `note?` |
| `unlock` | `pump_action` | `action=unlock`, `need`, `entry?`, `note?` |
| `withdraw-need` | `pump_action` | `action=withdraw`, `needs=[need]`, `reason` = `note?` |
| `set-priority` | `pump_priority` | `action=set`, `need`, `level` (0 travels; absent stays out) |
| `set-role-model` | `role_set` | `project`, `role`, `model` — nothing else, so no other field can move with it |
| `set-pool-cap` | `config_set` | the single key `pools."<pool>".caps."<model>"` (see Operating sequence) |
| `set-pool-band` | `pool_bands` | `action=set`, `pool`, `band`, `model`, `reasoning?` |
| `file-need` / `file-fix-need` | `send` | `kind=need`, `to`, `subject`, `body`, `done_when`, `target`, `write_scope?` |
| `dispatch-need` | `pump_send` | `need`, `body` = `note?` |
| `upgrade` / `self-release` | — | refused: not wired to the server in this build |

#### Budgets: two buckets, one window

`[budgets] per_need_actions` within `per_need_window` (defaults 3 within
24 h) bounds both buckets:

- **Per-need.** The need-scoped moves — `requeue`, `replan`, `answer`,
  `unlock`, `reply-need`, `set-priority`, `withdraw-need`, `dispatch-need`
  — spend the bare need handle's bucket.
- **Per-project.** Filing (`file-need`, `file-fix-need`) and the role/pool
  tuning moves (`set-role-model`, `set-pool-cap`, `set-pool-band`) spend one
  shared window per watched project, on the reserved ledger key
  `project:<root>`. The `project:` prefix carries `:` and path separators,
  which cannot pass the handle validator, so a project entry can never
  collide with a bare need handle.
- **Unbudgeted.** The eight reads, `letter-to-operator`, `escalate`,
  `pause-pump` and `resume-pump` draw down nothing.

Spending is charged as a permitted action passes the gate, before
actuation. **A permitted action the server later refuses is not refunded**:
the budget records that the mind tried, and the refusal is shown to the
session so it can choose differently next turn.

### Operating sequence

One wake runs probe → prompt → turn → envelope parse → gate → actuate →
feedback, and retains whatever the session never got to see.

1. **Probe.** The 60 s tick probes every watched root (`HealthProbe`:
   `pump health` plus the read-only `pump.db` reads and the `log.txt` tail
   described under Wakes) and feeds the signals to `wake::decide`.
2. **Prompt.** The wake prompt carries the stage, the wake reasons, a legend
   for the signals (a live model once read a locked need as "normal queue
   state" without it), one fenced block per project, and the action menu for
   the stage. Any report left pending by the previous wake is prepended, so
   the session sees what happened to its last envelope even if no follow-up
   turn ran.
3. **Turn.** The mind takes the one turn lock and sends the prompt. An
   attached operator's line waits behind it; a wake waits behind an attached
   conversation.
4. **Envelope parse.** The reply's **last** `+++` fence is the envelope; no
   envelope at all is a valid empty decision (the wake ends, nothing is
   pending). A TOML error is a parse failure, kept bounded (below) and
   reported instead of plans.
5. **Gate.** Each request passes, in order: the per-kind field matrix; the
   watched-project rule; the file-fix policy for `file-fix-need`; the
   authority gate (stage, pause, the need bucket); the project bucket for
   filing and tuning. The first refusal wins, and every verdict is logged.
6. **Actuate.** Each permitted action runs as its single tool call. The
   receipt or failure is rendered bounded (below) and logged.
7. **Feedback.** With `[mcp]`, the actuator feeds back: while the last
   outcome has something to tell — receipts, failures, refusals, or a parse
   failure — and `follow_ups` rounds remain (default 3, at most 8; `0` ends
   the wake after the first turn), the next turn receives the action
   report: one line per action (permitted, or refused with the reason
   fenced), then each receipt or failure fenced. A follow-up turn may name
   more actions; every round passes the same gate and the same budgets.
   Without `[mcp]` nothing feeds back and the wake ends after one turn.
8. **Retention.** At wake end, the last outcome that a turn never
   successfully replaced becomes `pending_report` in the mind's state, and
   the next wake's prompt carries it (step 2). A follow-up turn that fails
   leaves the report it was carrying outstanding rather than dropping it.
   A valid empty envelope leaves nothing pending.

The specific behaviors the contract fixes:

- **`read-status` is records-only.** `pump_list` returns the pump's records:
  status lines, health, open steps, filtered by `status` (`open|done|all`)
  and `limit` (1–100). Log text — including dispatch deferrals and tick
  failures — is `read-log`'s job (`pump_log`, `file` = `log|exceptions`,
  `lines` 1–500, text `filter`).
- **Threads are newest-page reads with cursors.** `need_thread` and
  `task_thread` return the newest page of one work item's thread. The
  rendered receipt puts the facts first — the handle, the item's state and
  newest event, how many of how many messages are shown, and, when older
  letters exist, `next_before` with the instruction to read on with
  `before=<cursor>`. On a task thread, real attempt records (letters whose
  `[pump]` table marks them `attempt`; ordinary replies and job summaries
  never count) come first, each with its outcome, failing command and
  excerpt ahead of bounded prose. Messages that did not fit are listed by
  handle instead of being silently dropped. The whole page is bounded by the
  shared allowance and fenced as untrusted at the prompt boundary.
- **Malformed envelopes and refusals feed back without losing anything.** An
  unparseable envelope is reported as `envelope: unparseable` with the
  bounded failure fenced; each refused action renders its reason in a fenced
  `gate-refusal` block. All of it travels on the follow-up turn, or, when
  the wake ends first, as the retained pending report.
- **Filing is a three-step loop.** `file-need`/`file-fix-need` is one
  observable `send kind=need` and nothing else — no priority, no dispatch.
  The send answers a returned handle; a later turn ranks that handle with
  `set-priority` (0 is a real level) and hands it to the pump with
  `dispatch-need` (one `pump_send`). The dispatch record is addressed to the
  pump's own identity, so the need itself need not be addressed to the pump.
  Filing itself names no need — the returned handle is the need.
- **Unlock entries.** An omitted `entry` restarts the locked run at the
  flow's default first entry: `plan` for a build flow, `fix1` for a repair
  flow. `entry="rework1"` continues the need's existing branch — the choice
  for a need whose merged build slices were useful. Any other entry
  (`research`, `slice`) starts fresh branch handling, so the menu tells the
  turn to inspect the evidence first with `read-need-thread` and
  `read-task-thread`. `note` records the ruling.
- **Replies never release locks.** `reply-need` posts a letter on a need's
  thread: it supplies evidence or a ruling, but releasing a lock is
  `unlock`'s job.
- **Guarded tuning is an explicit bracket.** When the running pump must not
  race a change, the menu instructs `pause-pump` before `set-role-model`,
  `set-pool-cap` or `set-pool-band`, and `resume-pump` after. The mind does
  not enforce the bracket; the pause gate and budgets still apply to every
  move.
- **Caps are one narrowly quoted key.** `set-pool-cap` is one `config_set`
  on the single key `pools."<pool>".caps."<model>"` — pool and model each a
  quoted segment of its own, so a model such as `vendor/model.v1` stays one
  segment. The already-validated integer travels as TOML text; zero is a
  real cap. No other configuration key can move with it.
- **Bands replace one choice entry.** `set-pool-band` is one guarded
  `pool_bands action=set` carrying the typed fields, which replaces that
  band's whole choice entry within the pool's `harness.models`. Never
  `config_set`, never an unset-then-set pair; an absent `reasoning` stays
  out so the server's own default holds.

#### Receipts, failures, and untrusted evidence

Everything that came from a pump, a letter, a log, a hand, or a tool answer
is untrusted, because it can carry prompt injection:

- `prompt::fence` wraps such text in `<<<untrusted source="..">` ...
  `untrusted>>>`; `fence_within(source, text, max)` bounds it first. Clipping
  ends with a visible `[cut]` mark, keeps whole characters, and defuses any
  fence marker or `+++` line inside the text (a zero-width space breaks the
  marker), so the text cannot close its fence or forge an envelope.
- Health fields keep 2000 characters. Action results, action errors, and
  retained parse failures share one larger allowance (16000 characters) so a
  diagnostic page survives whole.
- Diagnostic reads are rendered, not dumped: the structured render keeps the
  identity and page facts ahead of long bodies and marks every omission
  visibly (see the thread behavior above). Ordinary receipts and failures —
  including a tool error or a server refusal — stay bounded at 1500
  characters under their tool prefix, so a huge refusal cannot flood a turn
  either.

Rendering is data preparation, not trust: the prompt boundary fences and
defuses whatever a receipt carries.

#### Upgrade guardrails for stages 4 and 5 (document only)

The stages exist so the config shape is settled, but opening them is refused.
When they are built, they should follow these rules:

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

### Preserved limitations

Real limits of unrelated work, still true in this build:

- **Probe.** Steps held for an operator answer have no reader and stay zero.
  A running step with no job at all cannot be aged: `pump.db` keeps no step
  timestamps. Both need pump-side schema or reader work.
- **Service installation.** `vivi mind start` and
  `vivi mind service install|uninstall` are stubs with explanatory errors;
  `service unit` prints the example unit and installs nothing.
- **Pi.** Extension UI requests are not refused yet (the spike-era pi
  protocol notes below record what is verified; the refusal is still open).
- **Stages 4 and 5.** `upgrade` and `self-release` are refused by the config
  loader and by the tool mapping.

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
- `crate::mcp::client` (`Client`, `HttpUpstream`), the MCP client the
  actuator calls the hub server with.
- `mailspace::pump_note`, for recognizing attempt records in thread renders.

The branch adds, under `#[cfg(unix)]`, `src/mind.rs`, `src/mind/*.rs` with
`_test.rs` siblings, `src/cli/mind_command.rs`, the `Command::Mind` wiring in
`src/cli.rs`, `src/main.rs` and `src/local_mailspace_command.rs`, and
`tests/mind_cli.rs`.

| Module | Role |
| --- | --- |
| `config` | `<config_dir>/vivi/mind.toml`, beside `pump-service.toml`: the authority stage enum, heartbeat, proxy URL (loopback only), pi command, model, budgets, `[mcp]` (server, key file, follow-ups), and paths. `VIVI_MIND_HOME` overrides the home. The shipped `mind.toml.example` parses to the defaults, by test. |
| `authority` | The action surface: stages, the field matrix's companion (`need_scoped`, `budget`, `mutates`), and `permit`/`permit_project` — pure gates over stage, pause and the two budget buckets, unit-testable without a server. |
| `wake` | `decide()` and `finish()`: heartbeat, backoff, in-flight block, rolling-hour cap, signal de-duplication. |
| `probe` | `Probe` trait. `HealthProbe` = `pump::health` verdict plus the read-only `pump.db` reads and the `log.txt` tail described in Wakes. |
| `projects` | Watched roots: the registry's pumped entries, less the mind's ignore list. |
| `store` | `<mind home>/state.json`, updated under a sidecar lock: session id, pause, ignore list, budget ledger, last wake, pending report. The mind's own state, **never** the pump's `state.extra`. |
| `actions` | The TOML action envelope, `parse_envelope`, `plan()` (validate, gate, spend), `EnvelopeOutcome` with its bounded parse-failure path, the field matrix (`actions::fields`), the filing bounds and fix policy (`actions::filing_fields`), `DryRun`, and the diagnostic `argv()`. |
| `mcp_actuator` | The real actuator: one MCP tool call per permitted action (`calls`), bounded action-aware receipt rendering (`render`), the shared `ACTION_RESULT_ALLOWANCE`, and `HttpCaller` (bearer key from `key_file`). |
| `prompt` | Wake and attach prompts with untrusted fencing, the follow-up `plan_report`, and the stage action menu (`prompt::menu`), which mirrors the field matrix by test. |
| `pi` | The `PiSession` trait, the JSONL codec, `ProcessPi` (subprocess, every wait bounded) and `FakePi`. |
| `socket` | The attach protocol, `bind()`, `serve()` (request lines capped while read), peer-uid check, `Client`. |
| `service` | `tick()`: probe → decide → prompt (with any pending report) → turn → gate → actuate → follow-ups → retention; `serve_forever()`, the real loop; interrupted-wake recovery; actuator selection (`[mcp]` or `DryRun`). |
| `commands` | CLI dispatch, the attach REPL, and `probe`. |

Real and unit-tested: config (including the shipped example), the gate and
both budget buckets, the wake scheduler, registry reading, the state store,
envelope parsing and the full field matrix, the filing bounds and fix
policy, prompt fencing and the stage menu, the MCP actuator's call mapping
and bounded rendering (over a fake caller), the pi codec and `ProcessPi`
against a scripted `sh` child, the socket server and client, full `tick()`
runs with fakes (including follow-ups and pending-report retention), the
store reads of the probe, and the CLI surface (temp `VIVI_CONFIG_DIR` and
`VIVI_MIND_HOME`).

Exercised live on the 2026-10-25 e2e sandbox (pi 0.73.1, DeepSeek Flash
through the proxy), all before the MCP actuator landed: `ProcessPi`,
`serve_forever`, autonomous wakes, attach, and session resume across mind
restarts. Not yet exercised live: `respawn_if_exited` (a pi crash under a
running mind), and the delivered actuator against a real server.

Stubbed: `vivi mind start`, `service install|uninstall`, the held-steps
probe reader, ageing a job-less running step, and stages 4 and 5.

## Delivered Work

The spike's unit plan, with truthful per-unit statuses:

```text
unit: U1-pi-protocol
status: delivered. Live-checked against pi 0.73.1 on 2026-10-25 (prompt,
history, session id; compact checked over the socket); the codec matches the
observed records.
```

```text
unit: U2-pi-wiring
status: delivered, except the extension-UI refusal, which is still open.
Models reachable only through the loopback proxy; pi's own tools off.
```

```text
unit: U3-actuator
status: delivered, reshaped. The decision this branch settled (former G0):
every action goes through the hub server's MCP tools, so the mind needs no
filesystem write into any project. `McpActuator` turns one permitted action
into exactly one tool call; `DryRun` stays as the deliberate no-[mcp]
fallback, describing what would run. The old argv mapping has no production
caller.
```

```text
unit: U4-letters
status: delivered. letter-to-operator and escalate are send calls to the
operator; the subject names the need when one is linked. No from travels.
```

```text
unit: U5-held-and-orphan-steps
status: open. Held steps stay zero; a running step with no job cannot be
aged (pump.db keeps no step timestamps). Needs pump-side work.
```

```text
unit: U6-watchdog
status: open. No liveness signal yet; Restart=on-failure covers crashes only.
```

```text
unit: U7-service-install
status: open. start and install/uninstall remain stubs; the example unit
prints.
```

```text
unit: U8-attach-policy
status: the queue decision is delivered: one turn lock, an attached prompt
waits with a notice, a due wake waits behind a conversation. The steer and
suppress alternatives remain unexplored.
```

```text
unit: U9-file-fixes
status: delivered. file-need and file-fix-need are bounded `send kind=need`
calls; file-fix-need carries the vivi-only fix policy; the returned handle
is ranked with set-priority and dispatched with dispatch-need (pump_send).
```

```text
unit: U10-docs-and-release
status: this document describes the delivered behavior; the release note is
not written and no version work has been done.
```

## Checkpoints And Gates

- **G0, ownership (was: blocks every write stage).** Settled by U3's route:
  every action is a server tool call under the key's grant, so the mind
  needs no write access to any project. The systemd example correspondingly
  limits `ReadWritePaths` to the mind home.
- **G1, unit sanity.** Each unit's sanity command, `cargo fmt --check`, and
  `cargo test --test hygiene`. Ongoing practice.
- **G2, live observe run.** Not run against the delivered actuator. The
  2026-10-25 sandbox run exercised the dry-run loop only (see Validation).
- **G3, unstick live.** Not run.
- **Release gate.** Not reached: docs updated here, no release note, no
  version work.

## Validation

Spike-era validation (2026-10-25, debug build, box), recorded as history of
that day's branch state — the actuator had not landed:

- `cargo fmt --check`: clean.
- `cargo test --lib mind::`: all mind unit tests pass.
- `cargo test --test mind_cli`: 6 passed (temp dirs only). `cargo test
  --test hygiene`: 13 passed.
- `cargo clippy --all-targets -- --cap-lints warn`, filtered to mind files
  and `src/registry.rs`: zero diagnostics. Unfiltered clippy fails on
  `main` itself (about 446 pre-existing pedantic diagnostics), so the filter
  is the honest gate.
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

This documentation pass itself checked: `cargo test --lib mind::config` (the
shipped example parses to the defaults), `cargo fmt --check`, and
`cargo test --test hygiene` — all clean on the branch that carries the
delivered actuator.

## Open Questions

1. **File ownership — settled by implementation, deployment unverified.**
   The question was how the mind (user `vivi`) may write into projects owned
   by someone else. The delivered answer: it never does. Every action is a
   server tool call authenticated by the mind's key; the server, which runs
   as the project's own service identity, does the writing. What remains
   unverified is only the operational wiring of keys on a real hub.
2. **Attach during an autonomous turn: lock or queue?** The queue is what is
   implemented: the session sits behind one turn lock, and an attached
   prompt waits, with a notice, until the wake ends (and vice versa).
   pi's `streamingBehavior: "steer"` — injecting the operator's message into
   the turn in flight — is still unexplored, as is suppressing autonomous
   wakes entirely while an operator is attached.
3. **Pi RPC protocol details.** Verified live against pi 0.73.1 on
   2026-10-25 (historical facts about pi, not about deployment):
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

   Still open: refusing extension UI requests.
4. **Cross-project read access.** One mind reads every project's pump state.
   Is that acceptable for every registered project, or should the registry
   carry a per-project `mind = false` opt-out instead of the mind-local
   ignore list? Should a project's letters ever be quoted into a wake about
   another project, given that one session spans them all?
5. **A watchdog for the mind itself.**
   - `Restart=on-failure` covers crashes. A wedged mind (a hung pi turn past
     `TURN_TIMEOUT`, or a stuck socket) needs a liveness signal: systemd
     `WatchdogSec` with `sd_notify`, or a heartbeat file checked by a
     separate timer unit.
   - Who tells the operator that the mind is down: the pump, the hub server,
     or an external check?
6. **Smaller questions.**
   - Should `resume-pump` be limited to pumps the mind itself paused? That
     would need a "paused by mind" marker the pump would have to carry; the
     menu's wording is guidance, not a gate.
   - Should the per-need budget also cap distinct needs per wake?
