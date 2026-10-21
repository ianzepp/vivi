# Vivi 17.0.0

Major release on 16.0.4. One configuration change breaks old configs, as
described below. The rest is new: pools that run each job in a bounded
workspace on an SSH worker, pools that fetch and push a plain Git server
instead of GitHub, and APFS volumes as a second kind of per-job disk.

## Breaking: `ramdisk` tables are now `workspace`

`[pools.<name>.ramdisk]` and `[pump.ramdisk]` are renamed to
`[pools.<name>.workspace]` and `[pump.workspace]`, and gain a `kind`. The old
names are no longer accepted, so a config that still has one fails to load.
To migrate, rename the table. `kind` defaults to `ram`, which behaves exactly
as `ramdisk` did:

```toml
[pools.lanes.workspace]   # was [pools.lanes.ramdisk]
kind = "ram"              # or "apfs"
size_gib = 12
```

## APFS volumes as per-job workspaces (`local` pools, macOS)

`kind = "apfs"` gives each job of a `local` pool an APFS volume with a quota
of `size_gib`, instead of a RAM disk. The volume is made in the container
that holds the slot, or in the one `container = "diskN"` names. It costs no
memory for the disk itself, so it suits machines whose memory is small next
to their storage. Admission checks the memory margin and the container's
free space, less what other Vivi volumes may still grow into; a job that
does not fit waits with `apfs-wait`. Volumes use a quota and never a
reserve. Teardown verifies the device against the job's record. The pump
sweeps orphaned volumes at startup, because APFS volumes survive a reboot.

## Worker pools: one bounded workspace per job on an SSH worker

`provider = "worker"` runs each job in a workspace built for it alone on a
machine reached over SSH, and destroys it when the result is collected:

```toml
[pools.studio-apfs]
provider = "worker"
slots = 4

[pools.studio-apfs.worker]
host = "studio"   # anything ssh accepts
kind = "apfs"     # apfs | ram | dir
size_gib = 12
```

- **Per dispatch, like RunPod.** A dispatch builds the slot, seeds it, runs
  the job and collects the result, and collecting it destroys the slot.
  Nothing rests between jobs and nothing is cached.
- **Boundaries (macOS).** `apfs` is a sparse APFS disk image of `size_gib`,
  made by the login user: a hard size limit, using disk only as it is
  written. `ram` is a RAM disk. `dir` is a plain directory. All three work
  over SSH with nobody logged in at the console.
- **The worker sets its own limits.** `<root>/worker.conf` (default root
  `~/vivi-worker`) holds `ram_budget_gib`, `ram_margin_gib`, `apfs_dir`,
  `apfs_budget_gib` and `dir_root`. A request over a budget, or over free
  memory or disk, fails as `no free slot: <reason>`, and the pump tries the
  next pool.
- **Nothing is installed on the worker.** The driver (`worker-macos.sh`) is
  embedded in `vivi` and sent with every call, like the `ssh` provider's
  script. It needs git, tar, perl, lsof, diskutil, hdiutil and plutil.
- **Stopping** a slot ends every process with a file or working directory in
  its boundary, plus the boot wrapper's process group and descendants.
- `vivi pool doctor --test` works on worker pools. Design and the macOS
  measurements behind it are in `docs/worker-driver-protocol.md`.

Requirement: on an external drive, macOS denies every file and image
operation over SSH unless Remote Login has "Allow full disk access for remote
users".

## Plain Git pool sources (`remote_url`)

A pool can fetch from and push to any Git server instead of GitHub. Set
`remote_url` (an SSH or `file://` address) in place of `repo` and `[github]`:

```toml
[pools.lanes]
remote_url = "git@gitbox:lanes/owner/repo.git"
```

There is no token and no GitHub API. Refs resolve with `git ls-remote` on the
dispatching machine, and the slots fetch and push with the login user's own
key. A ref is a branch or tag name, or a full commit hash; an abbreviated
hash is refused, and an annotated tag resolves to its commit. Setting both
`repo` and `remote_url`, or an `http(s)` `remote_url`, is a configuration
error. A pump target whose remote is not on GitHub, and that names no
`github_repo`, now dispatches as a plain Git source instead of being
refused.

## Known limits

- The worker driver exists for macOS only. A Linux driver is planned.
- A worker pool places jobs by trying pools in order. Placement that reads
  each worker's `offer` is planned.
