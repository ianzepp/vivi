# Vivi on Temporal

The runtime from
[`docs/orchestration-reset/README.md`](../docs/orchestration-reset/README.md)
("The pipeline under B"): `NeedWorkflow`, `SliceWorkflow` and
`LanderWorkflow`, one activity interface (`src/defs.rs`) with two
implementations, and one worker binary that serves per-host task queues.

- **Real activities** (`src/real/`) wrap existing commands: Vivi's pool
  (`vivi task send`, `vivi pool dispatch|wait`, `vivi task show`), git, and
  `lab/lander/compose` and `gate`. Vivi stays the system of record.
- **Stand-in activities** (`src/activities.rs`) replay a scripted fault
  model. The workflow tests use them; they started as the Phase 2 spike.

It is its own Cargo workspace, like `sim/`. A `cargo build` or `cargo test`
at the vivi root never compiles it.

## Run the tests

```sh
cd temporal
cargo test
```

The only requirement is the `temporal` CLI on `PATH`. Set `TEMPORAL_CLI` to
use a different binary. Each workflow test starts its own in-memory dev
server on a free port, through the SDK's `WorkflowEnvironment::start_local`.
Nothing touches a remote host. One unit test makes a throwaway bare
repository under the system temp directory to prove the push fence.

## Run a deployment

One binary, `vivi-temporal`, and one config file shared by every host
(`deploy/sandbox.toml` is the end-to-end one).

```sh
vivi-temporal worker --host studio --config sandbox.toml   # on each host
vivi-temporal lander --config sandbox.toml --target pump-sandbox
vivi-temporal need --config sandbox.toml --target pump-sandbox \
    --handle e2e1 --brief-file brief.md --slice 's1:src/lib.rs'
```

A worker polls only the queues its `[hosts.<name>].queues` lists, each with
its own limit on concurrent activities (a fixed-size slot supplier). The
control queue also runs the three workflows. `lander` starts the target's
Lander (`lander-<target>`) unless it already runs. `need` starts
`need-<handle>` and waits for the outcome.

### Queue map

| Queue | Runs | Host (e2e) | Intended host |
|---|---|---|---|
| `control` | the workflows, `plan`, `record` | pepper | mini-m1 |
| `lane` | `compose`, `publish` (the lane's bare git cache) | pepper | hub |
| `agent-<host>` | `implement`, `review` (the host's pool and warm repo) | studio | studios |
| `check-<host>` | `check` (the host's check tree) | studio | studios |
| `gate-<host>` | `gate` (the host's warm gate tree) | studio | gate hosts without a full set of agent slots |

Queue names are templates in `[routing]` (`agent-{host}`); a template
without `{host}` names one shared queue, which is how the tests run every
step on one queue. Steps with affinity get an ordered list of placements
(`src/routing.rs`): the host that holds the slice's work first (from the
`Checkpoint`), then the other hosts in a per-slice stable order. The first
placement has a short schedule-to-start (`affinity_wait_s`), the rest a
longer one (`fallback_wait_s`). A host that does not take the task in time,
or that spends every infra attempt, hands the step to the next host
(`steps::run_placed`). Implement then resumes from the lane's branch head.

### Activities

| Activity | Wraps | Failure classes |
|---|---|---|
| `plan` | nothing yet: returns the need's given slices, or one slice for the whole brief | none |
| `implement` | `git ls-remote` (the branch head on the lane is the resume point), `git fetch`, `vivi task send`, `vivi pool dispatch --branch --from --model --budget`, `vivi pool wait` (heartbeating), `vivi task show --latest` (collected record and receipt), then `git push --force-with-lease=<branch>:<expected>` | 429/quota/rate-limit/5xx text in the receipt → provider (with Retry-After when named); a refused reply or no change at all → format; a fenced push, ssh/slot/pool/git failure, or an agent failure with no signature → infra |
| `check` | `git fetch`, `checkout --detach <head>`, `merge --no-ff <main>` in the check tree, then `sh -c <check_command>` with `VIVI_BASE`/`VIVI_HEAD` | a conflict with main or a non-zero exit → red (a value); killed at the limit, exit 126/127, or transport text → infra |
| `review` | the same pool path, `--read-only --from <head>`; the receipt must end in `VERDICT: APPROVE` or `VERDICT: CHANGES: …` | as implement; no verdict line → format |
| `compose` | refreshes the bare cache from the target's remote, then `compose --no-fetch --no-push --base <main>` with `LANDER_CACHE`; pushes the candidate to `refs/vivi/candidates/<id>` | exit 2 or bad JSON → infra; per unit: conflict or missing → ejected, already-merged or no-op → merged |
| `gate` | `gate --sha <candidate> --base <main> --tree <tree> --timeout` | `pass` → pass or fail (bisected); exit 3, exit 4, `baseline_*` (main itself broken) → infra, never a red that ejects good units |
| `publish` | `merge-base --is-ancestor`, then `git push --force-with-lease=<main>:<base> <candidate>:<main>` (a fast-forward) | a refused lease → `Stale` (re-composed, at most 3 times); other git failures → infra |
| `record` | `vivi mcp call` (`reply` for a status, `mark parked parked-<class>` plus a `reply` with the typed cause for a park, `done` for a close), or the log, or memory | an MCP failure is retried without limit (the record must not be skipped) |

Compose and gate are called with explicit paths from config, and compose
always with `--no-fetch --no-push`: its built-in remote is radix's lane, so
the runtime does the fetch and the candidate push against the target's
own remote.

### Scheduling rules (`src/admission.rs`)

The Lander keeps both rules as plain state and runs an admission loop
beside its landing loop. A slice signals `admit` before its first round and
waits for `admitted`; it signals `release` when it lands or parks.

- **Backpressure.** No new slice is admitted while (queued + landing units)
  × `land_s_per_unit` exceeds `max_backlog_s` (default 3600 s at 180 s per
  unit, so about 20 units). `max_backlog_s = 0` turns it off.
- **Hot-file mutex.** A slice whose write scope overlaps a configured hot
  path (whole path components: `crates/x` covers `crates/x/src/a.rs`, not
  `crates/xy`) holds that path from admission until it ends. Requests are
  first come, first served, but one blocked by the mutex does not hold up
  a later one that needs other paths.

### Failure classes

| Class | Mechanism |
|---|---|
| Provider (429, 5xx, quota) | The activity fails with an `ApplicationFailure` of type `provider`. It is marked non-retryable and carries `next_retry_delay` = Retry-After. The workflow reads both from the failure, sleeps on a durable timer, and runs the step again. That spends no Temporal attempt and no work round. After `max_provider_waits` the slice parks with `ProviderExhausted`. |
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
the park path and one for the hot-file mutex. The workflow tests run the
stand-ins through the same activity definitions the real activities
implement. Unit tests sit beside their modules (`*_test.rs`):

| File | Covers |
|---|---|
| `classify_test.rs` | 429, quota, rate-limit and 5xx text → provider, with Retry-After in s, ms and min, capped; `429` inside a sha is not provider; refused replies → format; check exit codes (red vs infra) |
| `routing_test.rs` | queue templates; preferred host first; every host once; stable per-slice order that spreads slices; affinity then fallback waits |
| `admission_test.rs` | backpressure threshold and off switch; path overlap by components; the mutex holds and frees; a granted slice re-asking is granted; duplicate requests queue once |
| `config_test.rs` | `deploy/sandbox.toml` parses; seconds convert; unknown hosts, 0 slots, a missing control queue, unknown keys and an incomplete MCP record are refused |
| `real/pool_test.rs` | the `pool_collected` record and receipt from real `vivi task show` output; a throttled agent fails as provider |
| `real/agent_test.rs` | review verdict parsing; the implement brief carries feedback and the proof command |
| `real/land_test.rs` | compose JSON → candidate, ejections, already-merged; gate JSON → pass, red, and broken base as an error |
| `real/git_test.rs` | `ls-remote` parsing; the push fence end to end against a bare repository |
| `record_test.rs` | the MCP calls for status, park and close |

| Test | Criterion | What it asserts |
|---|---|---|
| `provider_429_never_spends_an_attempt` | A 429 never spends an attempt | Throttles hit `plan`, `implement` (twice) and `review`. Implement runs once, as Temporal attempt 1, in round 1. It does not run again until the Retry-After has passed. Check and review each run exactly once. The need closes. |
| `timeout_resumes_from_pushed_commit` | A timeout resumes from the pushed commit | `timeout@1` (cap) and `lost@1` (heartbeat). Each slice is retried as attempt 2, resumed from `<slice>-r1-s1`, and in the same round. The branch holds exactly `s1, s2, s3`, with no step redone and none lost. |
| `duplicate_start_is_refused` | A duplicate start is refused | A second start of `need-n1` gets `WorkflowStartError::AlreadyStarted` while the need runs and again after it closes. The plan runs once. |
| `red_check_returns_to_implement_on_same_branch` | A red check goes back to implement on the same branch | Round 2 gets the red text (and, for the other slice, the review text) as feedback. It runs on the same branch, resumed from round 1's head, and adds `r2-s1..s3` after `r1-s1..s3`. |
| `no_slice_runs_twice_at_once` | No slice runs twice at once | Slices take a lost host, a timeout, a red check and a 429. No slice ever has two units of work running at once. A client start of a running slice's workflow id is refused. |
| `lander_bisection_ejects_exactly_bad_units` | A Lander bisection ejects exactly the bad units | Six slices land together and two are `bad`. At least one batch is bisected. The only units ejected are the two bad ones, and `main` holds exactly the four good ones, each once. The bad slices loop with the gate's text, run out of rounds, and park the need with `WorkExhausted`. |
| `hot_file_mutex_serializes_overlapping_slices` | Hot-file mutex | Two slices under the hot path `crates/hot`: the second starts implementing only after the first has merged. A third slice elsewhere starts at once. |
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
| `cargo test` after the runtime build (45 unit + 8 workflow tests) | 9.5 s and 9.0 s, two runs in a row |
| `cargo build --release --bin vivi-temporal`, warm deps | 48 s |

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

All three are handled now: implement resumes from the lane's branch head
(`git ls-remote`), every push is fenced with `--force-with-lease`, and the
per-pool provider cooldown is still open (listed below).

- **Heartbeat details lag the pushed head.** Core throttles heartbeats to `heartbeat_timeout * 0.8`. A host that dies between two flushes loses its last checkpoint, but that commit is already pushed. The real `implement` should resume from the remote branch head, and use the heartbeat details only as a hint. The stand-in sidesteps this, because a dark host always dies right after its first heartbeat, which is sent immediately.
- **Pushes need a fence.** Temporal runs activities at least once. A host declared lost, or one that overran its cap, may still be running when the retry starts. `World::push` refuses a push whose expected head is stale. The real agent should push the same way: plain `git push` without force, or `--force-with-lease=<expected>`.
- **The provider cooldown is per step, not per pool.** The README asks that "the provider cools down for the whole pool". That needs shared state, such as a cooldown workflow or the Lander-style singleton per provider. The spike does not build it.

## End-to-end run (2026-11-18)

Against the live server (`192.168.1.237:7233`, namespace `default`), with
the control and lane queues on pepper and agent, check and gate on studio,
on the sandbox lane `faberlang/pump-sandbox`, branch `temporal/main` (a
tiny Rust crate; `vivi/main` and `main` untouched). Workflows
`need-e2e1`, `slice-e2e1-s1` and `lander-pump-sandbox` are in the UI.

| Step | Queue | Took | Result |
|---|---|---:|---|
| implement (`pi`, `opencode-go/deepseek-v4.1-flash`, through the studio pool) | agent-studio | 62 s | pushed `temporal/e2e1/s1` @ 7589962 |
| check (`cargo test --quiet` on head + main) | check-studio | <1 s | green |
| review (read-only pool job) | agent-studio | 61 s | `VERDICT: APPROVE` |
| compose | lane | 2 s | candidate de8acc8, pushed to `refs/vivi/candidates/pump-sandbox-de8acc806adf` |
| gate (`lab/lander/gate --base`) | gate-studio | 1 s | base 1 test, candidate 2 tests, pass |
| publish | lane | <1 s | `temporal/main` f1f01d1 → de8acc8, leased on f1f01d1 |

The need closed with `Closed { landed: ["s1"] }` about 2 min 10 s after it
started. The record ran as `kind = "log"`. The Lander was terminated
afterwards.

## Not built yet

- an LLM `plan` step: `plan` passes given slices through
- WIP pushes during one agent job: the pool commits once, at the end of the
  job, so a lost host loses the work of that one job (everything before it
  is on the lane). Heartbeats during the job carry the last pushed head
- a cancelled `implement` stops waiting, but the pool job runs on to its
  budget and holds its slot until then
- a lease leak: a slice workflow that is terminated (not parked) never
  sends `release`, so its hot paths stay held until the Lander restarts
- `MainWatch`
- the per-pool provider cooldown (above)
- Lander continue-as-new, to bound history: the Lander's history grows
  without limit as written
- signal-with-start for the Lander: a slice started with no Lander parks
  with `InfraExhausted { step: "admit" }`
- deleting slice branches and candidate refs after landing
