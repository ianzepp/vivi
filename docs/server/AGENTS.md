# vivi-server Agent Guide

Public chat substrate server for vivi checkouts. Start with
[`README.md`](README.md) for what it is and how to run it, and with
[`docs/contract.md`](docs/contract.md) for the wire truth. Phase 02 work is
tracked in [`docs/phase-02-identity-and-sync.md`](docs/phase-02-identity-and-sync.md).

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
  `main` redeploys it. After merging, wait for the deploy, then run the smoke
  against it and report the result:

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

## Checks

Run before merging:

```sh
cargo fmt --check
cargo test
```

`scripts/two-checkout-smoke.sh` is the end-to-end acceptance (see the README);
run it for changes that touch the wire or the `vivi` dependency.

## Dependency on `vivi`

`Cargo.toml` tracks `vivi` on `branch = "main"` and `Cargo.lock` pins the
exact commit. Keep the lockfile on the `vivi` git source: the `.cargo` path
patch used for local development is git-ignored and must never be committed
(a lockfile that resolved to the path patch fails on Railway).
