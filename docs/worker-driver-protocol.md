# Worker driver protocol

Status: design, 2026-10-21. Nothing here is implemented yet except where it
says so. The repo wins wherever it disagrees with this document.

## Roles

- **Orchestrator** (the pump, normally on the hub). It decides which job runs,
  on which worker, inside what kind of boundary. It holds all state about
  jobs, projects and placement. It never assumes a worker will accept a job.
- **Worker.** A machine that offers capabilities and capacity: memory for RAM
  disks, disk for quota volumes, Docker, a GPU, a platform and its tools. It
  holds no job queue and has no job types. On request it builds one bounded
  workspace for one job, runs the job inside it, and tears it down. It is the
  final judge of whether it can do that right now.
- **Driver.** The script that carries out the orchestrator's requests on one
  kind of worker. There is one driver per environment (`macos`, `linux`,
  `docker`, `fly`). The driver is not installed: like `ssh-slot.sh` today, it
  is embedded in `vivi` and sent as the body of every call (`bash -c` over
  SSH, or plain `bash` for the local machine), so a worker can never run a
  stale copy. The orchestrator picks the driver from the worker's provider
  and its probed system.

## Invariants

1. One job, one boundary. A boundary is made for a job and torn down after it
   in every outcome: success, failure, timeout, cancel.
2. Stopping a job means tearing down its boundary. There is no hunting for
   processes outside it.
3. Every boundary starts cold and clean: a fresh checkout of the exact commit.
   There are no warm builds and no build caches (operator ruling, 2026-10-21).
4. A driver deletes only what it can prove it made: a `vivi-*` name, its
   ledger entry, and the device or path recorded at creation.
5. Every verb answers in the same format on every driver, so the orchestrator
   has one code path for all workers.

## Request and offer

A job's **request** names one boundary:

```text
kind=ram | apfs | dir | docker   # the boundary kind
size_gib=32                      # memory (ram) or quota (apfs, dir) or limit (docker)
platform=macos-arm64             # optional; must match the worker
needs=metal git cargo            # optional capabilities and tools
```

A worker's **offer** (the `offer` verb) reports what it has now:

```text
system=Darwin arm64
kinds=ram apfs dir
ram_budget_gib=64 ram_reserved_gib=32 mem_free_gib=40
apfs_container=disk5 apfs_budget_gib=500 apfs_reserved_gib=2 apfs_free_gib=780
docker=0
gpu=metal
tools=git cargo node
```

`*_budget_gib` is what the operator allows the driver to use on that machine.
`*_reserved_gib` is what live boundaries already hold. Budgets are the only
worker-side settings: they limit how much, never which jobs.

## Verbs

| Verb | Arguments | Does | Answers |
| --- | --- | --- | --- |
| `offer` | none | Reports capabilities and capacity | the offer lines |
| `admit` | job id, request | Under the worker lock: checks the request against budget minus reserved and live free memory or disk, records a reservation, builds the boundary | `result=admitted` with `path=`, or `result=undeliverable` with `reason=`, or `result=failed` with `reason=` |
| `put` | job id, file | Writes standard input into the boundary | nothing, or an error |
| `start` | job id | Runs the job's boot script detached, inside the boundary | nothing, or an error |
| `status` | job id | Reports the job | `state=running`, or `state=exited exit=<n>`, or `state=gone` |
| `read` | job id, files | Returns result files as a tar stream | the stream |
| `release` | job id | Stops everything using the boundary, tears it down, drops the reservation | `result=released`, or `result=gone` |
| `sweep` | none | Tears down boundaries and reservations whose job no longer runs (after a crash or reboot) | one line per boundary removed |

`undeliverable` means "not now, not here": the orchestrator re-queues the job
or places it on another worker. `failed` means the worker could not do what it
claimed it could; the orchestrator reports it and stops placing there.

## Worker-side ledger and lock

- **Lock.** `admit`, `release` and `sweep` hold one lock per worker for their
  whole check-reserve-build step: `mkdir <root>/.lock` (atomic everywhere,
  including macOS, which has no `flock` command), removed on exit.
- **Ledger.** One file per job under `<root>/ledger/<job>`: kind, size, the
  device or path, and the creation time. Reserved capacity is the sum of the
  ledger. It is also what proves a boundary is ours.

## Boundaries on macOS (`macos` driver)

These are the steps the `local` provider already runs from Rust
(`src/pool/ramdisk.rs`, `src/pool/apfs.rs`). The driver runs them as shell.

- **ram.** `hdiutil attach -nomount ram://<sectors>` (512-byte sectors), then
  `diskutil eraseVolume APFS vivi-<job> <device>`, then (after
  `diskutil unmount` of its default mount) `diskutil mount -mountPoint <path>
  <volume>`. Teardown: `diskutil eject <device>`, which frees the memory.
- **apfs.** `diskutil apfs addVolume <container> APFS vivi-<job> -quota <n>g
  -nomount`, then `diskutil mount -mountOptions noowners -mountPoint <path>
  <device>`. Creating and mounting in one step needs root; creating unmounted
  and then mounting does not. Use a quota, never a reserve. Teardown:
  `diskutil apfs deleteVolume <device>`, only after the live listing proves
  the device still holds that `vivi-*` volume.
- **dir.** A plain directory with no hard limit, for jobs that need none.
- **Stopping.** Every process with a file or working directory on the mount
  (`lsof +f -- <mount>`) gets TERM, then KILL. The volume is then torn down.
  A process cannot keep running in a volume that no longer exists.

## Other drivers (later)

- **linux.** `dir` boundaries; `tmpfs` under `/dev/shm` for RAM; a cgroup
  (or the existing `/proc` tag) for stopping.
- **docker.** The container is the boundary; `--memory` and a tmpfs give the
  size.
- **fly.** The machine is the boundary; `offer` and `admit` reduce to the Fly
  API.

## Open questions

- How the orchestrator learns offers: polling `offer` on a timer, or only at
  placement time.
- Whether the `local` provider switches to running the `macos` driver
  directly, which would retire the Rust boundary code.
- What admission subtracts from live free memory as a safety margin (the
  `ramdisk` admission has a compiler margin today).
