# Worker driver

Status: implemented for macOS workers, 2026-10-21 (`provider = "worker"`,
`crates/vivi-core/src/pool/worker.rs`, `crates/vivi-core/src/pool/worker-macos.sh`). The repo wins wherever
it disagrees with this document.

## Persistent repository slots (preferred for builds)

For warm builds, declare repository-bound slots in the owning mailspace's
`.vivi/mailspace.toml`. The directory is the working tree itself, used exactly
as written; Vivi does not append `repo/` or derive an owner/repo layout.

```toml
[pools.studio]
provider = "ssh"
# Add the usual harness/committer settings; no counted slots or ssh.host needed.

[[slot]]
name = "radix-1"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-1"
pools = ["studio"]
```

Names are unique within the mailspace. `repository` is exactly `owner/repo`,
matched against the last two components of the target's Git URL (without
`.git`), not its alias. `pools` is optional: absent/empty allows all SSH pools
in this mailspace; otherwise it lists eligible role-routing pools. A job
claims only a free slot of its repository, by name. A pool with no routed
explicit declarations retains its counted, derived slots unchanged.

The first job creates `dir` if absent. Later jobs fetch, `checkout -f` and
`clean -fd` (never `-x`) there. Unchanged source mtimes and ignored build output
survive. Claims, boot pid, logs, delivered files, source/worktree records and
preparation markers live in `dir/.vivi/`; `/.vivi/` is added only to Git's
`info/exclude`, never a tracked ignore file. Vivi never deletes `dir`, even
when the slot is destroyed. Use physical absolute paths, without symlinks or
`.`/`..` components. Explicit slots use the persistent `ssh` provider, not
per-job workspace disks. Multi-repository `--also-target` jobs need a generic
pool; explicit slots never put extra repository caches in their records.

On every host, set its operator budget in `~/vivi-worker/worker.conf`:

```ini
max_busy=8
```

Unset means unlimited. This caps running/reserved jobs across mailspaces and
pools, not slot declarations: ten declared slots can share eight busy places.
Host-side `flock` (via Perl, on Linux and macOS) serializes claims. Small tokens
under `~/vivi-worker/.busy/` reserve delivery and execution; normal completion,
stop and undo release them. Dead holders are reclaimed at the next claim;
an abandoned delivery expires after 120 seconds. All login accounts dispatching
on one physical host must use the same writable `VIVI_WORKER_ROOT` (default
`~/vivi-worker`) and worker.conf to share its budget.

**Per-job APFS images are unsuitable for builds:** destroying the image after
each job discards the build. Their bounded-workspace documentation remains
available for disposable work; use persistent repository slots for warm builds.

## Example persistent fleet

The faberlang mailspace owns all ten declarations below. A separate vivi
mailspace declares only its own repository slots. Paths here are operator
choices, not Vivi conventions. studio-01 sets `max_busy=8`, so at most eight
of the ten slots execute at once.

```toml
[pools.studio]
provider = "ssh"
# usual harness/committer configuration omitted

[[slot]]
name = "radix-1"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-1"
pools = ["studio"]

[[slot]]
name = "radix-2"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-2"
pools = ["studio"]

[[slot]]
name = "radix-3"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-3"
pools = ["studio"]

[[slot]]
name = "radix-4"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-4"
pools = ["studio"]

[[slot]]
name = "radix-5"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-5"
pools = ["studio"]

[[slot]]
name = "radix-6"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-6"
pools = ["studio"]

[[slot]]
name = "radix-7"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-7"
pools = ["studio"]

[[slot]]
name = "radix-8"
repository = "faberlang/radix"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/radix/slot-8"
pools = ["studio"]

[[slot]]
name = "gradus-1"
repository = "faberlang/gradus"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/gradus/slot-1"
pools = ["studio"]

[[slot]]
name = "gradus-2"
repository = "faberlang/gradus"
host = "ianzepp@studio-01"
dir = "/vivi/trees/faberlang/gradus/slot-2"
pools = ["studio"]
```

## Disposable bounded-worker protocol

The remaining sections describe per-job worker boundaries, not explicit
persistent SSH slots.

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
3. Every boundary starts clean. By default its build output is cold too. An
   opt-in per-pool Cargo target cache lives outside the boundary; nothing
   else from a job is reused.
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
  `open_for_dispatch` proposes the free slot indices, creates, waits for the
  seed boot to stop cleanly, marks the slot ready and claims it. `create`
  reserves its index under the driver's lock and fails as `slot exists: <id>`
  when the index (or a leftover ledger entry) is held; the dispatcher then
  tries the next index and never tears down the slot it did not make. Any
  other failure after the reservation destroys the slot, and a failed
  teardown is reported in the error. `reconcile` destroys a slot with no
  task, no open dispatch and no running boot once it is older than the setup
  grace (`orphan-stopped`).
- The provider is bound to its mailspace, not to one pool, so it serves its
  pool and that pool's `-doctor` self-test pool alike. Callers tell slots
  apart by their pool mark.

`boot-slot.sh` and `job-run.sh` run inside the boundary. When configured,
they source the Cargo cache controls after checkout, before repository seed
commands or the harness can build.

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

## Persistent Cargo output (opt-in)

Configure each pool independently in the mailspace config (not worker.conf):

```toml
[pools.rust.worker.cargo_target]
enabled = true
root = "/Volumes/Build"
cap_gib = 32
sccache = false
```

To verify work without a second cold build, configure the target's fixed
`[pump.targets.<id>.check]` (`command`, `timeout_s`) in the dispatching
mailspace. The job runner executes it after the agent and its commit step,
before publication, with this same slot's Cargo environment; a failing check
still publishes and records `check_rc`, `check_s`, `check_tail`. See
[fixed post-agent checks](../README.md#fixed-post-agent-checks-per-target)
for the Rust example, bounds, skip rules and red-result behavior.

The table is off when absent. `root` is an absolute worker build root, on the
same filesystem as `apfs_dir`; when omitted it is the parent of `apfs_dir`.
It must not be inside a job boundary. The driver resolves it on the worker,
checks the filesystem and rejects symlinked target paths. A slot gets
`CARGO_TARGET_DIR=<root>/slot-targets/<pool>/slot-NN`, with at least two
slot-number digits. The checkout, job inputs and temporary directory remain
in the disposable boundary. Pools should have unique names on a worker:
a lifetime reservation refuses concurrent use of the same target path even
from another mailspace or worker root. The normal worker lock serializes
creation and teardown; different slots never wait on one Cargo target lock.
A nonce ties each reservation to its ledger, so retrying an interrupted old
teardown cannot trim or unlock a newer owner's cache.

`cap_gib` defaults to 32 and accepts 1..=1024 GiB. On destruction, after the
slot's processes and boundary are gone, `du -sk` measures allocated target
size. Above the cap, the entire target is removed; otherwise it is kept for
the next job at that index. The ledger records the path and cap at creation,
so subsequent config edits do not redirect cleanup. Stopping a job without
destroying its held slot does not trim it. `rustc -vV` runs in the selected
checkout (honoring rustup's repository toolchain) before builds; a changed or
missing `.vivi-rustc` marker discards the old target. An unreadable compiler
fails preparation rather than risking stale output.

With `sccache = true`, install `sccache` on the worker's login PATH. Jobs also
receive `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR=<root>/sccache`,
`SCCACHE_SERVER_UDS=<root>/sccache/server.sock` and `CARGO_INCREMENTAL=0`.
Before building, the driver helper starts/reuses the server from that cache
with `TMPDIR=<root>/sccache/tmp` and a persistent working directory, never
from a job's temporary directory. The dedicated socket avoids adopting an
unrelated server started with disposable scratch. The agent itself still
gets its job-local TMPDIR. Slots share only this sccache cache, not targets.
The target cap does not change sccache's own built-in cache limit.

Disabling the table affects new jobs and leaves existing persistent caches
in place; it does not delete them. A crash between target reservation and
teardown can leave a `slot-NN.lock` directory. Reconcile/destroy the owning
slot first. If its ledger is also lost, verify no slot or compiler is using
the target before manually removing that reservation; it is never stolen.

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
- **PATH.** The boot wrapper is a login shell (for the variables its profile
  exports). On macOS its profile runs `path_helper`, which moves the system
  directories in front of the SSH session's `PATH`; the wrapper puts the
  session's `PATH` (passed in `VIVI_SESSION_PATH`) back in front and drops
  repeated entries before `boot.sh` runs, so a job resolves tools in the same
  order an SSH login does.

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
