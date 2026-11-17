# vivi console: plan

**Status:** draft for discussion, 2026-11-14. Nothing here is built yet. The paths below
were read against `origin/vivi/main` at `bee46eb0` (vivi 24.0.0), the line hub runs.

## Goal

A web console for one `vivi server`. It shows what the system is doing and has done:
- pump state;
- slots and what runs in them;
- needs, tasks and their attempts;
- failures and why they happened;
- how often work comes back;
- the instructions each job was given;
- wall time, and CPU time once vivi records it;
- pools, roles and config;
- mail.

The console only reads. It never pauses, dispatches, retries, closes or edits anything.

## Decisions

1. **It lives inside `vivi server`.** It is a new crate, `vivi-console`. `vivi-server`
   mounts its routes under `/ui`. The server already owns the project registry, sign-in,
   and the paths to each project's mailspace and `pump.db`. A separate process would
   repeat all of that.
2. **Read-only at every level.** It follows the pattern already in
   `crates/vivi-server/src/server/pump_health.rs` and
   `crates/vivi-monitor/src/monitor/events.rs`:
   - SQLite opened with `SQLITE_OPEN_READ_ONLY`;
   - `PRAGMA query_only`;
   - one read transaction per page;
   - a short busy timeout, so it never blocks a writer;
   - it never migrates a store.
3. **HTML drawn on the server.** Templates are compiled into the binary (`maud` or
   `askama`). One small CSS file. Optional auto-refresh is a few lines of plain JS
   polling a fragment URL. No npm, no bundler, no client-side app.
4. **No sign-in** (operator, 2026-11-17: "This is a local project on a local network").
   The console shows every project the server hosts, to anyone who can reach the
   server's port; hub's firewall limits that to the LAN. It only reads. The first build
   had API-key sign-in with a cookie session; it was removed.
5. **No secrets on screen.** This is the same rule as `vivi-monitor`: no environment
   values, no process arguments, no key material. Config pages show key names and
   non-secret values only.
6. **One source of truth per fact.** The console reads vivi's own records. It never
   reads hub's `/vivi/orch` files. Facts that live only there move into vivi's records
   first (see "New records").

## What vivi already records

### Mailspace (`crates/vivi-storage/src/storage/schema.rs`)

- **`messages` plus item metadata:** needs, tasks, mail, threads and the brief text, as
  sent to the agent.
- **`mailspace_events`:** one row per event on an item. The pool writes two kinds
  (`crates/vivi-core/src/pool/record.rs`):
  - **`pool_dispatched`**, a JSON `DispatchRecord`:
    - pool, slot, machine, job, model;
    - `from` and the commit it started from (`start`), branch, budget, dispatch time;
    - need, kind, target, repository;
    - hardware, and pods given up on, with their cost.
  - **`pool_collected`**:
    - the outcome: `finished`, `timeout` or `lost`;
    - every field of the slot's `job/result`: `push`, `branch`, `sha`, `agent_rc`,
      `agent_s`, `job_s`, `check_rc`, `check_s`, `check_tail`, the publish and delete
      states, facts, and discarded or restored paths.

  Each event also gets a readable reply in the task's thread: the agent's final
  message, the stderr tail and the runner log tail.
- **`work_graph_*`:** graphs, nodes, edges, events and attempts. One attempt row per task
  per graph node.

### Pump store (`pump.db`; `crates/vivi-core/src/pump/store.rs`, `store/flow_index.rs`)

- **`units`:** brief, status, attempts, task, branch, model, pool, dispatch time, merge
  time, verdict.
- **`unit_events`:** one row per attempt end, with why, detail, model, branch, task and
  time. This is the "sent back, and why" history.
- **Run results:** `gates`, `gate_failures`, `exceptions`, `red`, `promotions`, `merges`.
- **Flows:** `flow_runs`, `flow_steps` (with verdicts), `step_jobs` (job → task and
  attempt), and `rung_runs` (start, finish and exit code for each rung).
- **Control and priority:** `pump_control`, `need_priority`.
- **Health:** `crates/vivi-core/src/pump/health.rs` gives the verdict that
  `/v1/projects/{id}/pump/health` already serves.

### Slots

Slots are declared in the project's `slot` config. Whether a slot is busy is worked out
from the events: a task whose latest `pool_dispatched` has no later `pool_collected` is
still in its slot (`fold_open`). The server cannot see a slot's files on a studio. It
must not need to: everything it shows has to come from records.

## Gaps

1. **No lookup by slot or job.** `mailspace_events` is indexed by message and account,
   not by event type, slot or job. The monitor's event query
   (`events.rs`, `WHERE e.event_type IN (...)`) scans the whole table. On faberlang
   (145k+ letters) that is the kind of query that pinned a server core on 11-12.
2. **Landing is invisible to vivi.** On hub, `orch/land-unit` lands branches. Landed,
   red, conflict and raced exist only in `orch/land.log`. The reasons exist only in
   `orch/land-red/`.
3. **Hub's dispatch may skip `pump.db`.** `orch/unit-dispatch` calls `vivi pool dispatch`
   directly. Those jobs get the pool events, but probably no `units` or `unit_events`
   rows. Not yet verified.
4. **No CPU time.** `job-run.sh` records wall seconds (`agent_s`, `check_s`, `job_s`)
   only.
5. **No disk-write figure per job.** Hub's landing log has GB per landing. Slots record
   none.

## New records (before or alongside the console)

### A. `slot_runs`: an index, not a new truth

A table in the mailspace store, one row per job:

```
slot_runs(
  job TEXT PRIMARY KEY, task TEXT, need TEXT, pool TEXT, slot TEXT, machine TEXT,
  model TEXT, kind TEXT, target TEXT, branch TEXT, start_sha TEXT, budget INTEGER,
  dispatched_at INTEGER, collected_at INTEGER, outcome TEXT,
  push TEXT, sha TEXT, agent_rc INTEGER, agent_s INTEGER, check_rc INTEGER,
  check_s INTEGER, job_s INTEGER, cpu_user_s INTEGER, cpu_sys_s INTEGER,
  landing TEXT, landing_at INTEGER, landing_reason TEXT
)
-- indexes: (slot, dispatched_at), (need, dispatched_at), (outcome, collected_at)
```

- It is written in the same transaction as `pool_dispatched` and `pool_collected`.
- The event notes stay the source of truth. A `vivi doctor` pass can rebuild
  `slot_runs` from them.
- Every console page about slots, jobs or failures reads this table, never the JSON
  notes.

### B. Landing as recorded outcomes

A `pool_landed` event on the task, with `landed`, `red`, `conflict` or `raced`, plus:
- the base commit and resulting tip;
- the seconds taken;
- the failing command and its tail (red), or the conflicting files (conflict).

It sets `slot_runs.landing*`. `orch/land-unit` calls a small `vivi` command to write it.
That command would be `vivi pool landed <task> ...`; the name is open.

### C. CPU time

`job-run.sh` captures child CPU time for the agent and the check stages, via `times`,
or `getrusage` in a small helper. It writes `agent_cpu_s` and `check_cpu_s` into
`job/result`. Collection carries them into the event note and `slot_runs`.

### D. `vivi slot` (CLI)

A read noun over the same table:
- `vivi slot list`: declared slots, with busy or free state.
- `vivi slot show <slot>`: the current job.
- `vivi slot history <slot> [--since]`: past jobs.

The slot-side runner stays `job-run.sh`. It is not a second recorder.

## Pages

Every page is scoped to one project, with a project picker at the top. The left menu is
the same on every page. Lists page by time, newest first, with a limit.

1. **Overview**
   - Pump: mode, last tick, health verdict.
   - Slots: busy, free and finished-but-uncollected, per pool.
   - The last 24 h: landed and attempts; failures by kind.
   - Per hour: landed against failed.
   - The newest failures, each linking to its job.
2. **Slots**
   - A tile per slot, grouped by host, for a quick look.
   - One row per declared slot: state, task, need, model, runtime against budget.
   - Click a slot for its history (from `slot_runs`).
3. **Jobs**
   - A filterable list: outcome, pool, slot, model, need, time range.
   - The job page shows one dispatch from start to finish:
     - the outcome badges (agent, check, landing);
     - the facts: pool, slot, machine, model, start commit, and wall and CPU time for
       the agent, the check, the whole job and the landing;
     - folding sections: the brief in full, the check output tail, the agent's final
       message, the stderr and runner log tails, the landing failure, the task's
       thread, and the raw dispatch, collect and landing records as JSON;
     - the events in time order.
4. **Needs**
   - A list showing where each need is: subject, flow run, the step it is at, slices
     built out of total, and age.
   - Filters: state, kind, step, and "stuck longer than".
   - A count per step on top shows where the pipeline is backing up.
   - Need page, attempts tab (work dispatched straight into pool slots):
     - done_when as a checklist;
     - the thread;
     - a timeline of every attempt: outcome, why, and a link to the job.
   - Need page, flow tab (work run through the pump's flow):
     - **path:** the steps this run has taken, with each step's answer word, and the
       step it is at now. A link draws the whole flow (`flows/build.mmd`) with this
       path lit up.
     - **slices:** one row per slice of the current build step: write scope, state
       (built, running, failed, blocked scope and who holds the file), tries, task
       rung result, wall time. Each slice links to its job.
     - **plan:** the latest revision, the admitter's verdict on each, and a diff
       between revisions.
     - **runs:** earlier runs and where each CTO restart sent the need.
   - Sources: `flow_runs`, `flow_steps`, `restart_entries`, `fanout_open`,
     `fanout_starts`, `slice_links`, `step_jobs`, `slice_scope`, `rung_runs`, `needs`,
     `plan_revisions`, `need_branches`.
5. **Failures**
   - By kind, with a window of 1 h, 24 h or 7 d: count, share and trend. Kinds: conflict,
     landing red, check red in the slot, timeout or lost, raced, setup.
   - Hot files: the paths that conflict most. This is the "one unit per hot crate"
     signal.
   - Red by failing command. A spike means a regression on main.
   - Needs that keep coming back: three or more attempts and still open, with their
     outcome mix.
   - Each kind links to the filtered jobs list.
6. **Pump**
   - Units by status.
   - `unit_events` stream.
   - Gates and gate failures.
   - Exceptions.
   - Rung runs with their times.
7. **Pools and roles**
   - Role → kind → pool → models.
   - Pool members and slot caps.
   - Cadences.
   - Read from config, with secrets left out.
8. **Mail**
   - Threads by identity and role, newest first, opened read-only.
9. **Work graphs**: graphs, nodes and attempts. Later; small.

### Drilling in

Each level is its own page with its own link: Overview counts lead to the Failures or
Jobs list; a row leads to the job page; the job page opens its sections in place. Filters
live in the URL, so any view can be bookmarked or pasted. Sections have anchors
(`#brief`, `#check`, `#raw`).

The deepest level is what vivi records: tails of the check output, stderr and runner log,
plus the full brief, the agent's message and the raw records. Full logs (`run.log`,
`boot.log`, the whole check output) stay on the slot and are out of scope for now.

## Sketches

Whiteboard sketches from 2026-11-14. The handles and numbers are samples; some come from
hub's state that morning.

### Overview

```
+------------------------------------------------------------------------------+
| vivi console                    project [faberlang v]   auto-refresh [x] 10s |
+------------+-----------------------------------------------------------------+
| >overview  | [pump      ] [slots     ] [landed 24h ] [failed 24h      ]      |
|  slots     | [running   ] [14 / 16   ] [116        ] [159             ]      |
|  jobs      | [tick 2s ok] [0 uncoll. ] [of 275 tries] [88 confl, 68 red]     |
|  needs     |                                                                 |
|  failures  | per hour, last 24h:  landed  | | || |  | ||| | | |               |
|  pump      |                      --------------------------------           |
|  pools     |                      failed  || ||| | || || ||| |              |
|  mail      |                                                                 |
|            | newest failures                                                 |
|            | 06:29  3dc2af9b a10  (check red)  radix-mir-clif tests  s-02    |
|            | 06:28  33566f4f a8   (conflict)   corpus/reference      s-01    |
|            | 06:25  66a7013f a10  (raced)      main moved            s-02    |
|            |                                   row -> job page               |
+------------+-----------------------------------------------------------------+
```

### Slots

```
 pool [studio v]  host [all v]
 studio-01: [r1][r2][r3][r4][r9 ][r10][r11][r12]      busy / free / uncollected
 studio-02: [r5][r6][r7][r8 ][r13][r14][r15][r16]

 slot      state         need / task                     model       runtime / budget
 radix-1   (busy)        a344e52c "clif: lower tuple..." gpt-6-luna  [####......] 18m / 55m
 radix-8   (free)        last: landed 06:12
 radix-3   (uncollected) 438c802e                        gpt-6-luna  [##########] done 2m ago

 history: radix-3
 06:49  438c802e a2  agent 0 / check 0  31m  (running)
 05:40  7cad638c a2  agent 0 / check 0  44m  (land red)
 04:52  9948782d a3  agent 0 / check 0  38m  (landed)
 04:01  e7d7f538 a5  agent 1            51m  (agent fail)
```

### Job page

```
 job 6877a85f-a9   need 6877a85f, attempt 9        (agent 0) (check 0) (landing red)

 pool/slot studio/radix-12   machine studio-01   model gpt-6-luna   from vivi/main@0f231b
 agent 29m wall, 41m cpu     check 12m wall, 3h cpu     job 44m / 55m     landing 12m

 v brief, full text                                                          [copy]
   Fix one done_when item: "clif-jit: struct return by pointer ..."
   Proof rule: cargo check and the crate's tests only ...
 v check output                                         tail ~4 KB  [find] [wrap]
   test result: ok. 412 passed; 0 failed
 > agent final message
 > stderr tail / runner log tail
 > landing: failing command and output
 > thread on the task
 v raw records                                       [dispatch | collect | landing]
   {"job":"...","slot":"radix-12","outcome":"finished","agent_rc":"0", ...}

 events: dispatched -> agent done -> check ok -> pushed -> collected -> landing red
```

### Needs list

```
 state [open v]  kind [build v]  at step [any v]  stuck > [2h]
 by step:  plan 4 | admit 2 | build 11 | audit 3 | judge 2 | merge 1 | cto 3 | parked 6

 need      subject                       run  at step    slices      age
 a1f3c2d0  clif: lower tuple returns     1    (build 1)  3 / 5 built 40m
 5c20e9b7  parser: trailing commas ...   2    (judge 2)  2 / 2       3h
 9e04a6f1  clif-jit: varargs call        1    (cto)      -           6h
 77b1e3aa  semantic: shadowed loop var   1    (parked)   -           2d
```

### Need page: attempts tab

```
 need 886c6935 "python register: lower bitwise ops"     (open) (5 attempts)

 done_when                         attempts
 [x] HIR lowers &, |, ^            o a1 (agent fail)   timeout at budget
 [x] register test passes          o a2 (conflict)     hir-python/expr.rs
 [ ] corpus case bits-02 on go     o a3 (landed)       items 1 and 2
 [ ] no new hygiene uses           o a4 (check red)    register_test
                                   o a5 (landing red)  py/swift gate
 thread: progress note 06:41;      each row -> its job page
 coalesce ruling 1dc1a652
```

### Need page: flow tab

```
 need a1f3c2d0 "clif: lower tuple returns"   flow build v3   branch vivi/need/a1f3c2d0

 run 1 (entry: research)
 [research]-READY->[plan]-planned->[admit 1]-revise->[plan 2]-planned->[admit 2]
   -admitted->[slice]-sliced->[[build 1]]--> :audit + need rung: --> :merge:
                                  now         not yet reached
 [show whole flow graph]  draws build.mmd with this path lit

 slices of build 1
 slice  write scope                   state            tries  task rung  wall
 s1     radix-mir-clif/lower/call.rs  (built)          1      pass 3m    22m
 s2     radix-mir-clif/abi.rs         (built)          2      pass 4m    51m
 s3     runtime/clif/tuple.rs         (built)          1      pass 2m    18m
 s4     corpus/reference/tuples/      (running)        1      -          9m
 s5     radix-hir-*/tuple.rs          (blocked scope)  1      -          wants abi.rs, held by s2

 plan: revision 2 of 2; admit 1 said "revise: split abi from call lowering" [diff r1->r2]
 other runs: none yet (a CTO restart starts a new run at research/plan/slice/rework)
```

### Failures

```
 window [1h | 24h | 7d]  pool [all v]

 kind               count  share                trend
 (conflict)         88     [###########.......]  flat
 (landing red)      68     [########..........]  falling
 (check red, slot)  21     [###...............]  flat
 (timeout / lost)    6     [#.................]
 (raced)             3     [..................]
 (setup)             1     target_source_mismatch          kind -> filtered jobs

 hot files (conflicts)        red: failing command
 radix-mir-clif/     26       faber --test integration   24
 corpus/reference/   18       radix-module lib tests     17
 runtime/clif/       11       scripta stage 2            12
 radix-hir-swift/     9       crate's own tests          15

 needs that keep coming back (3+ attempts, open)
 66a7013f  clif-jit: varargs call  10  (red)(conflict)(raced)
 3dc2af9b  clif: tagged unions     10  (red) x6
```

## Query rules

- Every page query uses an index. A query plan check in tests: each console query runs
  `EXPLAIN QUERY PLAN` against a seeded store and fails on `SCAN` of `mailspace_events`,
  `messages` or `slot_runs`.
- Each page has a row limit and a time window. No unbounded list.
- Auto-refresh is opt-in per page, at 10 s or slower, and fetches only the changing
  fragment.
- Store reads run on the blocking pool (`spawn_blocking`), never on the async runtime
  threads.

## Phases

1. **Records.**
   - `slot_runs` with its indexes, written at dispatch and collect.
   - A doctor pass that rebuilds it from the events.
   - `vivi slot list|show|history`.

   Tests: the table matches the events; the rebuild is idempotent.
2. **Console skeleton.**
   - The `vivi-console` crate, mounted at `/ui`.
   - Login and cookie session.
   - The Overview, Slots and Jobs pages.
   - The query plan test.
3. **Landing and CPU.**
   - The `pool_landed` event and its CLI.
   - `orch/land-unit` switched to call it.
   - CPU fields in `job-run.sh`.
4. **The rest of the pages.** Needs (attempts and flow tabs), Failures, Pump, Pools and
   roles, Mail.
5. **Later, if wanted.** Work graphs; a JSON twin of each page for scripts.

## Build slices

Six implementation seats in two waves. Seats in one wave touch different files; the
parent reviews, checks and commits each slice before the next wave starts.

### Code facts the slices rest on

- Every pool event goes through `Mailspace::record_pool_note`
  (`crates/vivi-core/src/mailspace/pool_record.rs`). It writes the event and its reply
  in one storage transaction: `note_message_with_reply` in
  `crates/vivi-storage/src/storage/mutate.rs`. `slot_runs` is updated inside that same
  transaction, so the index can never drift from the events.
- A new mailspace table needs a `STORAGE_SCHEMA_VERSION` bump, because `ensure_schema`
  skips its DDL when the version matches. A store opened by an older binary is refused,
  so a hub install with a bump needs `vivi-server` and `vivi-mind` restarted.
- `vivi-server` owns `AppState`, so `vivi-console` cannot depend on it. The console
  takes its own `ConsoleState`: a project lookup (id → mailspace root) and a key check,
  both supplied by the server. The server mounts the console router at `/ui`.
- Read-only store access follows `crates/vivi-monitor/src/monitor/events.rs`, and
  pump health comes from `vivi_core::pump::health`.
- No new dependency for HTML: a small escaping writer in the console crate, and CSS
  served from one `include_str!` file.
- The repo's hygiene test caps files at 1000 lines and functions at 60, and keeps unit
  tests in companion `_test.rs` files.

### Wave 1 (two seats, in parallel)

- **S1 records.**
  - The `slot_runs` table, its indexes and the schema bump.
  - The upsert from `pool_dispatched` and `pool_collected` inside the event transaction.
  - An idempotent rebuild from the events.
  - `vivi slot list|show|history|rebuild`.
  - Files: `vivi-storage` (schema, a new `slot_runs` module, `mutate.rs`), a
    `vivi-core` mailspace read API, and root `src/` CLI wiring.
- **S2 console skeleton.**
  - The `vivi-console` crate.
  - `ConsoleState` and the router.
  - Login and logout, with an in-memory cookie session.
  - The layout: menu and project picker.
  - CSS and the HTML escaping writer.
  - A read-only SQLite open helper.
  - The query plan test harness.
  - The Overview and Slots pages.
  - The `/ui` mount in `vivi-server`.
  - Files: `crates/vivi-console/**`, plus a small adapter and one router line in
    `vivi-server`.
  - Tests build `slot_runs` from a copy of the agreed DDL until S1 lands.

### Wave 2 (four seats, after wave 1 is committed)

- **S3 jobs:** the jobs list and the job page. `vivi-console` modules `pages/jobs*`.
- **S4 needs:** the needs list, the attempts tab and the flow tab. `pages/needs*`,
  reading `pump.db` read-only.
- **S5 the rest:** Failures, Pump, Pools and roles, Mail. `pages/{failures,pump,pools,mail}*`.
- **S6 landing and CPU:**
  - the `pool_landed` event and its CLI;
  - CPU fields in `job-run.sh` and collection;
  - the `slot_runs` upsert extended to both.

  Files: `vivi-core` pool modules, `job-run.sh`, and the `vivi-storage` `slot_runs`
  module.

Each seat runs `cargo fmt --check`, the hygiene test and its own crate's tests. The
parent runs `cargo test --workspace` (about 4 minutes) once per wave.

## Open questions

1. Landing command name and shape: `vivi pool landed`, or a field on an existing
   command?
2. Should `slot_runs` also take hand jobs dispatched by the pump's own flow (through
   `step_jobs`)? Probably yes, because all dispatches go through the pool. To verify.
3. Should a per-job disk-write figure be recorded? It needs the host to report a SMART
   or file delta per job, which is noisy with several jobs per drive.
4. Does the console listen on the server's port, or a separate local port? The default
   is the server's port under `/ui`, LAN-only like the rest.
5. Is faberlang's flow off on hub today? The register says the pump was resumed with
   the flow off on 11-12, and hub's scripts dispatch straight into pool slots. If so,
   faberlang's flow tab stays empty and its needs show only the attempts tab until work
   runs through the flow again. Not checked against the live config.

## Follow-ups to file as needs

Found during wave 1 (2026-11-14). To be filed on hub's vivi project once this laptop's
key is granted there; until then they live here.

1. **A failing pool test prints the process environment.** A `vivi-core` pool test,
   when it fails, prints the whole environment, API keys included, into test output.
   Done when: no test prints environment values on failure.
2. **Seven `vivi-core` pool tests fail on macOS at `vivi/main` `bee46eb0`.** They are
   `explicit_provider` (claims by declared name, concurrent claims, host budget, exact
   dir reuse) and `worker::cargo_tests` (persistent cargo off, paths, sccache). The
   first trips on the `/var` → `/private/var` temp link. Done when:
   `cargo test -p vivi-core --lib pool::` passes on a Mac.
3. **Check that hub's dispatches carry the need.** `DispatchRecord.need` is set only
   when the dispatcher has a source; hub's `unit-dispatch` calls `vivi pool dispatch`
   directly. Done when: `slot_runs.need` is filled for hub's unit jobs, or the console
   says why it is empty.
4. **Prove `slot_runs` sees every pool event.** Only `record_pool_note` updates it.
   Done when: every path that can create a `pool_*` event (sync and letter ingestion
   included) updates the index, or is shown not to exist.
5. **Pump health writes.** `vivi_core::pump::health` goes through
   `Mailspace::discover`, which can mint an id or write `.gitignore`. Both the
   `/v1/projects/{id}/pump/health` route and the console use it. Done when: health
   opens nothing for writing.
6. **Console speed on a real store.** Done when: the overview, slots and jobs pages are
   timed against a copy of faberlang's store and each answers in under 200 ms.
7. *(Dropped 2026-11-17: the console has no sign-in.)*
8. **Hub landing writes `pool_landed`.** Done when: hub's `orch/land-unit` records each
   outcome with `vivi pool landed`, after wave 2's S6 ships.
9. **Flow steps have no times.** `flow_steps` records no start or end, so the needs
   list estimates "stuck" from job dispatch times. Done when: steps carry `started_at`
   (and `ended_at`) and the console uses them.
10. **`vivi slot list` skips count-only pools.** It lists `[[slot]]` declarations only,
    while the console also counts `[pools.*] slots`. Done when: both show the same slots.

## Not in scope

- Any write action: pause, dispatch, retry, close, unhold, config edits.
- Reading hub's `/vivi/orch` files, or any host-specific layout.
- Live process inspection on worker hosts.
- A store for full job logs (`run.log`, `boot.log`, the whole check output). The console
  shows the recorded tails only (operator, 2026-11-14).
