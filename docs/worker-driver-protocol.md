# Worker driver protocol

Status: design, 2026-10-21. The `macos` driver (`src/pool/worker-macos.sh`)
exists and passed live tests on a Mac Studio over SSH, but no provider calls
it yet. The repo wins wherever it disagrees with this document.

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
apfs_container=disk7 apfs_budget_gib=500 apfs_reserved_gib=2 apfs_free_gib=480
docker=0
gpu=metal
tools=git cargo node
```

`*_budget_gib` is what the operator allows the driver to use on that machine,
set in `<root>/worker.conf` (`ram_budget_gib`, `ram_margin_gib`, `apfs_image`
or `apfs_container`, `apfs_budget_gib`).
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
| `sweep` | the live job ids | Tears down every boundary whose job the orchestrator did not name: the orchestrator owns job state, so the worker never guesses which are dead | one line per boundary removed |

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
  `diskutil apfs deleteVolume <device>`, only while the device still holds
  that `vivi-*` volume.
- **The apfs container is a sparse disk image.** Measured on the Studio
  (macOS 27, over SSH): in the system container (`disk3`) a new volume will
  not mount at a custom path, and refuses `noowners`; its default mount is
  owned by root and not writable. In the container of a sparse image the
  login user attached (`hdiutil create -type SPARSE -fs APFS`, then
  `hdiutil attach -nomount`), both work. The driver creates the image at the
  apfs budget when it is missing and attaches it on demand (attaching twice is
  harmless). The image's size is then the hard ceiling of all apfs
  boundaries. It grows as volumes are written, reuses freed space, and does
  not shrink while attached.
- **An external drive can hold the volumes directly.** Measured on the Studio
  with a 2 TB NVMe drive formatted APFS as `Lanes`: once Remote Login has
  "Allow full disk access for remote users", the login user can create quota
  volumes in the drive's own container and mount them with `noowners`. Name
  it by path (`apfs_volume=/Volumes/Lanes`), because device names change
  across reboots. Without that grant, every file and volume operation on an
  external drive over SSH fails with "restricted by Sandbox".
- **Mount points stay on the internal disk.** macOS will not mount a volume
  at a path inside another external volume (the same mount works under the
  home folder or `/tmp`). So the driver root, which holds the ledger, run
  state and mount points, lives on the internal disk (`~/vivi-worker`). Only
  the data lives on the drive: apfs volumes in its container, and `dir`
  boundaries under `dir_root`.
- **dir.** A plain directory with no hard limit, for jobs that need none.
- **Running.** The boot wrapper's pid plus its start time (`ps -o lstart`), so
  a reused pid is not mistaken for it. A boot that ends before its start time
  is read leaves it empty, and an empty time never counts as running; a
  written exit code always means the job ended.
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
