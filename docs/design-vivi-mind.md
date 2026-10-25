# Delivery: `vivi mind`, a persistent hub-wide supervisor

Status: **exploratory spike** on `spike/vivi-mind`, written 2026-10-25 for
agent review. Nothing here is released. The branch carries a compiling,
tested skeleton for discussing the shape; this document is the
implementation spec for the non-spike follow-up.

Release decision: **defer-release** for the spike (no version bump, tag, or
release notes; the PR is a draft for review). Completing the follow-up units
below implies **release-prep**: a minor version with a
`docs/release-vX.Y.Z.md` note, per the repository's release convention.

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
acts only through letters and existing pump controls within an
operator-chosen authority stage.

## Normalized Spec

### Commands

The service verbs mirror `vivi pump service`. "Spike status" is what the
branch does today.

| Command | What it does | Spike status |
| --- | --- | --- |
| `vivi mind` / `vivi mind attach` | Attach a line REPL to the running mind's one session. Lines are prompts. `/status`, `/pause`, `/resume`, `/compact`, `/history` and `/quit` are verbs. | Real (socket client), against the fake in tests |
| `vivi mind status [--json]` | Authority, paused, busy, session id, and last wake. Falls back to the state file when no mind is listening. | Real |
| `vivi mind log [-n N]` | Tail of `<mind home>/mind.log`. | Real |
| `vivi mind pause` / `resume` | Stop or restart autonomous wakes and mutating actions. Attach keeps working. Goes through the socket, or through the state file when the mind is down. | Real |
| `vivi mind stop` | Ask the running mind to exit. The service manager decides whether to restart it. | Real |
| `vivi mind start` | Starting is the service manager's job. | Stub (explanatory error) |
| `vivi mind compact` / `history [--limit N]` | Forwarded to the pi session. | Real over the socket. The pi side is assumed (see below). |
| `vivi mind service add\|remove <root>` | Watch or ignore a project that is already registered. This edits the mind's own ignore list and never touches the host registry. | Real |
| `vivi mind service list` | Registered projects and whether the mind watches each. | Real |
| `vivi mind service status` | Whether a mind is listening, plus every path it uses. | Real |
| `vivi mind service run` | The foreground service, for a service manager. | Real loop. It needs a live pi, so it was not run here. |
| `vivi mind service unit` | Print an example hardened systemd unit. Installs nothing. | Real (template) |
| `vivi mind service install\|uninstall` | OS service install. | Stub (explanatory error) |

`vivi mind probe [--json]` (spike-only diagnostic) runs one read-only probe
pass plus the wake decision, with no running mind, no turn, and no write.

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
                                      │                 ─▶ actuator          │──▶ vivi pump <control>
                                      └──────────────────────────────────────┘    letters to the operator
```

The modules are listed under Repo-Aware Baseline.

#### Persistence and the one session

All context lives in **one** persistent pi harness session. Pi sessions are
JSONL files under `--session-dir`, resumable with `--session <id>`, and pi has
compaction built in. The scenario this has to support: the operator drops SSH,
comes back ten minutes later, runs `vivi mind`, and is back in the same
session. No tmux, and no choosing between sessions.

- The mind records the session id in `state.json` the first time it starts
  (assumed: from `get_state`) and passes `--session <id>` on every restart.
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
  home) and `VIVI_PROXY_URL` only.
- How pi is pointed at the proxy is an open question (see below).

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

### Authority, staged

The operator sets the stage explicitly in `mind.toml` (`authority = ...`) and
opens each one deliberately. Each stage includes the powers of the stages
below it.

| Stage | Opens | Spike status |
| --- | --- | --- |
| 1 `observe` (default) | Read status and logs; letters to the operator; escalate. | Gate real. Letters are not sent yet: the `DryRun` actuator logs them. |
| 2 `unstick` | `vivi pump requeue`, `replan`, `answer` and `unlock`, within per-need budgets (default 3 per need per 24 h), plus `vivi pump pause` (and resume of a pump the mind paused). | Gate, budget and argv real. Execution is dry-run. |
| 3 `file-fixes` | File fix needs to the vivi project through the normal pump, stopping at merged. | Gate real. No actuator. |
| 4 `upgrade-on-approval` | `vivi update --version` with verification and rollback, after the operator cuts a release. | Enum only. The config loader refuses it. |
| 5 `self-release` | Cut releases itself. Long term only, once things are stable. | Enum only. The config loader refuses it. |

The rules every action follows:

- **Only existing channels.** Every action goes out as a letter or as a pump
  control the pump already accepts. The mind never edits `.vivi`, git, or
  config directly. `ActionRequest::argv()` maps each action onto an existing
  `vivi pump ...` command line, and unknown kinds do not parse.
- **The model runs nothing itself.** A turn ends with a TOML envelope between
  `+++` lines (the shape cadence head roles already use), and the mind decides
  what happens to each `[[action]]`:
  - `deny_unknown_fields`.
  - Handles and answer words must be short `[A-Za-z0-9_-]` tokens that do not
    start with `-`, so flag and shell injection is refused.
  - `project` must be a watched root.
  - Notes are capped at 500 characters, and an envelope at 8 actions.
  - Budget is spent as actions pass, so two requeues of one need in one
    envelope count twice.
  - The gate's verdicts are logged and replayed to the next wake as
    `Action results:`.
- **Pause is the brake.** A paused mind takes no autonomous wakes and no
  mutating actions. Reads and letters still pass.

The rules every action follows:

- **Only existing channels.** Every action goes out as a letter or as a pump
  control the pump already accepts. The mind never edits `.vivi`, git, or
  config directly. `ActionRequest::argv()` maps each action onto an existing
  `vivi pump ...` command line, and unknown kinds do not parse.
- **The model runs nothing itself.** A turn ends with a TOML envelope between
  `+++` lines (the shape cadence head roles already use), and the mind decides
  what happens to each `[[action]]`: `deny_unknown_fields`; handles and answer
  words must be short `[A-Za-z0-9_-]` tokens not starting with `-`; `project`
  must be a watched root; notes are capped at 500 characters and an envelope
  at 8 actions; budget is spent as actions pass; the gate's verdicts are
  logged and replayed to the next wake as `Action results:`.
- **Pause is the brake.** A paused mind takes no autonomous wakes and no
  mutating actions. Reads and letters still pass.

#### Untrusted input

Letters, logs, pump health reasons and hand outputs are untrusted, because
they can carry prompt injection.

- `prompt::fence` wraps every such string in `<<<untrusted source="..">` ...
  `untrusted>>>`.
- It defuses any fence marker or `+++` line inside the text, so the text
  cannot close its fence or forge an envelope.
- It clips each field to 2000 characters.

The narrow, validated action surface is the second line of defence, and the
stage gate is the third.

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
- The `vivi pump` control verbs the actuator maps onto: `requeue`, `replan`,
  `answer`, `unlock`, `pause`, `resume`.

The spike branch adds, under `#[cfg(unix)]`, `src/mind.rs`, `src/mind/*.rs`
with `_test.rs` siblings, `src/cli/mind_command.rs`, the `Command::Mind`
wiring in `src/cli.rs`, `src/main.rs` and `src/local_mailspace_command.rs`,
and `tests/mind_cli.rs`.

| Module | Role |
| --- | --- |
| `config` | `<config_dir>/vivi/mind.toml`, beside `pump-service.toml`: the authority stage enum, heartbeat, proxy URL (loopback only), pi command, model, budgets, and paths. `VIVI_MIND_HOME` overrides the home. |
| `authority` | The action surface and `permit(gate, ledger, action, need)`, a pure gate over stage, pause and per-need budget. |
| `wake` | `decide()` and `finish()`: heartbeat, backoff, in-flight block, rolling-hour cap, signal de-duplication. |
| `probe` | `Probe` trait. `HealthProbe` = `pump::health` verdict plus the read-only `pump.db` reads and the `log.txt` tail described in Wakes. |
| `projects` | Watched roots: the registry's pumped entries, less the mind's ignore list. |
| `store` | `<mind home>/state.json`, updated under a sidecar lock. The mind's own state, **never** the pump's `state.extra`. |
| `actions` | The TOML action envelope, `plan()` (validate, gate, spend), and `argv()`. The `Actuator` trait has only a `DryRun` impl. |
| `prompt` | Wake prompts with untrusted fencing. |
| `pi` | The `PiSession` trait, the assumed JSONL codec, `ProcessPi` (subprocess, every wait bounded) and `FakePi`. |
| `socket` | The attach protocol, `bind()`, `serve()` (request lines capped while read), peer-uid check, `Client`. |
| `service` | `tick()`, one wake end to end, testable with fakes; `serve_forever()`, the real loop. |
| `commands` | CLI dispatch, the attach REPL, and `probe`. |

Real and unit-tested: config, the gate, the wake scheduler, registry
reading, the state store, envelope parsing and argv, prompt fencing, the pi
codec and `ProcessPi` against a scripted `sh` child, the socket server and
client, one full `tick()` with fakes, the store reads of the probe, and the
CLI surface (temp `VIVI_CONFIG_DIR` and `VIVI_MIND_HOME`).

Real but not exercised live: `ProcessPi` against a real pi (pi is not on the
build box), `serve_forever`, `respawn_if_exited`.

Stubbed: the actuator (dry-run only; no `vivi pump` call, no letter), held
steps in the probe, ageing a running step that has no job, `vivi mind start`, `service install|uninstall`,
`file-fixes` execution, stages 4 and 5.

## Stage Graph

```text
G0 ownership decision (OQ1) ──┬─▶ U3 actuator ──▶ U4 letters ──▶ U9 file-fixes
                              │
U1 pi protocol ──▶ U2 pi wiring ─────────────┐
                                             ├─▶ G2 live observe run ─▶ G3 unstick live
U5 held/orphan steps ────────────────────────┤
U6 watchdog ──▶ U7 service install ──────────┘
U8 attach policy (OQ2), independent
U10 docs and release note, last
```

U1, U5, U6 and U8 can run in parallel: their write scopes are disjoint.
U3, U4 and U9 serialize on `src/mind/actions.rs`.

## Implementation Work

```text
delivery: docs/design-vivi-mind.md
unit: U1-pi-protocol
write_scope: src/mind/pi.rs, src/mind/pi_test.rs
edit: confirm the RPC against a real pi (session flags, get_state session id, get_messages shape, agent_settled on errors and aborts) and correct the codec
done_when: ProcessPi round-trips prompt, compact, history and session_id against a real pi; the scripted test mirrors the observed records
sanity: cargo test --lib mind::pi
```

```text
delivery: docs/design-vivi-mind.md
unit: U2-pi-wiring
write_scope: src/mind/pi.rs, src/mind/config.rs
edit: point pi at VIVI_PROXY_URL with a dummy key, disable pi's own tools, refuse extension UI requests
done_when: a turn reaches a model only through 127.0.0.1:18180 and pi cannot run bash or edit
sanity: cargo test --lib mind::pi mind::config
```

```text
delivery: docs/design-vivi-mind.md
unit: U3-actuator
write_scope: src/mind/actions.rs, src/mind/actions_test.rs, src/mind/service.rs
edit: a real Actuator that runs the planned argv (or the vivi-server API, per G0) with a timeout, logging output as untrusted
done_when: at stage unstick a permitted requeue runs once and its result is replayed to the next wake; refused actions never run
sanity: cargo test --lib mind::actions mind::service
```

```text
delivery: docs/design-vivi-mind.md
unit: U4-letters
write_scope: src/mind/actions.rs, src/mind/prompt.rs
edit: send operator letters and escalations through the channel chosen in OQ6
done_when: an escalate action at stage observe delivers one letter to the operator
sanity: cargo test --lib mind::actions
```

```text
delivery: docs/design-vivi-mind.md
unit: U5-held-and-orphan-steps
write_scope: src/mind/probe.rs, src/mind/probe_test.rs, src/mind/store.rs (or src/pump/store/flow_index.rs if the pump records step times)
edit: read steps held for an operator answer from pump.db (StepState::Waiting at a non-lock answer node) into held_steps; age a running step with no job (a step `state_at` column, which is a pump schema change, or a first-seen time in the mind's store)
done_when: fixtures with a step awaiting an answer, and with a step running with no job past the threshold, are alarming and fingerprint stably
sanity: cargo test --lib mind::probe
```

```text
delivery: docs/design-vivi-mind.md
unit: U6-watchdog
write_scope: src/mind/service.rs, src/mind/vivi-mind.service.example
edit: a liveness signal (sd_notify WATCHDOG or a heartbeat file) refreshed by the wake loop, not by pi
done_when: a wedged wake loop is restarted by the service manager
sanity: cargo test --lib mind::service
```

```text
delivery: docs/design-vivi-mind.md
unit: U7-service-install
write_scope: src/mind/commands.rs, src/mind/install.rs (new), src/mind/install_test.rs
edit: vivi mind service install|uninstall, mirroring pump::service::install
done_when: install writes the hardened unit for the vivi user and uninstall removes it; tests use temp dirs only
sanity: cargo test --lib mind::install
```

```text
delivery: docs/design-vivi-mind.md
unit: U8-attach-policy
write_scope: src/mind/socket.rs, src/mind/socket_test.rs
edit: apply the OQ2 decision (queue, steer, or suppress wakes while attached)
done_when: the chosen behaviour is tested over a real socket in a temp dir
sanity: cargo test --lib mind::socket
```

```text
delivery: docs/design-vivi-mind.md
unit: U9-file-fixes
write_scope: src/mind/actions.rs, src/mind/authority.rs
edit: the file-fix action: file a need to the vivi project through the normal pump, stopping at merged
done_when: at stage file-fixes one need is filed per permitted action and counted against the budget
sanity: cargo test --lib mind::actions mind::authority
```

```text
delivery: docs/design-vivi-mind.md
unit: U10-docs-release
write_scope: docs/design-vivi-mind.md, docs/README.md, docs/release-vX.Y.Z.md, README.md
edit: user-facing mind docs and the release note
done_when: docs describe shipped behaviour only; release note written at release-prep
```

## Checkpoints And Gates

- **G0, ownership (blocks U3, U4, U9).** Decide OQ1: shared group, ACLs, or
  every action through vivi-server. No write stage ships before it.
- **G1, unit sanity.** Each unit's `sanity` command, `cargo fmt --check`, and
  `cargo test --test hygiene`.
- **G2, live observe run (after U1, U2, U5, U6).** On a sandbox hub (never a
  real mailspace): the mind runs as a service at stage `observe`, survives a
  pi kill and an SSH drop, and wakes on a locked need.
- **G3, unstick live (after G0, U3, G2).** On the sandbox, the mind
  requeues or unlocks one stuck need within budget and reports it.
- **Release gate.** Full `cargo test`, `cargo clippy --all-targets` with no
  new diagnostics in mind files, docs updated, then release-prep.

## Validation

Spike validation (2026-10-25, debug build, box):

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

The follow-up proves its outcome at G2 and G3, not with unit tests alone.

## Companion Skill Plan

- **factory**: run the units above as plan, implement, verify, review and
  commit phases, with a deferred-findings ledger and the release decision.
- **polish**: a per-file pass over every touched mind file before each PR,
  `polish(scope):` commits with `Polish-Primary` / `Polish-Supporting`
  trailers, and `.polish-inspected.json` for clean files.
- **delivery**: keep this document the spec; amend it when G0 or OQ2 is
  decided instead of re-deriving the architecture.

## Open Questions

1. **File ownership of project `.vivi` folders vs the `vivi` user.** The pump
   controls the mind would run (`vivi pump requeue` and the rest) post letters
   into `<project>/.vivi/mail.sqlite`. If projects are owned by the operator's
   user and the mind runs as `vivi`, the mind needs write access. The options:
   - a shared group with setgid directories and `UMask=0007`,
   - ACLs on `.vivi/`, or
   - routing every mind action through vivi-server's API, which already runs
     as `vivi`, so the mind needs no filesystem write anywhere.

   The last keeps `ReadWritePaths` down to the mind home, which is attractive.
   Health reads also need read access to each pump home and to `/proc/locks`.
2. **Attach during an autonomous turn: lock or queue?** The spike queues: the
   session sits behind one turn lock, and an attached prompt waits, with a
   "waiting" notice, until the wake ends. A wake that comes due during an
   attached conversation also waits.

   The alternative is pi's `streamingBehavior: "steer"`, which injects the
   operator's message into the turn in flight. That is better for "stop, don't
   do that", but riskier: the operator's text and untrusted wake context mix
   in one turn.

   A related question: should an attached operator suppress autonomous wakes
   entirely until they detach?
3. **Pi RPC protocol details to confirm.** These come from pi's docs and have
   not been verified against a running pi:
   - the exact session flags (`--session <id>` vs a file path; whether
     `--session` creates a session when the id is missing);
   - where `get_state` reports the session id;
   - the `get_messages` data shape;
   - whether `agent_settled` always follows a started prompt, including on
     provider errors and aborts;
   - how to point pi at the proxy (a custom provider in pi's models config
     with base URL `VIVI_PROXY_URL`, and a dummy key the proxy ignores?);
   - how to disable pi's own tools (bash, edit) so the envelope is the only
     action path, for example `--no-tools` or an allow-list;
   - extension UI requests, which the mind should refuse.
4. **Cross-project read access.** One mind reads every project's pump state.
   Is that acceptable for every registered project, or should the registry
   carry a per-project `mind = false` opt-out instead of the mind-local ignore
   list? Should a project's letters ever be quoted into a wake about another
   project, given that one session spans them all?
5. **A watchdog for the mind itself.**
   - `Restart=on-failure` covers crashes. A wedged mind (a hung pi turn past
     `TURN_TIMEOUT`, or a stuck socket) needs a liveness signal: systemd
     `WatchdogSec` with `sd_notify`, or a heartbeat file checked by a separate
     timer unit.
   - Who tells the operator that the mind is down: the pump, vivi-server, or
     an external check?
6. **Smaller questions.**
   - Should `resume` be limited to pumps the mind itself paused? That would
     need a "paused by mind" marker the pump would have to carry.
   - Should letters to the operator go through `vivi mail send` into one
     designated project, or through a hub-level channel?
   - Should the per-need budget also cap distinct needs per wake?

### Review asks

- Is the module split right, and is `src/mind/` the right home rather than
  under `src/pump/`? It is kept out of `pump` on purpose, for independence.
- Is the envelope (with model tools disabled) the right action path, as
  opposed to giving pi a narrow custom tool set?
- The stage table: is `pause` correctly in `unstick` rather than `observe`?
- Open question 1 (ownership) blocks every write stage. Does the vivi-server
  API route look right?
- Is the de-duplication by fingerprint enough, or should a pump that stays
  stalled get a faster re-wake than the heartbeat?
