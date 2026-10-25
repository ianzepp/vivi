# vivi-server Agent Guide

Public chat substrate server for vivi checkouts. Start with
[`README.md`](README.md) for what it is and how to run it, and with
[`docs/contract.md`](docs/contract.md) for the wire truth. Phase 02 work is
tracked in [`docs/phase-02-identity-and-sync.md`](docs/phase-02-identity-and-sync.md).

Phase 03 is built (1.1.0), recorded in
[`docs/phase-03-project-mailspace-hub.md`](docs/phase-03-project-mailspace-hub.md):
the project, not the room, is the tenant, and a project is a real vivi
mailspace on the server (`DATA_DIR/projects/<id>/.vivi/`) with a
`transport.sqlite` index beside it. Rooms are lightweight chat on a chat-mode project (item 17, 1.3.0).
Do not revive the seat/challenge rooms API or store chat under
`DATA_DIR/rooms/`.

Hosted projects (a mailspace already on the machine, served from its own
`.vivi/` through the host registry) are built, recorded in
[`docs/phase-04-hosted-projects.md`](docs/phase-04-hosted-projects.md).

Phase 05 is designed, not built: projects created and owned by ed25519
signing keys, GitHub-only `source`, and archive before purge with a 24-hour
delay, in
[`docs/phase-05-signed-projects.md`](docs/phase-05-signed-projects.md)
(operator decisions 2026-10-25).

## Vivi Mailspace

Development coordination runs through a project mailspace at `.vivi/`:
two roles, `mind` (coordinating seat — dispatch, merge cycle, deploy
checks) and `hand` (implementation seat — one handle, narrow
verification, no commits). Nothing under `.vivi/` is committed: the config
(`mailspace.toml`, which carries this mailspace's id), charters, keys,
store, blobs, and logs are all machine-local and git-ignored, so one
person's roles and charters never propagate into the repository. A
mailspace is a machine; its id and key must never be shared between
clones. After a fresh clone run `vivi mailspace init` and add the `mind`
and `hand` roles. The directory is excluded from the Docker context.

## Git Workflow

Standing authorization from the repo owner for agent development:

- Develop on a branch, not directly on `main`. Commit per unit of work.
- Once you are satisfied that development is complete and ready to merge,
  fast-forward `main` to the branch yourself. No PR and no human review is
  required.
- Fast-forward only: never force-push and never rewrite history on `main`.
  If `main` has moved, merge it into the branch, re-run the checks, then
  fast-forward.

## Deploy

The only deploy path is a tagged binary release installed on a self-hosted
server. Railway is retired (owner decision 2026-10-21; its project is
scheduled for deletion and its secrets were exposed in a Railway breach):
there is no staging service, no production service, and no deploy on a merge
to `main`. A merge to `main` deploys nothing.

A `v*` tag runs `.github/workflows/release.yml`, which checks that the tag
matches the `[package] version` in `Cargo.toml`, builds
`x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`, and publishes
`vivi-server-<target>.tar.gz` with a `.sha256` beside each to a GitHub
Release. Hosts verify the checksum before installing.

- A release is a version bump on `main` (Cargo.toml and Cargo.lock), then
  `git tag -a vX.Y.Z` on that commit and push the tag. **Ask the repo owner
  before bumping a version or pushing a tag.**
- The hub (the operator's server) is tailnet-only and runs the installed
  binary as the `vivi` user under systemd. Installing or restarting it is the
  owner's call (see the `hub` skill for the host).
- `BIND` sets the listen address (default `0.0.0.0`). A tailnet-only host sets
  its tailnet IP so the server never listens on the LAN.
- `STORE_ENCRYPTION_KEY` must not change for an existing data directory: the
  boot verifier exits the process when the key does not match a sealed store.
  Never put secret values in commits or docs.
- `Dockerfile` and `railway.toml` remain in the tree but are unused. Deleting
  them is the owner's call.

## Checks

The merge gate, run before fast-forwarding `main`:

```sh
cargo fmt --check
cargo test
VIVI=~/.local/bin/vivi scripts/project-sync-smoke.sh
VIVI=~/.local/bin/vivi scripts/two-checkout-smoke.sh
scripts/hosted-smoke.sh
```

All three smokes run against a LOCAL server that the script boots from
`target/debug/vivi-server`; do not point them at a deployed server (there is
none to point at). Use a `vivi` binary at the version `Cargo.toml` pins.

Measured baselines (burgus, 18-core Mac, 2026-10-10, warm target):
`cargo fmt --check` under 1 s, `cargo test` about 8 s. Cold builds are
unmeasured; `scripts/hosted-smoke.sh` takes a few seconds; the two-checkout
smoke takes about two minutes (the prune leg waits for a 1 m cutoff).

`scripts/two-checkout-smoke.sh` is the release acceptance: the 1.3.0 rooms
wire over HTTP, then two checkouts and a third member syncing through a
durable project with `vivi sync`, item events, MCP, and prune/resync.
`scripts/project-sync-smoke.sh` is the project wire and store gate.
`scripts/hosted-smoke.sh` checks that a hosted project is the host's own
mailspace: a CLI-made host registered with `vivi pump service add --serve`,
served by the local server, and driven over MCP and the CLI against the same
files. Narrower scripts: `scripts/chat-room-smoke.sh` (rooms) and
`scripts/mcp-smoke.sh` (MCP).

## Dependency on `vivi`

`Cargo.toml` pins `vivi` to a release tag (`tag = "v21.3.2"`) and `Cargo.lock`
pins the exact commit. Move it by changing the tag, running
`cargo update -p vivi`, and running the three smokes. Keep the lockfile on the
`vivi` git source: the `.cargo` path
patch used for local development is git-ignored and must never be committed
(a lockfile that resolved to the path patch fails the release build).
