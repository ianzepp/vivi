# Worker driver

Status: implemented for macOS workers, 2026-10-21 (`provider = "worker"`,
`src/pool/worker.rs`, `src/pool/worker-macos.sh`). The repo wins wherever
it disagrees with this document.

## Roles

- **Orchestrator** (the pump). It decides which job runs on which pool, and
  holds all state about jobs, projects and placement. It never assumes a
  worker will accept a job.
- **Worker.** A machine that offers capacity: memory for RAM disks, disk for
  quota-limited images, plain directories, a platform and its tools. It holds
  no queue and no job types. For each job it builds one bounded workspace,
  runs the job inside it, and tears it down. It is the final judge of whether
  it can do that right now.
- **Driver.** The script that does this on one kind of worker. It is embedded
  in `vivi` and sent as the body of every `bash -c` over SSH, like the `ssh`
  provider's `ssh-slot.sh`, so a worker never runs a stale copy and nothing
  is installed on it.

## Invariants

1. One job, one boundary. A worker pool is per-dispatch: a dispatch builds
   the slot, and collecting the result destroys it, boundary and all, in
   every outcome.
2. Stopping a slot means ending what uses its boundary; there is no hunting
   for processes elsewhere.
3. Every boundary starts cold and clean: no warm builds, no caches (operator
   ruling, 2026-10-21).
4. The driver deletes only what its ledger recorded and its marks name.
5. A request the worker cannot hold now fails as `no free slot: <reason>`;
   the pump counts the pool as full and places the job on another pool.

## How it fits Vivi

The driver answers the slot verbs of `ssh-slot.sh` (`list`, `machine`,
`probe`, `envgaps`, `create`, `start`, `stop`, `claim`, `unclaim`, `put`,
`seal`, `read`, `exec`, `destroy`), plus `offer`, the worker's capacity.
So `WorkerProvider` wraps an `SshProvider` that sends the driver instead of
`ssh-slot.sh`, and reuses its calls, claim protocol and listing parser. The
differences:

- `create <id> <request>` takes the pool's boundary request
  (`kind=<k> size_gib=<n>`, from `[pools.<name>.worker]`), checks it against
  the worker's budgets, builds the boundary at the slot path, and unpacks the
  slot into it. Worker slots also carry stand-ins for GNU `timeout`,
  `sha256sum` and `nproc`.
- `destroy` stops everything on the boundary and tears it down.
- The provider is `SlotLifetime::PerDispatch`, like RunPod:
  `open_for_dispatch` picks a free slot index, creates, waits for the seed
  boot to stop cleanly, marks the slot ready and claims it.
- The provider is bound to its mailspace, not to one pool, so it serves its
  pool and that pool's `-doctor` self-test pool alike. Callers tell slots
  apart by their pool mark.

`boot-slot.sh` and `job-run.sh` run unchanged inside the boundary.

## Worker settings

`<root>/worker.conf` (key=value) holds the worker's budgets. They limit how
much, never which jobs:

| Key | Meaning |
| --- | --- |
| `ram_budget_gib` | Total GiB of live RAM-disk boundaries |
| `ram_margin_gib` | Free memory a new RAM disk must leave (default 8) |
| `apfs_dir` | Directory for the per-job disk images (an external drive) |
| `apfs_budget_gib` | Total GiB of live disk-image boundaries |
| `dir_root` | Directory for `dir` boundaries |

The root itself (default `~/vivi-worker`) holds the lock, the ledger and the
mount points. It belongs on the internal disk, because macOS will not mount a
volume at a path inside another external volume.

## Boundaries on macOS

All three kinds work over SSH with nobody logged in at the console. That was
measured on a Mac Studio, macOS 27.

- **apfs.** A sparse APFS disk image of `size_gib` under `apfs_dir`, created
  by the login user (`hdiutil create -type SPARSE -fs APFS`), attached with
  `-nomount`, and mounted at the slot path with `diskutil mount -mountPoint`.
  Its size is a hard limit, and it takes disk only as it is written. Teardown:
  `hdiutil detach`, then delete the image. Writes measured about 1.5–2 GB/s on
  an NVMe drive.
- **ram.** `hdiutil attach -nomount ram://<sectors>`, `diskutil eraseVolume
  APFS`, remount at the slot path. Teardown: `diskutil eject`.
- **dir.** A plain directory under `dir_root`, linked from the slot path. No
  limit.
- **Running.** The boot wrapper's pid plus its start time (`ps -o lstart`),
  so a reused pid is not mistaken for it. An empty start time (a boot that
  ended before it was read) never counts as running.
- **Stopping.** Every process with a file or working directory on the
  boundary (`lsof +f --` for a mount, `lsof +D` for a directory), plus the
  boot wrapper's process group and every descendant, gets TERM, then KILL.

### What did not work, and why

- **Volumes added to an APFS container** (`diskutil apfs addVolume -quota`)
  mount writable only with `-mountOptions noowners`. With nobody logged in at
  the console, `noowners` combined with a custom mount point is refused, and
  without `noowners` the volume is owned by root. Turning ownership off needs
  root. Per-job disk images avoid this: the login user formats them, so their
  volume is the user's.
- **The system container** refuses custom-path and `noowners` mounts over
  SSH even with a console session.
- **External drives** need Remote Login's "Allow full disk access for remote
  users". Without it, every file and volume operation on them over SSH fails
  with "restricted by Sandbox (-69464)".
- **`ps -E` shows no process environments** on macOS 27, so the `/proc` tag
  that `ssh-slot.sh` uses on Linux has no macOS equivalent. Boundaries make
  it unnecessary.

## Later

- A `linux` driver: `dir` boundaries, `tmpfs` under `/dev/shm` for RAM, and a
  cgroup (or the existing `/proc` tag) for stopping.
- Placement by offers: the pump reads `offer` and chooses among workers,
  instead of trying pools in order.
- Whether the `local` provider should run the macOS driver directly and
  retire the Rust RAM-disk code.
