# Vivi 11.0.1

Vivi 11.0.1 is a documentation patch on 11.0.0. The binary is unchanged
except the version string. The mailspace schema stays at 8. An existing
`.vivi/` opens with this binary.

## Documentation

- **Pool bring-up.** The vivi skill now has an order for a new pool:
  configure provider settings only, prove the host with `doctor`, then add
  `repo` and GitHub, then decide the harness before creating slots, then
  smoke both dispatch modes. It also records facts and setup traps from a
  first docker-over-ssh pool: an `ssh://` host the docker CLI accepts, tmpfs
  sized from real build output, and `facts` that print what the machine
  already holds (a rustup shim that downloads fails every receipt).
- **Linking a mailspace to a server project.** New section in the vivi
  skill, with a pointer from the README: `[sync]` fields, `sync status`
  minting the identity key, the postmaster vouch, `sync join`, and the join
  errors (`admission_required`, `mailspace_prefix_taken`). Never copy a
  `.vivi` folder between machines.

## Upgrading

1. Install 11.0.1. No schema migration is needed.
2. The [11.0.0](release-v11.0.0.md) upgrade steps still apply if you have
   not taken them.
