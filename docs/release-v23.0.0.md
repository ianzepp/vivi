# Vivi 23.0.0

Major release on 22.1.0. The server is configured by one file under one home
instead of environment variables; machines sign in with their own keys
instead of bearer tokens; the server runs every project's pump itself; repos
are served over Git HTTP; one-job tickets let a bare worker machine pull
everything it needs; and a project can name an upstream project on another
server. Upgrading a host is a deliberate step: see **Upgrading a host** below.

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
