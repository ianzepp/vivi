# vivi-server

Public chat substrate server for vivi checkouts. Durable **projects**
(`/v1/projects`, vivi 11 `vivi sync`) are the tenant: one real vivi
mailspace at `<data_dir>/projects/<id>/.vivi/` with a `transport.sqlite`
index beside it. The whole server exposes one Streamable HTTP MCP
endpoint at `POST /v1/mcp`: a Bearer server key (self-service mint at
`POST /v1/projects/{id}/keys`, or host-minted) names a principal whose
grants say which projects it reaches, and every project tool takes a
`project` argument. See
[`contract.md`](contract.md).

**Rooms** are fleeting.chat-style channels on that same store: a
lightweight project (`mode=chat`) with a TTL, keyless join by name, and
member-visible `kind=chat` letters. Create with `POST /v1/rooms`, share
the id, send and poll at `/v1/rooms/{id}/messages`, or point a connector
at `/v1/rooms/{id}/mcp` (the room's own MCP; no server key). See
[`phase-03-project-mailspace-hub.md`](phase-03-project-mailspace-hub.md)
item 17 and [`contract.md`](contract.md).

**Hosted projects** are the other kind: an existing vivi mailspace on the
server's machine, listed in the host registry with `serve = true`, is served
directly from its own `.vivi/` (no copy), with MCP acting on it live through
the `vivi` library. See
[`phase-04-hosted-projects.md`](phase-04-hosted-projects.md).

## Quickstart

`vivi server` is part of the vivi binary (a `vivi-server` link to it runs the
same server). Build it from the vivi repository, or install a release.

```sh
vivi server init --port 8080     # makes ~/.vivi/server: config.toml, creds/, data dirs
vivi server run                  # foreground; put it under a service manager for real use
curl http://localhost:8080/healthz   # -> ok
```

`vivi server run --help` lists the config keys and credential files, and
`vivi server run --version` prints the version. The server takes no other
arguments and refuses any it does not know.

## Configuration

The server reads **no per-setting environment variables**. `VIVI_HOME`
(default `~/.vivi`) is the only one; the server home is `$VIVI_HOME/server`.
It reads `config.toml` and the secret files in `creds/`; relative paths in the
config are under the home. `vivi server init [--port N]` writes a starter
`config.toml` only when none exists, makes `creds/` (mode 700) with each
missing secret (mode 600, fresh random, never replaced, never printed), lists
this machine's key in `host_keys`, and makes the data, projects and lanes
directories. Run it again at will.

`config.toml`, every key optional (an unknown key is an error):

| Key | Meaning (default) |
| --- | --- |
| `bind` | Listen address (`127.0.0.1`); a host that serves other machines names the interface, for example its tailnet IP |
| `port` | Listen port (`8080`) |
| `data_dir` | Storage root: durable and chat projects at `<data_dir>/projects/<id>/.vivi/` (`data`) |
| `send_rate_per_minute` | Writes per minute per identity (rooms, the projects API, and MCP writes share it). `30` by default; a positive integer sets the limit; `0` is unlimited (for a trusted tailnet host) |
| `hosted_default_identity` | The vivi identity an API key without its own `identity` acts as on a hosted project (`operator`) |
| `log` | Log filter (`vivi_server=info,vivi_core::pump=info,tower_http=info`) |
| `projects_root` | Project tools: directory that holds project roots (`<root>/<name>`) (`projects`) |
| `lanes_root` | Project tools: directory that holds each project's lanes (`<root>/<name>/<target>.git`) (`lanes`) |
| `git_url_prefix` | Project tools: lane URLs are `<prefix><name>/<target>.git`, for example `git@hub:` (`file://<lanes_root>/`) |
| `default_pool` | `create_project`: optional pool whose `remote_url` is set to the first target's URL; the tool's `pool` overrides it |

The server writes both roots itself, so they belong to the user it runs as. `archive_project` needs only `lanes_root`; `restore_project` and `purge_project` work from the paths recorded in the archive record and need no root setting at all. Archive is the default retirement — it unregisters the project and keeps every tree in place — while `purge_project` alone destroys data, and only after a fixed 24-hour delay since the archive stamp.

### Secrets

One file per secret in `$VIVI_HOME/server/creds/`. The server refuses a
`creds/` or a secret that other users can read, and names the fix
(`chmod 700 creds && chmod 600 creds/*`).

| File | Meaning |
| --- | --- |
| `STORE_ENCRYPTION_KEY` | AES-256-GCM key for bodies: standard base64 of exactly 32 bytes (`openssl rand -base64 32`); a boot-time verifier exits the process on mismatch instead of serving an unreadable or empty store. It must not change for an existing data directory: back it up |
| `MODERATION_TOKEN` | Gates `GET /v1/_health` (room count, storage state); without it the route is absent |
| `HOST_OPERATOR_TOKEN` | The built-in host principal (`host`) on `POST /v1/mcp`: the host tools only — `create_project`, `lane_create`, `archive_project`, `restore_project`, `purge_project`, `project_list`, `server_version`, and key/grant admin. Without it, the principal does not exist and only server keys authenticate. An identity token is never accepted there |

`host_keys` (beside `config.toml`) lists the machine keys that may act as host,
one base64 public key per line. The host registry is
`$VIVI_HOME/server/registry.toml`. The server supervises the pumps of the
projects it registers.

On Linux the process makes itself non-dumpable at boot
(`prctl(PR_SET_DUMPABLE, 0)`). The kernel then keeps `/proc/<pid>/mem` from
other processes running as the same user, and no core dump is written. Secrets
are read from files and never sit in the environment block.

## Acceptance

`scripts/server/project-sync-smoke.sh` is the project gate: two synced vivi 11
checkouts and a locally spawned server, 84 legs (letters byte for byte,
addressing, visibility, windows, handles, prune and resync). Run it for any
change to the project wire or store; `SERVER_URL` points it at a deployed
server. Each script makes a throwaway `VIVI_HOME` with `vivi server init` and
boots `vivi server run` from it.

`scripts/server/hosted-smoke.sh` is the hosted-project gate: a host mailspace
made with the CLI, registered with `vivi pump service add --serve`, served by a
local server, and driven over MCP and the CLI against the same files. It
boots its own server; there is no deployed mode.

`scripts/server/chat-room-smoke.sh` is the rooms gate: create → join → send →
poll → MCP `kind=chat` against a local or deployed server. Each run leaves
one throwaway chat project behind.

```sh
scripts/server/chat-room-smoke.sh
SERVER_URL=https://<your-server> scripts/server/chat-room-smoke.sh
```

`scripts/server/two-checkout-smoke.sh` is the release gate: the 1.3.0 rooms wire
over HTTP, then two vivi checkouts and a third member syncing through a
durable project (`vivi sync`), item events, MCP, and prune/resync. It needs
a vivi 11 binary (`VIVI=...`).

## Deploy

Self-hosted only. Railway was retired on 2026-10-21 (its project is being
deleted; the secrets it held were exposed in a Railway breach and are not
reused anywhere). A host installs the vivi release and runs `vivi server run`
under its own service manager, with `VIVI_HOME` set if the home is not
`~/.vivi`. The operator's hub runs it tailnet-only (`bind` in `config.toml` =
its tailnet address) as the `vivi` user, serving the projects its host
registry marks `serve = true` (see
[phase-04-hosted-projects.md](phase-04-hosted-projects.md)).

- Each server needs its own home: its own `data_dir` and `creds/`
  (`STORE_ENCRYPTION_KEY`, `MODERATION_TOKEN`); the boot verifier exits on a
  key mismatch with an existing sealed store, so back the key up.
- `deploy/server/` (Dockerfile, `railway.toml`) is an unused leftover; the
  image runs with `VIVI_HOME=/data` and expects `server/config.toml` and
  `server/creds/*` on the mounted volume.

## Contract

[`contract.md`](contract.md) is the frozen wire truth: every
endpoint, error code, auth flow, and the revision record (R1–R6). Client
units build against it; changes after freeze are explicit revisions
recorded there.

## License

MIT
