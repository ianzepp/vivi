# Temporal runtime spike (Phase 2)

This is the Phase 2 spike from
[`docs/orchestration-reset/README.md`](../docs/orchestration-reset/README.md).
It runs Vivi's need pipeline on Temporal with **stand-in activities** and a
scripted fault model. It does not call git, a model, or the vivi server.

It is its own Cargo workspace, like `sim/`. A `cargo build` or `cargo test`
at the vivi root never compiles it.

## Run it

```sh
cd temporal
cargo test
```

The only requirement is the `temporal` CLI on `PATH`. Set `TEMPORAL_CLI` to
use a different binary. Each test starts its own in-memory dev server on a
free port, through the SDK's `WorkflowEnvironment::start_local`. You do not
need to run `temporal server start-dev` first, and nothing touches a remote
host.

## What is here

| File | Contents |
|---|---|
| `src/need.rs` | `NeedWorkflow` (id `need-<handle>`): plan, then one child `SliceWorkflow` per slice in parallel, then close. A parked slice parks the need in the record, with a typed `ParkCause`. |
| `src/slice.rs` | `SliceWorkflow` (id `slice-<need>-<slice>`): implement → check → review → land. A red check, a review change request, or a Lander ejection loops back to implement with the failure text, on the branch `vivi/<need>/<slice>`, at most `max_work_rounds` times. |
| `src/lander.rs` | `LanderWorkflow`: one long-lived workflow. It takes `submit` signals, waits one batch window, gates the batch, and merges it on a pass. On a fail it bisects until each bad unit stands alone and is ejected. It signals `land_result` back to each slice. |
| `src/steps.rs` | The failure-class policy (below) and the activity options for each step class. |
| `src/activities.rs` | Stand-in `plan`, `implement`, `check`, `review`, `gate`, `merge`, `record`. |
| `src/fault.rs` | The fault script: `quota@N`, `lost@N`, `timeout@N`, `red@N`, `changes@N`, and `bad`. |
| `src/world.rs` | The stand-in world. It holds call counters, branches with fenced pushes, `main`, and an event log that the tests read. |
| `src/record.rs` | `NeedRecord`, the Vivi record trait (status, park, close), with an in-memory implementation. |

### Failure classes

| Class | Mechanism |
|---|---|
| Provider (429, quota) | The activity fails with an `ApplicationFailure` of type `provider`. It is marked non-retryable and carries `next_retry_delay` = Retry-After. The workflow reads both from the failure, sleeps on a durable timer, and runs the step again. That spends no Temporal attempt and no work round. After `max_provider_waits` the slice parks with `ProviderExhausted`. |
| Infra (lost host, cap reached) | `implement` heartbeats a `Checkpoint` (branch, head commit) after every pushed commit. A missed heartbeat (`lost`) or the start-to-close cap (`timeout`) makes Temporal retry, up to `max_infra_attempts`. The next attempt reads the checkpoint from `heartbeat_details()` and continues from that commit. When the attempts run out, the slice parks with `InfraExhausted`. |
| Work (red check, change request, ejection) | This is not an error. The activity returns it as a value, and the slice loops with the text as feedback. When the rounds run out, it parks with `WorkExhausted { rounds, last_failure }`. |
| Duplicate start | `need_start_options` sets the id `need-<handle>` with `RejectDuplicate`. Children use `RejectDuplicate` as well. |

### Fault scripts

A script entry is `kind@call`. `call` counts every invocation of that step
for that slice, Temporal retries included, starting at 1. The lab's
`plan.toml` uses the same shape.

| Entry | Effect |
|---|---|
| `quota@N` | `plan`, `implement` or `review` returns a provider 429 before doing any work. |
| `lost@N` | `implement` pushes one commit, then stops heartbeating. |
| `timeout@N` | `implement` pushes one commit, then keeps heartbeating with no progress until the cap. |
| `red@N` | `check` returns red. |
| `changes@N` | `review` asks for changes. |
| `bad` (slice flag) | The slice passes its own check but breaks the Lander's gate. |

## Tests and what each proves

`tests/phase2.rs` has one test for each Phase 2 pass criterion, plus one for
the park path. `src/fault_test.rs` covers the script parser.

| Test | Criterion | What it asserts |
|---|---|---|
| `provider_429_never_spends_an_attempt` | A 429 never spends an attempt | Throttles hit `plan`, `implement` (twice) and `review`. Implement runs once, as Temporal attempt 1, in round 1. It does not run again until the Retry-After has passed. Check and review each run exactly once. The need closes. |
| `timeout_resumes_from_pushed_commit` | A timeout resumes from the pushed commit | `timeout@1` (cap) and `lost@1` (heartbeat). Each slice is retried as attempt 2, resumed from `<slice>-r1-s1`, and in the same round. The branch holds exactly `s1, s2, s3`, with no step redone and none lost. |
| `duplicate_start_is_refused` | A duplicate start is refused | A second start of `need-n1` gets `WorkflowStartError::AlreadyStarted` while the need runs and again after it closes. The plan runs once. |
| `red_check_returns_to_implement_on_same_branch` | A red check goes back to implement on the same branch | Round 2 gets the red text (and, for the other slice, the review text) as feedback. It runs on the same branch, resumed from round 1's head, and adds `r2-s1..s3` after `r1-s1..s3`. |
| `no_slice_runs_twice_at_once` | No slice runs twice at once | Slices take a lost host, a timeout, a red check and a 429. No slice ever has two units of work running at once. A client start of a running slice's workflow id is refused. |
| `lander_bisection_ejects_exactly_bad_units` | A Lander bisection ejects exactly the bad units | Six slices land together and two are `bad`. At least one batch is bisected. The only units ejected are the two bad ones, and `main` holds exactly the four good ones, each once. The bad slices loop with the gate's text, run out of rounds, and park the need with `WorkExhausted`. |
| `exhausted_attempts_park_with_cause` | Out of attempts → park, never restart | Two red checks park `n1` with `WorkExhausted { rounds: 2, .. }`. Three lost hosts park `n2` with `InfraExhausted { step: "implement" }`. The record's last call for each is a `Park`. The plan ran once per need. |

## Measured times

Measured on pepper (M5 Max, 18 cores) on 2026-11-18, with rustc 1.99.1.

| Command | Wall time |
|---|---:|
| Cold `cargo test --no-run`, empty target dir | 29.7 s |
| Cold `cargo build`, dependencies only | 24.9 s |
| Incremental `cargo test --no-run` after editing one source file | 1–6 s |
| `cargo test` (9 tests; 7 dev servers in parallel) | 8.6 s, three runs in a row: 8.6, 8.6, 11.4 (the 11.4 s run included a rebuild) |
| `cargo test --test phase2 -- --test-threads=1` | 30.8 s |

Use a 60 s timeout for `cargo test`. The longest single test is
`exhausted_attempts_park_with_cause`, at about 8 s: three heartbeat timeouts
of 1 s each, plus the retries.

## What the Rust SDK (temporalio-sdk 1.2.0) could and could not do

Checked against the crate source and its `examples/`, not only its README.

**It could do everything the spike needed:**
- workflows and activities defined with macros
- typed signals between workflows (`external_workflow(..).signal(..)`)
- child workflows with an id and an id reuse policy
- durable timers and `wait_condition`
- deterministic `join_all`
- retry policies
- heartbeats whose typed details the next attempt reads back
- typed `ApplicationFailure`s whose type name and `next_retry_delay` the workflow can read from `err.cause()`
- `WorkflowStartError::AlreadyStarted`
- a dev-server test harness that uses an existing CLI binary

**Gaps:**
- **There is no time-skipping test environment.** The `testing` module only starts a real dev server. Core has a `TestServerConfig` for the Java time-skipping test server, but the SDK wraps neither it nor the time-skipping calls. So the tests use real, short timeouts: a 1 s heartbeat and a 2.5 s cap. They cannot run the real 55-minute cap. Go (`testsuite.TestWorkflowEnvironment`), TypeScript and Python (`WorkflowEnvironment.createTimeSkipping` / `start_time_skipping`) and Java can all skip timers. The README's Phase 2 line "Temporal's test server skips time" is therefore not true for Rust today.
- **There is no in-process workflow test with mocked activities.** Go's test suite can mock activities without a server. Here, every workflow test needs a server. Spawning one is cheap, about 1 s, so this costs little.
- **The macros need extra direct dependencies.** `#[activities]` expands to `futures::…` and `#[workflow_methods]` expands to `temporalio_workflow::…`. Both crates must be direct dependencies, and the SDK README does not say so.
- **Activity markers are not `Clone`.** The generated activity markers are unit structs without `Clone`, so a generic "run this step" helper takes a factory closure (`|| Stand::implement`).
- **Parts of the SDK README are out of date.** Some of its snippets do not match the 1.2.0 API. For example, sync signal handlers take `&mut SyncWorkflowContext`. Trust `examples/` and the source.

**Not a Rust gap, but it shaped the design:** a Temporal retry policy has one attempt budget per activity. `next_retry_delay` changes the delay, but the attempt still counts toward `maximum_attempts`. So a 429 cannot have its own unlimited budget inside one retry policy. Provider waits are therefore handled in workflow code, and this is the same in every SDK.

## Findings for the real build

- **Heartbeat details lag the pushed head.** Core throttles heartbeats to `heartbeat_timeout * 0.8`. A host that dies between two flushes loses its last checkpoint, but that commit is already pushed. The real `implement` should resume from the remote branch head, and use the heartbeat details only as a hint. The stand-in sidesteps this, because a dark host always dies right after its first heartbeat, which is sent immediately.
- **Pushes need a fence.** Temporal runs activities at least once. A host declared lost, or one that overran its cap, may still be running when the retry starts. `World::push` refuses a push whose expected head is stale. The real agent should push the same way: plain `git push` without force, or `--force-with-lease=<expected>`.
- **The provider cooldown is per step, not per pool.** The README asks that "the provider cools down for the whole pool". That needs shared state, such as a cooldown workflow or the Lander-style singleton per provider. The spike does not build it.

## Not built here (out of the spike's scope)

- backpressure from the Lander queue
- the hot-file mutex
- `MainWatch`
- per-host task queues (one queue runs everything here)
- Lander continue-as-new, to bound history: the Lander's history grows without limit as written
- signal-with-start for the Lander
- Postgres persistence
- a real vivi record client
