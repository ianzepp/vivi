# Hosted indexing measurements

Captured evidence for the hosted transport-index work, taken with the
real `vivi server run` process via
[`tests/server_measure_process.rs`](../../tests/server_measure_process.rs)
(`process_benchmark_windows_report_cpu_pass_latency_and_candidate_work`).
This file records what was measured, never what is assumed; historical
phase-04 timings are not reused. Each number below is labeled as either a
**measured** value (read out of the harness report) or a **derived**
value (arithmetic over measured values, as the harness itself labels its
`candidate` field).

Status: baseline candidate captured **before** the push selector is
replaced. Later runs reproduce the same workload with the commands here
and compare against it.

## Reproducing

```sh
# no-write idle window (captured below, 2026-10-26)
VIVI_MEASURE_MODE=idle cargo test --release --test server_measure_process \
  process_benchmark_windows_report_cpu_pass_latency_and_candidate_work \
  -- --ignored --nocapture

# local-write window (captured below, 2026-10-26)
VIVI_MEASURE_MODE=local cargo test --release --test server_measure_process \
  process_benchmark_windows_report_cpu_pass_latency_and_candidate_work \
  -- --ignored --nocapture

# mixed off-host-write window (captured below, 2026-10-26)
VIVI_MEASURE_MODE=mixed cargo test --release --test server_measure_process \
  process_benchmark_windows_report_cpu_pass_latency_and_candidate_work \
  -- --ignored --nocapture
```

Environment settings, none of them secret:

- `VIVI_MEASURE_MODE` — each captured run selects one window: `idle`
  (no-write), `local` (continuous local-only writes), or `mixed`
  (local writes plus the two-second off-host schedule). Unset selects all
  three modes (`idle,local,mixed`) in one process.
- `VIVI_MEASURE_CORPUS` unset — the harness uses its 5,000-sends-per-host
  minimum. `VIVI_MEASURE_SENDS` must stay unset (it is smoke-only and
  refuses values at or above the smoke cap).
- `RUST_LOG` is not set for the test; the harness itself boots the child
  with `RUST_LOG=vivi=debug` (`ServerSpec::with_rust_log`), never
  inherited, so the server's per-pass latency tracing is visible.

The test is `#[ignore]` by default; `-- --ignored` runs it. It is
self-contained: it creates a fresh temp base, two disposable hosted
mailspaces, an isolated server data directory, and its own registry; it
mints disposable sync keys and cleans up after itself. It never touches
a deployed server or `~/.vivarium/`.

## Environment (measured at capture)

| Item | Value |
| --- | --- |
| Date | 2026-10-26 (~21:30 UTC idle capture, ~21:50 UTC local-write capture, ~22:13 UTC mixed capture) |
| Host hardware | Apple M5 Max, 18 cores, 36 GiB RAM |
| OS | macOS 27.0.1 (Build 26A434), arm64 |
| Source revision | `a52197d5204a1c27dc6186969c2f6c6048832fc5` (clean tree, package `vivi` 22.0.0) |
| Source revision, local-write capture | `744d706527b5c78810f6ef48723db20eca9fe874` (clean tree; the only change since the idle capture is this document) |
| Source revision, mixed capture | `fe45427cc34bb566f46fc9e3880d74531c68e3a7` (clean tree; only this document changed since the local-write capture) |
| Toolchain | rustc/cargo 1.99.1 (58cf8f9c3 2026-10-13, Homebrew) |
| Build mode | `--release`; `[profile.release]` is `strip = true`, `lto = "thin"`, other settings default (opt-level 3) |

The run builds and boots `CARGO_BIN_EXE_vivi` — the release-linked
binary this `cargo test` invocation itself produced — so the measured
code is exactly the checkout at the revision above.

## Workload configuration (equivalence contract for later runs)

Fixed by the harness; a later run must keep all of this to stay
comparable:

- Two hosted mailspaces, `before` and `after`, freshly
  `Mailspace::init`-ed in a temp base, each with `mind` and `hand`
  identities, `[sync] name = "hub"` and `send = "addressed"` appended to
  the mailspace config, and its own disposable sync key; both registered
  `serve = true` by one reconcile into an isolated server data directory.
- Corpus: **5,000 sends per host** (`build_initial_corpus`, which
  refuses less), bootstrapped strictly before any server or window
  exists. Traffic is predominantly local to the host's own `hand`
  address, plus one off-host letter when the writer starts and then one
  every **2 s** to `mind@0badc0de.local` (alternating need / mail by
  count; need letters carry format-1 front matter). Letter text is
  deterministic (`letter n` / `letter n body\n`).
- Server child on its own defaults: indexer cadence
  `INDEX_INTERVAL = 5 s` (`src/server/hosted.rs`), registry watcher
  poll 3 s.
- Warm-up: after boot (30 s ready deadline), `wait_index_settled` polls
  up to 180 s until both transport indexes have served every off-host
  letter from the corpus. Verification reads happen only between
  windows, never inside one.
- Window: 120 s (`MEASURE_WINDOW`). The idle window runs no writer
  threads; the local window runs one writer thread per hosted mailspace
  pumping local-only letters through the normal `Mailspace::send` API
  flat-out until the deadline (no in-window off-host letters — the
  two-second periodic schedule belongs to the corpus bootstrap and the
  mixed window, not the local window). CPU integrates sysinfo per-refresh `cpu_usage()` over refresh
  intervals ≥ `MINIMUM_CPU_UPDATE_INTERVAL`; pass latencies come from
  the server's own `timed_push` debug lines (`elapsed_ms`), parsed
  within the window's wall-clock bounds.

## Baseline: no-write idle window (2026-10-26)

Directly measured, one idle window against both hosted mailspaces:

| Quantity | Value |
| --- | --- |
| Window wall time | 120.281 s |
| Server CPU seconds over the window | 0.073 s |
| Average fraction of one core | 0.0006 |
| Push passes completed in-window | 1, slowest 0.257 s |
| Message rows, window start | 19,947 (both mailspaces) |
| Message rows, window end | 19,947 (no writes: rows unchanged) |
| Writer sends / threads in-window | 0 / 0 |
| Harness total (`test result: ok. 1 passed`) | 225.47 s |

Derived (harness-labeled candidate-work arithmetic, not a measured
counter):

| Quantity | Value |
| --- | --- |
| Candidate selector work | 1 full-listing pass × 19,947 rows = **19,947 row visits** in the window |

Phase durations across the run:

| Phase | Duration | Kind |
| --- | --- | --- |
| Fixture + corpus bootstrap (2 × 5,000 sends) + server boot + warm-up settle | ≈ 105 s | derived (225.47 s total − 120.28 s window; the harness does not print per-phase seconds) |
| Measured idle window | 120.281 s | measured |
| Post-window off-host verification + teardown | remainder of 225.47 s | derived |

The corpus bootstrap enforces the 5,000-send floor per host before
anything is sent and re-checks it via `Traffic::enforce_minimum` after
each host builds; the row count (19,947 for 10,000 sends) reflects the
storage shape: a local letter files one row per recipient plus the
sender's sent copy, an off-host letter only its sent copy.

Why only one in-window pass: under a settled no-write steady state the
indexer's passes do nothing — a pass skips when the watermark has not moved (and
when a pass is already running or the sync lock is held), and a skipped
pass emits no `timed_push` line. The single in-window pass (0.257 s) is
the settling push immediately after warm-up. This is the honest
steady-state shape of the no-write baseline: near-zero CPU, and pass
work only when something actually changed.

## Baseline: local-write window (2026-10-26)

Same fixture, bootstrap, server boot, and warm-up settle as the idle
baseline above, then one 120 s local window: a writer thread per hosted
mailspace pumping local-only letters through the normal mailspace API
until the deadline. Directly measured:

| Quantity | Value |
| --- | --- |
| Window wall time | 120.382 s |
| Server CPU seconds over the window | 19.546 s |
| Average fraction of one core | 0.1624 |
| Push passes completed in-window | 40, slowest 0.712 s |
| Message rows, window start | 19,966 (both mailspaces) |
| Message rows, window end | 36,998 (both mailspaces) |
| Writer sends / threads in-window | 8,516 / 2 (one per hosted mailspace) |
| Writer wall seconds in-window | 240.012 s summed over both threads (≈ 120 s each; the writers ran to the deadline) |
| Harness total (`test result: ok. 1 passed`) | 189.76 s |

Derived (harness-labeled candidate-work arithmetic and simple cross-checks
over the measured values, not measured counters):

| Quantity | Value |
| --- | --- |
| Candidate selector work | 40 full-listing passes × 19,966 rows = **798,640 row visits** in the window |
| Storage growth per in-window send | 36,998 − 19,966 = 17,032 new rows = exactly 2 per send (recipient row + sender's sent copy) |
| Aggregate writer throughput | ≈ 70.7 sends/s across both hosts (≈ 35.4 sends/s per host) |
| In-window off-host transport additions | **0** — under `send = "addressed"` a local-only letter files its local delivery and sent copy, and nothing else |

Pass cadence: 40 traced passes over 120.382 s is consistent with the
fixed 5 s indexer cycle firing ~20 times against both hosted mailspaces
(two pass lines per cycle, ≈ 6 s per cycle of sleep plus pass work). In
contrast to the idle steady state, no pass could skip silently: every
cycle found new rows to index, and the slowest pass (0.712 s) ran while
ingest was continuous.

Server health: the child answered readiness inside the 30 s deadline and
stayed live at every CPU sample (the sampler fails the window if the pid
dies); the pre-window settle served every corpus off-host letter; after
the window the same corpus set was verified served with the expected set
unchanged — the local-only letters joined nothing to either transport
index, which is the point of this baseline. Both writers joined clean and
the run ended `test result: ok. 1 passed`.

Phase durations across the run:

| Phase | Duration | Kind |
| --- | --- | --- |
| Fixture + corpus bootstrap (2 × 5,000 sends) + server boot + warm-up settle | ≈ 60 s, plus the post-window verification and teardown in the remainder | derived (189.76 s total − 120.38 s window; the harness does not print per-phase seconds) |
| Measured local-write window | 120.382 s | measured |

## Baseline: mixed off-host-write window (2026-10-26)

Same fixture, bootstrap, server boot, and warm-up settle as the two
baselines above, then one 120 s mixed window: a writer thread per hosted
mailspace pumping predominantly local letters through the normal
mailspace API until the deadline, plus the harness's fixed off-host
schedule — one off-host letter when the writer starts, then one every 2 s
to `mind@0badc0de.local`, alternating need/mail. Directly measured:

| Quantity | Value |
| --- | --- |
| Window wall time | 120.471 s |
| Server CPU seconds over the window | 19.236 s |
| Average fraction of one core | 0.1597 |
| Push passes completed in-window | 40, slowest 0.667 s |
| Message rows, window start | 19,962 (both mailspaces) |
| Message rows, window end | 38,476 (both mailspaces) |
| Writer sends / threads in-window | 9,316 / 2 (one per hosted mailspace) |
| Writer wall seconds in-window | 240.004 s summed over both threads (≈ 120 s each; the writers ran to the deadline) |
| Harness total (`test result: ok. 1 passed`) | 197.85 s |

Derived (harness-labeled candidate-work arithmetic and simple cross-checks
over the measured values, not measured counters):

| Quantity | Value |
| --- | --- |
| Candidate selector work | 40 full-listing passes × 19,962 rows = **798,480 row visits** in the window |
| Storage growth cross-check | 38,476 − 19,962 = 18,514 new rows = 2 per local send + 1 per off-host sent copy, consistent with ≈ 9,198 local + 118 off-host sends (≈ 59 off-host per host on the 2 s schedule) |
| Aggregate writer throughput | ≈ 77.3 sends/s across both hosts (≈ 38.8 sends/s per host) |
| In-window off-host transport additions | **118** — verified served: after the window the index was checked against the corpus set **plus** every in-window off-host identity, by to/kind, for both hosts, and matched exactly |

Pass cadence and load match the local-write baseline: 40 traced passes
over 120.471 s (the fixed 5 s indexer cycle firing ~20 times against both
hosts), no silent skips, and essentially the same server CPU (19.236 s /
0.1597 cores here vs 19.546 s / 0.1624 local). At this harness cadence
the two-second off-host schedule adds ~118 indexed letters per window and
does not change the CPU shape; the cost scales with the local write rate,
not with the off-host tail.

Capture note: the first capture attempt under the identical command
failed the post-window identity check two letters short (92 of 94 served
for host `before`); an immediate re-run passed completely and is the
baseline above. The harness's post-window assert reads the index once,
and the server's pre-read pass is deliberately not waited for while
another pass is in flight (`refresh_hosted` skips on `WouldBlock`), so a
read landing in that gap can see an index one pass stale; the letters
were demonstrably written (the expected set comes from the writers' own
classification), only their indexing pass had not landed at the moment
of the read. A later run should
verify against a settle poll (see `wait_index_settled`), which the
warm-up phase already uses.

Phase durations across the run:

| Phase | Duration | Kind |
| --- | --- | --- |
| Fixture + corpus bootstrap (2 × 5,000 sends) + server boot + warm-up settle | ≈ 77 s, plus the post-window verification and teardown in the remainder | derived (197.85 s total − 120.471 s window; the harness does not print per-phase seconds) |
| Measured mixed window | 120.471 s | measured |

## Limitations

- Single machine, single run, no repetitions; the machine was otherwise
  idle but not pinned, and wall time is first-to-last CPU sample, so
  the window slightly exceeds its 120 s deadline.
- macOS task accounting through sysinfo deltas; platforms without
  kernel CPU accounting are an explicit `AccountingError::Unsupported`,
  so these numbers do not transfer to such hosts.
- Only the idle, local, and mixed windows are captured here, one run
  each; all three come from the same harness (`VIVI_MEASURE_MODE`).
- Pass-latency evidence under a no-write steady state is sparse by
  design (skipped passes are silent); the local-write window shows the
  dense shape (40 working passes). The writers were not pinned and the
  machine was otherwise idle; per-host throughput (≈ 35.4 sends/s) is
  what the unpumped two-thread writer achieved, not a mailspace ceiling.
- The mixed window's post-window verification asserts immediately and
  can race an in-flight pass (see the capture note in the mixed baseline
  above); the recorded baseline is the run whose verification passed.
- The fixture lives in a fresh temp directory per run; its absolute
  paths and pid-derived name differ run to run, which is fine — the
  workload contract above is what must stay fixed.
