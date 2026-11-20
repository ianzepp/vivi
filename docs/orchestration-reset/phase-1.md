# Phase 1: a fast, honest gate

**Status**: 2026-11-18. The gate and batch-and-bisect lander are built (`lab/lander/`, `65183b41`), dry-run, and used for one real landing. Still open: the disk experiments, and a publish step inside the lander itself (the push below was done by hand).

## Gate, measured

- **Command:** the full workspace build and test of radix (`cargo test --workspace --lib --bins --tests`, doctests excluded) in the dedicated warm `land` tree on each studio. Since 2026-11-20 that tree is `/Volumes/Lanes/vivi/trees/faberlang/radix/land`, the S2 layout the Temporal workers use ([`temporal/README.md`](../../temporal/README.md)).
- **Pass rule:** no test fails that passes on the main sha's baseline.

| | studio | studio-02 |
|---|---|---|
| Warm gate | 205–220 s | about 330 s |
| Incremental build | 43–48 s | similar |
| Bytes written per gate | 12–14 GB | similar |
| Baseline failures on main f5bdf605 | 73 | 72 |

The plan assumed 9.4 min per gate, which was a near-cold build. At about 3.5 min on studio, the merge model's capacity estimate roughly doubles.

## Dry run and landing (41 radix branches, batch 8)

| Outcome | Count | Detail |
|---|---|---|
| Landed | 15 | One candidate, `5000179a`: 30 commits, 28 files, +1,143/−71. Gate green against the baseline, with no new failures |
| Code conflicts | 14 | All in the 3b2c49a9 corpus chain, editing the same `radix-mir-clif` files. They need rework on current main |
| Build failures | 2 | 3b2c49a9-a470 and a471 call `generate_call_expr` with 4 arguments; main now takes 5 |
| No-ops | 10 | Pin-file-only or empty diffs, or content already on main |

The dry run took 29 min of gate time for all 41 branches. The pin file caused none of the ejections.

**Landed 2026-11-18 (operator approved):**
- radix `vivi/main` went `f5bdf605` → `5000179a`, as a fast-forward with a lease on the old tip.
- git-sync carried it to GitHub.
- The 25 landed and no-op refs were archived to `refs/archive/vivi/unit/*`.
- The 13 dry-run candidate refs were deleted.
- `/vivi/orch/backlog` now reports 16 unit branches, every one needing rework.

## Findings filed (faberlang wants)

- `5f4a8297`: `radix_parser --lib` aborts on a stack overflow in `nesting_budget_test`, hiding every other parser test from the gate.
- `705ad5ce`: main fails 73 tests on the studios, mostly hardware-gated CUDA and Metal tests and ratchets. The gate needs a known-failure baseline until they are cfg-gated or fixed.

## Known limits

- Flaky tests: since 2026-11-20 the gate reruns a candidate's new failures once, by exact name; a test that passes then is reported as `flaky` and does not eject the batch. Seen: a Metal GPU timing test failed under a loaded studio and passed 12 of 12 when idle.
- `bytes_written` is the whole drive's delta, so it is accurate only while the cluster is idle.
- `CARGO_BUILD_JOBS=16` overrides the studio-wide `jobs = 2`.
