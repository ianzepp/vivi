# vivi-server

Public chat substrate server for vivi checkouts. Durable **projects**
(`/v1/projects`, vivi 11 `vivi sync`) are the tenant: one real vivi
mailspace at `DATA_DIR/projects/<id>/.vivi/` with a `transport.sqlite`
index beside it. Each project exposes Streamable HTTP MCP at
`POST /v1/projects/{id}/mcp`.

**Rooms** are fleeting.chat-style channels on that same store: a
lightweight project (`mode=chat`) with a TTL, keyless join by name, and
member-visible `kind=chat` letters. Create with `POST /v1/rooms`, share
the id, send and poll at `/v1/rooms/{id}/messages`, or point a connector
at `/v1/rooms/{id}/mcp` (same tools as the project endpoint). See
[`docs/phase-03-project-mailspace-hub.md`](docs/phase-03-project-mailspace-hub.md)
item 17 and [`docs/contract.md`](docs/contract.md).

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
| `PORT` | Listen port |
| `DATA_DIR` | Storage root: durable and chat projects at `DATA_DIR/projects/<id>/.vivi/` |
| `STORE_ENCRYPTION_KEY` | AES-256-GCM key for bodies; a boot-time verifier exits the process on mismatch instead of serving an unreadable or empty store |
| `MODERATION_TOKEN` | Gates `GET /v1/_health` (room count, storage state) |

## Acceptance

`scripts/project-sync-smoke.sh` is the project gate: two synced vivi 11
checkouts and a locally spawned server, 84 legs (letters byte for byte,
addressing, visibility, windows, handles, prune and resync). Run it for any
change to the project wire or store; `SERVER_URL` points it at a deployed
server.

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

## Railway deploy

- Two Railway services, one per branch:
  - **Production** deploys the `release` branch at
    <https://vivi.fleeting.chat>. A push to `main` does
    not redeploy it. To release, fast-forward `release` to a `main` commit
    that has passed `cargo test` and the smoke
    (`git push origin <main-sha>:release`), then run the smoke against
    production.
  - **Staging** (`vivi-server-main`) deploys `main` at
    <https://vivi-server-main-production.up.railway.app>, so every merge
    to `main` is built and can be smoked before a release.
- Each service needs its own `/data` volume, `STORE_ENCRYPTION_KEY` and
  `MODERATION_TOKEN`. Never share a volume or reuse the production key: the
  boot verifier exits on a key mismatch with an existing sealed store.
- A volume mounted at `/data` is required — rooms are persistent and
  `DATA_DIR` must survive restarts.
- The lockfile must keep the `vivi` git source
  (`https://github.com/ianzepp/vivi`). The `.cargo` path patch used for
  local development is local-only and uncommitted; a build whose lockfile
  resolved to the path patch will fail on Railway.
- Deployed at <https://vivi.fleeting.chat>. The Railway default domain,
  `https://vivi-server-production.up.railway.app`, still answers.

## Contract

[`docs/contract.md`](docs/contract.md) is the frozen wire truth: every
endpoint, error code, auth flow, and the revision record (R1–R6). Client
units build against it; changes after freeze are explicit revisions
recorded there.

## License

MIT
