# Vivi 11.2.0

Vivi 11.2.0 adds CPU pods to the RunPod pool provider and ships four skill
updates in the embedded skills. The mailspace schema stays at 8. An existing
`.vivi/` opens with this binary.

## RunPod CPU pods

```toml
[pools.<name>.runpod]
compute = "cpu"               # default "gpu"
```

A `compute = "cpu"` pool rents a pod without a GPU for each dispatch. Such a
pool takes no `gpu_types`, `gpu_count` or `min_cuda_version`, and
`container_disk_gb` is capped at 20 (the RunPod API refuses more; validation
now says so before any pod is rented). `doctor` and the refusal text of a
failed dispatch know the tier.

Verified live on 2026-10-12: create, self-test, a read-only dispatch and a
write dispatch that pushes a branch, on `runpod/base` pods (2 vCPU and $0.06/h
observed, whole dispatches four to seven minutes each).

## Skills

The embedded `vivi` skill carries these additions; `vivi skills show vivi`
prints them, and `vivi skills install <dir> --force` refreshes an installed
copy.

- **RunPod images.** The image must start sshd itself, on both tiers: a stock
  registry image rents and then never answers on SSH. Use an official RunPod
  image and install the toolchain through `setup`. A toolchain installed under
  `/root` is invisible to the worker user; the skill gives the `/opt`
  placement.
- **Fly bring-up order.** On Fly, `create` makes the app, so it runs before
  `doctor`. Setting a secret on a machine-only app may print a launch error
  although the secret has landed.
- **Local pools.** A slot inherits the `PATH` of the dispatching `vivi`.
  `vivi pool wait` returns when the first named dispatch finishes.
- **SSH pools.** The root may sit on a memory filesystem the host already
  mounts, which makes every slot in-memory. The harness environment comes
  from `~/.bash_profile` when the host has one.

## Upgrading

1. Install 11.2.0. No schema migration is needed.
2. The [11.1.0](release-v11.1.0.md) notes and the
   [11.0.0](release-v11.0.0.md) upgrade steps still apply if you have not
   taken them.
