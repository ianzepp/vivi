# vivi-server

Public chat substrate server for vivi checkouts: a rendezvous point where
two `vivi` installations, anywhere on the internet, join one persistent
room and exchange messages through the `vivi chat` CLI alone. Rooms are
real mailspaces — server storage for room `<id>` is
`DATA_DIR/rooms/<id>/.vivi/`, opened through the vivi crate, with chat
messages stored as ordinary mail items so future phases can cross needs
and receipts over the same store. Identity is ED25519 challenge/sign seat
binding with digest-only bearer tokens; message bodies are encrypted at
rest (AES-256-GCM); the wire truth is [`docs/contract.md`](docs/contract.md).

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
| `DATA_DIR` | Room storage root (`DATA_DIR/rooms/<id>/.vivi/`) |
| `STORE_ENCRYPTION_KEY` | AES-256-GCM key for bodies; a boot-time verifier exits the process on mismatch instead of serving an unreadable or empty store |
| `MODERATION_TOKEN` | Gates `GET /v1/_health` (room count, storage state) |

## Acceptance

`scripts/two-checkout-smoke.sh` is the end-to-end gate: two vivi
checkouts, a locally spawned server, create → join → send → badge →
watch → offline queue → flush. It is the deploy acceptance; the deployed
URL must pass it too.

## Railway deploy

- A volume mounted at `/data` is required — rooms are persistent and
  `DATA_DIR` must survive restarts.
- The lockfile must keep the `vivi` git source
  (`https://github.com/ianzepp/vivi`). The `.cargo` path patch used for
  local development is local-only and uncommitted; a build whose lockfile
  resolved to the path patch will fail on Railway.
- Deployed at <https://vivi-server-production.up.railway.app> (default
  domain; custom hostname open).

## Contract

[`docs/contract.md`](docs/contract.md) is the frozen wire truth: every
endpoint, error code, auth flow, and the revision record (R1–R6). Client
units build against it; changes after freeze are explicit revisions
recorded there.

## License

MIT
