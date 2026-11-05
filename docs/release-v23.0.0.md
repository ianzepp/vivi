# Vivi 23.0.0

Major release on 22.1.0. The server is configured by one file under one home
instead of environment variables; machines sign in with their own keys
instead of bearer tokens; the server runs every project's pump itself; repos
are served over Git HTTP; one-job tickets let a bare worker machine pull
everything it needs; and a project can name an upstream project on another
server. The pump was tested against real workers and a Docker lab and comes
out with about fifteen flow defects fixed, a tick that is cheap when nothing
has changed (about 0.2-0.3 s, from about 20 s), and the workspace split into
domain crates. Upgrading a host is a deliberate step: see **Upgrading a
host** below.

## Breaking changes

- **The server reads no per-setting environment variables.** `vivi server
  run` reads `$VIVI_HOME/server/config.toml` (`VIVI_HOME` defaults to
  `~/.vivi`) and secret files in `$VIVI_HOME/server/creds/`. `PORT`, `BIND`,
  `DATA_DIR`, `SEND_RATE_PER_MINUTE`, `HOSTED_DEFAULT_IDENTITY`,
  `HOST_PROJECTS_ROOT`, `HOST_LANES_ROOT`, `HOST_GIT_URL_PREFIX`,
  `HOST_DEFAULT_POOL`, `RUST_LOG` (for the server), `CREDENTIALS_DIRECTORY` and
  secrets in the environment are no longer read. A `creds/` directory or
  secret readable by other users is refused.
- **The host registry moved** from `<config dir>/vivi/pump-service.toml` to
  `$VIVI_HOME/server/registry.toml`. `VIVI_CONFIG_DIR` is gone.
- **The mind's home is `$VIVI_HOME/mind/`** (`mind.toml`, state, socket,
  sessions, log). `VIVI_MIND_HOME` is gone.
- **The registry's `pump` flag is retired.** The server starts a pump child for
  every registered project; the project's own pump control (running, paused,
  stopped) decides whether it works. On first start, each `pump = false`
  entry is moved into its project's control state (stopped) once and the
  entry is rewritten `pump = true`.
- **`vivi server run` supervises pumps.** Run it instead of `vivi pump service
  run`. While a separate pump service holds the lock, the server serves but
  does not supervise.
- **The pump log's `collect took Ns` line is gone.** Each tick logs
  `tick took Nms: integration, collect, flow, dispatch, board` and
  `flow took: ... (N runs, slowest <need> r<run> Nms)`; a command that takes
  500 ms or more is logged as `slow command Nms: ...`. Anything that parsed
  the old line must read the new one.
- **A hand may change files outside its slice's write scope** (operator
  ruling 2026-11-04). The runner lists the files the published commit
  changed; a path no scope entry covers is posted on the need thread, raised
  once as `flow-scope-exceeded`, and added to the slice's deviations, with the
  open slices that hold those paths named. Nothing is refused.
- **A runner ref delete reports `push_refused` or `still_present`** where it
  said `verify_failed` for every failure after the lease check.

## Server home and init

- `config.toml` keys: `bind`, `port`, `send_rate_per_minute`,
  `hosted_default_identity`, `log`, `data_dir`, `projects_root`, `lanes_root`,
  `git_url_prefix`, `default_pool`. Every key is optional; relative paths are
  under `$VIVI_HOME/server`; an unknown key is an error. `vivi server run
  --help` lists them with defaults.
- `vivi server init [--port N]` makes the home: a starter `config.toml`,
  `creds/` (700) with fresh secrets (600; an existing secret is never
  replaced), the data, projects and lanes directories, and this machine's key
  in `host_keys`. Safe to run again.

## Machine keys

- Each machine has one ed25519 key at `$VIVI_HOME/keys/machine.ed25519`
  (`vivi key show`). A machine signs in by challenge and gets a one-hour
  session for `/v1/mcp`, Git and remote mode; clients sign in again once when
  a restarted server refuses a cached session.
- Host is any key listed in `$VIVI_HOME/server/host_keys`. `vivi mcp --host`
  signs in as host.
- New host tools: `machine_add` (attach a key to a principal), and
  `grant_add` accepts a project name and defaults the member when the project
  has one. Challenges are capped (8 per key, 4096 in total).
- The stdio `vivi mcp` bridge on a machine key signs in again once when a
  restarted server refuses its session.
- `vivi mcp` finds its server from `--server`, `VIVI_MCP_SERVER`, the nearest
  mailspace's `[remote]`, then `[sync]`, then this machine's server home.

## Remote mode

- A `[remote]` table (`server`, `project`, optional `key_file`) in a
  mailspace's `mailspace.toml` sends the mapped CLI commands (`need
  send|list|show|done|reopen`, `mail reply|thread`, `board`, `boot`) to that
  server. Unmapped commands are refused by name.

## Projects and pumps

- Host tools `project_register {root}` / `project_unregister {root}` serve an
  existing mailspace (absolute path) or stop serving it; nothing on disk
  changes.
- `vivi server pump pause|resume|status` stops every project pump the server
  supervises, or lets them run again (`$VIVI_HOME/server/pump-state`).
- On shutdown the server waits for its pump children to stop.
- Pump-start disk sweeps keep a RAM disk or APFS volume whose local-pool job is
  still running; they remove only disks whose job ended or has no owner.

## Git over HTTP

- `vivi server run` serves each target's lane at
  `/git/<project>/<target>.git`. `vivi key git-credential` is a Git credential
  helper that signs in with the machine key; configure it per server URL (see
  its `--help`). The machine needs a grant on the project.
- `main`, `vivi/main` and the target's main are refused for non-host pushes;
  Git prints the reason as a remote rejection.

## Job tickets and workers

- Host tool `job_create` mints a ticket for one job: one project and target,
  one branch to push, one receipt. `job_show` / `job_list` read jobs back.
- `vivi pool run <server> --job <id> [--detach]` is the worker: ticket on
  stdin, slots from `$VIVI_HOME/worker/config.toml`, then manifest, clone,
  harness, commit, gate, push, receipt. Exit 0 / 1 / 2 / 75 (busy).
- Pool provider `ticket` (`[pools.<name>.ticket]`: `host`, `server`, and
  optional `tunnel`, `worker_server`, `vivi`, `worker_home`, `project`,
  `target`) dispatches through tickets over `ssh -R`, so the dispatcher opens
  no inbound port. Standing rules are not yet carried to the worker, and
  read-only jobs are refused on this provider.

## Project origin

- A hosted project's `[origin] server, project` names an upstream project on
  another server. Handle tools (`show`, `thread`, `reply`, `done`, `reopen`,
  `mark`, `unmark`, `depend`) on a handle that is not local go to the origin;
  `send` stays local; `list` and `board` take `origin` (`origin` or `all`,
  items tagged `source`); `vivi need list --origin`. The server signs in to
  its origin with its own machine key. Chains stop at 4 hops.

## Pump flow

- A spent provider quota holds the step for the operator instead of burning
  attempts; an agent that printed nothing is raised as `flow-agent-silent`,
  not sent to envelope correction.
- The slicer fan-out and the hand classifier read role replies the way the
  collectors do (a prose preamble before the envelope is accepted).
- A retried slice is a repair: a slice whose merge went red starts from its
  own failed commit with the failure in its brief; a conflict starts from the
  need-branch tip with the conflicting paths. A hand the pump refused although
  it answered `built` keeps its pushed work, and the retry is told why.
- An exhausted slice holds only the slices that depend on it; independent
  slices still build.
- A run restarted at slice slices the earlier run's admitted plan. A rework
  brief lists the merged slices of every run it continued through, the slices
  that did not land with their hand's last message, and the CTO ruling that
  restarted it; a slice a later run landed is no longer listed as missing.
- A queued build merge to main is cut from main as it is when dispatched, not
  when queued. Merges go one per target, so with many needs finishing
  together a merge cut at queue time found main moved and queued again; in the
  lab 20 of 30 needs ended locked. Repair merges keep their tested base.
- Read-only role jobs start from their target's main, not `[pump] main`.
- `vivi pump requeue` releases a step held after its start failed.
- `[pump.flow.stages]` switches admit, audit, judge and the need rung off;
  a switched-off stage answers by rule and records the skip.
- The attempt record names the pushed branch, the model and where the attempt
  started; the hand contract says `[[check]]` describes the final tree.
- The pump writes its own charters for the auditor, merge and head-cto
  roles (they no longer borrow Tugboat's), and the slicer charter says a
  slice that uses another slice's result depends on it.
- A failed ref cleanup names what the runner found.

## Pump speed

- One SSH connection per host is shared across vivi processes (control
  sockets in `vivi-ssh-<uid>` in the temporary directory, or `/tmp` when that
  path is too long, as on macOS; mode 0700, refused unless private; 60 s idle).
  The pump's own Git uses it through `GIT_SSH_COMMAND` unless the operator
  set `GIT_SSH_COMMAND` or `GIT_SSH`. A pinned (RunPod) runner keeps a private
  socket. An `ls-remote` over a shared connection: about 5 ms, from 185 ms.
- Dispatch looks at a seeding or settling slot after 100 ms and backs off to
  the old interval, instead of sleeping a fixed 2 s and 3 s: about 1 s per
  dispatch, from about 3 s. The pump waits for each command from 1 ms up to
  50 ms, instead of a fixed 50 ms.
- The pool runner retries a remote Git command when SSH refused the connection
  before login (three tries, 2 then 4 s apart): sshd's default `MaxStartups
  10:30:100` drops logins when a dozen jobs connect at once.
- The step-error holds need `max(3, ceil(60 s / tick))` failing ticks, so a
  short `[pump] tick` (5 s is now a good value) does not turn a brief outage
  into holds for the operator.
- Concurrent pump log lines no longer run together.
- Measured in the lab: a quiet tick about 0.3 s, a busy one about 8 s (from
  22 s); 30 needs with every injected failure landed in 744 s (from 990 s).

## Development

- The root crate is split into domain crates under `crates/` (cold build of
  the binary about 35 s -> 18.5 s); `lettre` is gone.
- `sim/` drives the real pump loop on a virtual clock; `lab/` runs the real
  binary on Docker hub and worker containers with a misbehaving stand-in
  agent, regression scenarios with `[expect]` checks (`lab/run-regress.sh`),
  and stress scenarios (`lab/gen-multi.py`).

## Upgrading a host

Do these in order on the host, with the old server and pump service stopped.

1. Install 23.0.0.
2. `vivi server init --port <the old PORT>` to make `~/.vivi/server/`. It keeps
   nothing from the old setup on its own.
3. Write the old environment into `~/.vivi/server/config.toml`:

   | Old variable | `config.toml` key |
   |---|---|
   | `BIND` | `bind` |
   | `PORT` | `port` |
   | `DATA_DIR` | `data_dir` |
   | `SEND_RATE_PER_MINUTE` | `send_rate_per_minute` |
   | `HOSTED_DEFAULT_IDENTITY` | `hosted_default_identity` |
   | `HOST_PROJECTS_ROOT` | `projects_root` |
   | `HOST_LANES_ROOT` | `lanes_root` |
   | `HOST_GIT_URL_PREFIX` | `git_url_prefix` |
   | `HOST_DEFAULT_POOL` | `default_pool` |
   | `RUST_LOG` | `log` |

4. Replace the secrets `init` just made with the old ones, one file each in
   `~/.vivi/server/creds/` (mode 600, directory 700): `STORE_ENCRYPTION_KEY`
   must be the old key, or the existing data cannot be read.
   `MODERATION_TOKEN` and `HOST_OPERATOR_TOKEN` likewise, if they were set.
5. Move the registry: `<config dir>/vivi/pump-service.toml` to
   `~/.vivi/server/registry.toml`. Move `mind.toml` to `~/.vivi/mind/`.
6. Remove the environment, `CREDENTIALS_DIRECTORY` / `LoadCredential` and the
   separate pump service from the service unit; run `vivi server run`.
7. Check the log: the server home line, `host_keys lists N machine key(s)`, one
   `hosting project …` per project, and `started project pump` for each
   project whose pump control is running.
8. Admit the machines that will use the host: `vivi key show` on each, then
   `machine_add` and `grant_add` as host.

A host that runs a local or edge build, not a release, should check that the
build's commit is on `main` before installing 23.0.0: work that is only in
that build is not in this release.
