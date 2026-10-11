# vivi-server Agent Guide

Public chat substrate server for vivi checkouts. Start with
[`README.md`](README.md) for what it is and how to run it, and with
[`docs/contract.md`](docs/contract.md) for the wire truth. Phase 02 work is
tracked in [`docs/phase-02-identity-and-sync.md`](docs/phase-02-identity-and-sync.md).

A direction change is drafted in
[`docs/phase-03-project-mailspace-hub.md`](docs/phase-03-project-mailspace-hub.md):
the project, not the room, becomes the tenant, and the room concept goes away.
Until it is built, the room-based contract is the shipped behavior. Do not
optimize or extend the rooms API (polling, cleanup, privacy) without checking
that draft first.

## Vivi Mailspace

Development coordination runs through a project mailspace at `.vivi/`:
two roles, `mind` (coordinating seat — dispatch, merge cycle, deploy
checks) and `hand` (implementation seat — one handle, narrow
verification, no commits). Charters in `.vivi/charters/` are committed.
Everything else under `.vivi/` is machine-local and git-ignored: the
config (`mailspace.toml`, which carries this mailspace's id), keys, store,
blobs, and logs. A mailspace is a machine, so its id and key must never be
shared between clones. After a fresh clone run `vivi mailspace init` and add
the `mind` and `hand` roles; the charters then apply. The directory is
excluded from the Docker context.

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

Two Railway services, one per branch:

- **Staging** (`vivi-server-main`) deploys `main` at
  `https://vivi-server-main-production.up.railway.app`. Every fast-forward of
  `main` redeploys it. After merging, confirm the deploy succeeded on the
  merged commit (see Railway below). For changes to code, the wire, or the
  `vivi` dependency, then run the smoke against it and report the result; a
  docs-only merge needs only the deploy check, since every smoke run leaves a
  throwaway room behind:

  ```sh
  SERVER_URL=https://vivi-server-main-production.up.railway.app scripts/two-checkout-smoke.sh
  ```

- **Production** deploys `release` at
  `https://vivi-server-production.up.railway.app`. A push to `main` does not
  redeploy it.

Advancing `release` is a production deploy and is **not** covered by the
standing authorization above: do it only when the repo owner asks. When
asked, fast-forward only, from a `main` commit that has passed the checks
below and the staging smoke, then run the smoke against production:

```sh
git push origin <main-sha>:release
```

## Railway

Project `vivi-server` (`e58a80d2-1354-47d7-89eb-e60ac7a51834`) has one
environment, `production` (`2812c193-7363-43cb-b5de-b6cdc4588ad4`), which holds
both services:

| Service | ID | Deploys | URL |
| --- | --- | --- | --- |
| `vivi-server-main` (staging) | `0a95c904-f75c-475c-b9ab-564277307bce` | `main` | `https://vivi-server-main-production.up.railway.app` |
| `vivi-server` (production) | `1a754647-ef11-42a7-a5ef-7ec0b7ba3ed6` | `release` | `https://vivi-server-production.up.railway.app` |

The Railway connector's grant is account-wide: it sees every project in every
workspace the owner has, and both services share one environment, so nothing in
Railway separates staging from production. These rules are the only separation:

- Touch only the `vivi-server` project. Never act on the owner's other projects.
- Staging only, unless the owner asks. Any change to the production service
  (redeploy, variables, source) needs an explicit ask, like advancing `release`.
- Always pass `serviceId` and `environmentId` to `set-variables`. Omitting
  `serviceId` writes environment-wide shared variables, which production
  inherits.
- Never read or echo production's variable values. `describe-service` lists
  variable names only; do not call `list-variables` on production.
- Never generate or set production's `STORE_ENCRYPTION_KEY`. The boot verifier
  exits the process when the key does not match an existing sealed store, so a
  rotation needs the owner and a plan for the data. Staging is the same: changing
  its key means clearing its volume first.
- Never put secret values in commits or docs.

To see what is live, use `list-deployments`: it reports each deployment's status
and exact commit hash. `/healthz` is identical across builds and cannot tell
them apart. `railway.toml` overrides the dashboard: the dashboard shows the
`RAILPACK` builder, but builds use the Dockerfile.

## Checks

Run before merging:

```sh
cargo fmt --check
cargo test
```

Measured baselines (burgus, 18-core Mac, 2026-10-10, warm target):
`cargo fmt --check` under 1 s, `cargo test` about 8 s. Cold builds are
unmeasured; the smoke script takes about two minutes (the prune leg waits
for a 1 m cutoff).

`scripts/two-checkout-smoke.sh` is the end-to-end acceptance (see the README);
run it for changes that touch the wire or the `vivi` dependency.

## Dependency on `vivi`

`Cargo.toml` tracks `vivi` on `branch = "main"` and `Cargo.lock` pins the
exact commit. Keep the lockfile on the `vivi` git source: the `.cargo` path
patch used for local development is git-ignored and must never be committed
(a lockfile that resolved to the path patch fails on Railway).
