# vivi-sim: the pump simulator

Runs the real pump run loop (`vivi_core::pump::run::step`: state reload,
control check, config refresh, recorded tick) against a simulated world on a
virtual clock. No model is called and nothing compiles. An idle pass costs
about 0.27 ms, so a virtual hour takes about 50 ms and a virtual day about
1.2 s (measured 2026-11-03 on the laptop, M5 Max).

`sim/` is its own Cargo workspace. `cargo build` and `cargo test` at the vivi
root never compile it; it builds into `sim/target/`.

## Run

```sh
cd sim
cargo run --release                 # every scenario
cargo run --release -- idle         # scenarios whose directory name contains "idle"
cargo run --release -- --verbose    # also list the commands the world does not model yet
```

Exit status: 0 when every scenario passed, 1 when any failed, 2 when a
scenario could not be read or set up.

Measured 2026-11-03 on the laptop (M5 Max): cold release build about 70 s,
about 20 s after a change in `vivi-core`.

## Layout

```text
sim/
  src/main.rs       discover scenarios, run them, report
  src/scenario.rs   the scenario.toml format
  src/project.rs    a throwaway mailspace with a ready pump config, and the
                    pump context built from it the way `vivi pump run` does
  src/host.rs       SimHost: the virtual clock and the world's answers
  src/run.rs        the tick loop and the expectation checks
  scenarios/<NNN-name>/scenario.toml
```

## Scenario format

One directory per scenario, numbered so they run in order. Unknown keys are
refused, so a misspelled expectation fails instead of passing silently.

```toml
name = "idle-one-worker"
description = "What this scenario proves."

[world]
workers = 1        # worker pools, each a simulated machine
slots = 1          # slots per pool

[run]
virtual_hours = 24
tick_secs = 20     # virtual seconds between pump ticks

[expect]           # every bound is a maximum
exceptions = 0
tick_errors = 0
commands = 0       # optional; unbounded when absent
max_wall_secs = 5
```

## How it works

The pump reaches the outside world only through the `Host` trait
(`crates/vivi-core/src/pump/host.rs`): `now`, `run`, `spawn`, `log`. Git, `vivi pool` dispatch
and checks are all commands through `run`. `SimHost` implements that trait:
`now` reads a counter the driver advances after each pass, where the real
pump would sleep, and `run` answers
from the world's rules. A command no rule answers gets exit 0 and no output,
and is counted as unscripted (`--verbose` lists them), which shows what the
world does not model yet.

## Known gaps

- The world is idle: no needs, no jobs, no worker behaviour, no faults. Those
  are the next scenarios.
- Some pump files still read the real clock instead of `Host::now` (for example
  `hold.rs`, `service.rs`, `jobs.rs`, `health.rs`, `requeue_job.rs`,
  `priority.rs`, and the tick id in `cycle.rs`). The sim calls `run::step`, so
  the sleeping loops are skipped, but timestamps from those files are real time.
- `BoardPass` runs on its own thread, so runs are not yet fully deterministic.
