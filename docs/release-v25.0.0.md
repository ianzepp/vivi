# Vivi 25.0.0

Major release on 24.0.0. Pool slots can be declared one by one per
repository, persist between jobs, keep a warm cargo target directory, and run
a fixed check after the agent. A new read-only web console under `/ui` shows
every hosted project's needs, jobs, slots, pump and mail. Command cadence
checks now file, reply on and close their own red needs. The board and the
backlog graph no longer slow down quadratically as a mailspace grows. Beside
the `vivi` binary, the repository now carries two separate tools: a Temporal
runtime for large projects (`temporal/`) and a batch-and-bisect lander
(`lab/lander/`). The pump, flow and pool remain the runtime for projects that
do not need them. Read **Breaking changes** before upgrading a host.

## Breaking changes

- **Mailspace storage schema 12.** Schema 10 indexes letters by normalized
  Message-ID, schema 11 adds the `slot_runs` table, and schema 12 indexes
  backlog graph nodes by subgraph and source id. A 25 binary upgrades a store
  the first time it opens it. A 24 binary then refuses that store ("newer than
  this vivi supports"). Upgrade every binary that opens a mailspace together,
  the server and pump children included.
- **`vivi monitor --root`.** The monitor's repeatable option for extra
  project roots is now `--root`. Its old name, `--project`, clashed with the
  global `--project`: a debug build panicked on any `vivi monitor` call, and
  which option a release build filled was not defined.

## Pool and slots

- **Declared slots.** A mailspace can declare `[[slot]]` entries, each bound
  to one repository (`owner/repo`), one host and one directory, with an
  optional list of eligible pools. A job claims a free slot of its
  repository by name. The directory is the working tree itself and persists
  between jobs, so unchanged sources and ignored build output survive. A
  pool with no declared slots keeps its counted slots as before.
- **Per-host busy limit.** `max_busy` in a host's `~/vivi-worker/worker.conf`
  caps the jobs running or reserved on that host across every mailspace and
  pool. Unset means unlimited.
- **Warm cargo target per slot.** Opt-in worker cache configuration keeps a
  persistent cargo target directory for each slot, invalidated when the
  compiler or size limit changes, with optional sccache.
- **Post-agent check.** A fixed check command runs in the job's own slot
  after the agent finishes.
- **Integration ref in every checkout.** Each pool job fetches the
  integration ref into its checkout.
- **Shared slot mirror.** Slots of one repository on one host are worktrees
  of one full bare mirror. Fetches no longer go shallow, and jobs that start
  together retry each other's lock collisions instead of failing.
- **macOS process groups.** On macOS a slot's processes are tracked by
  process group, because the OS no longer shows process environments.
  Studio jobs no longer see their agent as still running after it ends.
- **SSH master recovery.** A worker whose multiplexed SSH master refuses new
  sessions is recovered instead of failing every dispatch.
- **`vivi slot`.** `slot_runs` keeps one row per pool job, written in the
  same transaction as the pool event it comes from. `vivi slot list`, `show`
  and `history` read it without contacting any host; `vivi slot rebuild`
  replays the events to repair it. A failed index write is logged and never
  fails the pool event.
- **Landing outcomes and CPU time.** `vivi pool landed` records a
  `pool_landed` event (landed, red, conflict or raced, with base, tip, time
  and the failing command or conflicting files). Job results carry
  `agent_cpu_s` and `check_cpu_s`.

## Pump

- **Command cadence red needs.** A red command check with no open red need
  files one; a later red replies once on it; a green closes it and records
  the green commit. The need's subject names the role, target and commit,
  not the command line.
- **Flow switched off means flow jobs wait.** With `flow = false`, flow jobs
  left pending stay queued; cadence jobs, command checks and digests still
  dispatch.
- **Results collected by someone else.** When another caller's
  `pool reconcile` collected a pump job, the pump now reads and applies that
  result instead of leaving the job dispatched forever.
- The command cadence's wait for a new main tip is logged once per tip, not
  every tick.

## Server and console

- **Web console.** `vivi-console` is mounted at `/ui` on `vivi server run`.
  It lists every hosted project that is neither archived nor retired, with
  pages for overview, needs (attempts and flow), jobs, slots (with history),
  failures, pump, pools and roles, and mail. It reads through connections
  that cannot write and has no sign-in: run it on a trusted network only.
  See [`server/README.md`](server/README.md) and
  [`console-plan.md`](console-plan.md).

## Mailspace

- **No more quadratic board.** The board counts every need's runs from one
  read of the backlog graph (2,000 needs: 5.29 s → 0.05 s in a debug build).
  Want state, board wants, `board --graph` and backlog minting share one
  backlog index. `crates/vivi-core/tests/scaling.rs` guards the row counts.
- Short handles resolve and display through the message id, without loading
  the full handle index.

## Fixes

- **The per-host busy limit works on macOS.** `max_busy` checked that a
  reservation's guardian was alive by reading its environment with `ps -E`.
  macOS 27 shows no other process's environment, so every reservation looked
  dead and was deleted, and the limit never held on a Mac host. The guardian
  is now recognized by its own arguments, which `ps` shows on both systems.
- The declared-slot and per-slot cargo tests build their temp homes on the
  physical path. macOS's temp dir is under the `/var` symlink, which slots
  rightly refuse, so these tests failed on macOS and passed on Linux. A
  failing cargo-cache test no longer prints the job's whole environment.
- A test builds the whole `vivi` command tree through clap's own checks, so
  an option clash in any subcommand fails the gate.

## Separate tools in this repository

These are not part of the `vivi` binary and are not built by
`cargo build` or `cargo test` at the root.

- **`temporal/`: the Temporal runtime.** `vivi-temporal` runs need, slice and
  lander workflows on a self-hosted Temporal server, with per-host task
  queues, typed failure classes (provider, infra, work), resume from the last
  pushed commit, backpressure and a hot-file mutex. Vivi stays the record of
  needs and threads. It is meant for large projects; smaller projects can keep
  using the pump. See [`../temporal/README.md`](../temporal/README.md).
- **`lab/lander/`: compose, gate and lander.** Python scripts that compose
  candidate merges, gate them against a main baseline (rerunning new failures
  once to separate flaky tests), and batch-and-bisect a queue of branches.
  The lander itself is dry-run only; the Temporal runtime calls `compose` and
  `gate` as activities.
- **`deploy/temporal/`.** Install and upgrade scripts for a self-hosted
  Temporal server and Postgres under systemd.

## Documentation

- README, `AGENTS.md`, the embedded `vivi` skill and the server README now
  describe what this release ships: declared slots, `vivi slot`, the console,
  command cadence red needs, the separate tools, and the storage schema.

- [`orchestration-reset/`](orchestration-reset/README.md): the failure
  review behind the Temporal runtime, its phase 0 and phase 1 status, and
  audits of quadratic costs and storage writes.
- [`pump-failure-model-review.md`](pump-failure-model-review.md).

## Testing

Run on pepper (M5 Max, macOS 27.2.1) on 2026-11-21, each subsystem on its own:

| Subsystem | Command | Result |
|---|---|---|
| `vivi` workspace (mailspace, pool, pump, server, console, proxy, mind, monitor, doctor, report, update, skills) | `cargo test --workspace --no-fail-fast` | 6,178 passed, 14 ignored, 140 suites, 280 s |
| Temporal runtime | `cd temporal && cargo test` | 64 unit and 10 workflow tests passed, 13 s |
| Pump simulator | `cd sim && cargo run --release` | 1 scenario passed (24 virtual hours in 1.1 s) |
| Lander | `python3 lab/lander/test_lander.py` | all 9 checks passed |

Before this pass, six pool tests failed on macOS (the busy-limit bug and the
temp-path tests under **Fixes**). They had passed on Linux.
