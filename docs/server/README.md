# vivi-server

Public chat substrate server for vivi checkouts. Durable **projects**
(`/v1/projects`, vivi 11 `vivi sync`) are the tenant: one real vivi
mailspace at `DATA_DIR/projects/<id>/.vivi/` with a `transport.sqlite`
index beside it. The whole server exposes one Streamable HTTP MCP
endpoint at `POST /v1/mcp`: a Bearer server key (self-service mint at
`POST /v1/projects/{id}/keys`, or host-minted) names a principal whose
grants say which projects it reaches, and every project tool takes a
`project` argument. See
[`docs/contract.md`](docs/contract.md).

**Rooms** are fleeting.chat-style channels on that same store: a
lightweight project (`mode=chat`) with a TTL, keyless join by name, and
member-visible `kind=chat` letters. Create with `POST /v1/rooms`, share
the id, send and poll at `/v1/rooms/{id}/messages`, or point a connector
at `/v1/rooms/{id}/mcp` (the room's own MCP; no server key). See
[`docs/phase-03-project-mailspace-hub.md`](docs/phase-03-project-mailspace-hub.md)
item 17 and [`docs/contract.md`](docs/contract.md).

**Hosted projects** are the other kind: an existing vivi mailspace on the
server's machine, listed in the host registry with `serve = true`, is served
directly from its own `.vivi/` (no copy), with MCP acting on it live through
the `vivi` library. See
[`docs/phase-04-hosted-projects.md`](docs/phase-04-hosted-projects.md).

## Quickstart

Requires Rust 1.93+ (toolchain pinned in `rust-toolchain.toml`).

```sh
git clone https://github.com/ianzepp/vivi-server.git
cd vivi-server
PORT=8080 \
DATA_DIR=./data \
STORE_ENCRYPTION_KEY=<32-byte-key> \
MODERATION_TOKEN=<secret> \
cargo run
curl http://localhost:8080/healthz   # -> ok
```

| Env var | Meaning |
| --- | --- |
| `BIND` | Listen address (default `0.0.0.0`); a tailnet-only host sets its tailnet IP |
| `PORT` | Listen port |
| `DATA_DIR` | Storage root: durable and chat projects at `DATA_DIR/projects/<id>/.vivi/` |
| `STORE_ENCRYPTION_KEY` | AES-256-GCM key for bodies; a boot-time verifier exits the process on mismatch instead of serving an unreadable or empty store |
| `MODERATION_TOKEN` | Gates `GET /v1/_health` (room count, storage state) |
| `HOST_OPERATOR_TOKEN` | The built-in host principal (`host`) on `POST /v1/mcp`: the host tools only — `create_project`, `lane_create`, `delete_project`, `project_list`, `server_version`, and key/grant admin. Unset, the principal does not exist and only server keys authenticate. An identity token is never accepted there |
| `HOST_PROJECTS_ROOT` | Project tools: existing directory that holds project roots (`<root>/<name>`) |
| `HOST_LANES_ROOT` | Project tools: existing directory that holds each project's lanes (`<root>/<name>/<target>.git`) |
| `HOST_GIT_URL_PREFIX` | Project tools: lane URLs are `<prefix><name>/<target>.git` (for example `git@hub:`) |
| `HOST_DEFAULT_POOL` | `create_project`: optional pool whose `remote_url` is set to the first target's URL; the tool's `pool` overrides it |

The server writes both roots itself, so they belong to the user it runs as. The three project tools refuse with one error naming every one of the first three that is unset.
| `SEND_RATE_PER_MINUTE` | Writes per minute per identity (rooms, the projects API, and MCP writes share it). Unset is 30, the public default; a positive integer sets the limit; `0` is unlimited (for a trusted tailnet host). Any other value refuses to boot |
| `HOSTED_DEFAULT_IDENTITY` | The vivi identity an API key without its own `identity` acts as on a hosted project (default `operator`) |

## Acceptance

`scripts/project-sync-smoke.sh` is the project gate: two synced vivi 11
checkouts and a locally spawned server, 84 legs (letters byte for byte,
addressing, visibility, windows, handles, prune and resync). Run it for any
change to the project wire or store; `SERVER_URL` points it at a deployed
server.

`scripts/hosted-smoke.sh` is the hosted-project gate: a host mailspace made
with the CLI, registered with `vivi pump service add --serve`, served by a
local server, and driven over MCP and the CLI against the same files. It
boots its own server; there is no deployed mode.

`scripts/chat-room-smoke.sh` is the rooms gate: create → join → send →
poll → MCP `kind=chat` against a local or deployed server. Each run leaves
one throwaway chat project behind.

```sh
scripts/chat-room-smoke.sh
SERVER_URL=https://vivi-server-main-production.up.railway.app scripts/chat-room-smoke.sh
```

`scripts/two-checkout-smoke.sh` is the release gate: the 1.3.0 rooms wire
over HTTP, then two vivi checkouts and a third member syncing through a
durable project (`vivi sync`), item events, MCP, and prune/resync. It needs
a vivi 11 binary (`VIVI=...`).

## Deploy

Self-hosted only. Railway was retired on 2026-10-21 (its project is being
deleted; the secrets it held were exposed in a Railway breach and are not
reused anywhere). A `v*` tag publishes checksummed binaries (see
`.github/workflows/release.yml`); a host verifies and installs one and runs
it under its own service manager. The operator's hub runs it tailnet-only
(`BIND` = its tailnet address) as the `vivi` user, serving the projects its
host registry marks `serve = true` (see
[docs/phase-04-hosted-projects.md](docs/phase-04-hosted-projects.md)).

- Each server needs its own `DATA_DIR`, `STORE_ENCRYPTION_KEY` and
  `MODERATION_TOKEN`; the boot verifier exits on a key mismatch with an
  existing sealed store, so back the key up.
- The Dockerfile and `railway.toml` are unused leftovers.
- The lockfile must keep the `vivi` git source
  (`https://github.com/ianzepp/vivi`), so tagged builds resolve it.

## Contract

[`docs/contract.md`](docs/contract.md) is the frozen wire truth: every
endpoint, error code, auth flow, and the revision record (R1–R6). Client
units build against it; changes after freeze are explicit revisions
recorded there.

## License

MIT
