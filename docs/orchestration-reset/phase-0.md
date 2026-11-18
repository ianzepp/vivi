# Phase 0: stop the bleeding

**Status**: started 2026-11-18. Done items are done. The items under "Waiting on the operator" have not been applied.

The plan is in [README.md](README.md). The operator chose direction B (Temporal) on 2026-11-18.

## Done

| Item | Result |
|---|---|
| Find the hub `/mnt/vivi` writer | The faberlang pump: about 77 GB/h while idle. Pausing it did nothing, because a paused pump keeps ticking. `vivi pump stop` dropped the drive to 57 MB/h. **The faberlang pump is stopped**, so its cadence checks do not run. The vivi pump is paused and writes almost nothing. |
| Watch `/mnt/vivi` | `/vivi/orch/disk-writes` now logs sdd (`/mnt/vivi`) next to sdc (`/vivi`) every 5 minutes, and keeps each drive's window separate. Backup at `disk-writes.bak-20261118`. `/vivi/orch/health-check` still passes. |
| Keep the pump flow off | The faberlang pump is stopped and the vivi pump is paused. No flow runs anywhere. |
| A backlog count anyone can read | `/vivi/orch/backlog` on hub fetches radix `vivi/*` into `/vivi/orch/cache/radix.git`, classifies every unit branch, and prints the counts (about 12 s). `--tsv PATH` writes the branch list. |
| Triage lists | Under `/vivi/orch/triage/` on hub: `2026-11-18-radix-units.tsv` and `-detail.tsv`, `2026-11-18-open-needs-hold.tsv`, `2026-11-18-open-needs-work-on-main.tsv`. |

`/vivi/orch` is not a git repository. Its scripts have only `.bak` copies, which the research found to be part of the drift. It should become a repository, or be retired with the pump.

## Radix unit branches (256)

| Class | Count | Need still open | What it means | Proposed action |
|---|---|---|---|---|
| merged | 37 | 37 | Every commit is on `vivi/main` | Archive the ref |
| equivalent-landed | 45 | 44 | Different commits, but every patch is on `vivi/main` | Archive the ref |
| clean-merge | 9 | 8 | Merges onto `vivi/main` with no conflict | Keep. First workload for the Phase 1 gate |
| conflict-pin-only | 32 | 32 | Conflicts only on `corpus/known-results.json`, the pin file retired on Nov 6 (need 93b36924 is open to finish retiring it) | Keep. Resolve the pin side mechanically, then send through the Phase 1 gate |
| conflict-code | 133 | 130 | Real code conflicts, a mean 354 commits behind main. 40 belong to the corpus stream 3b2c49a9. Hot files are `radix-mir-clif` (`abi.rs`, `lower*`), `radix-hir`, and `grammar.jsonl` | Archive the ref. The need stays open and is redone from current main under the new runtime |

"Archive" means moving the ref to `refs/archive/vivi/unit/*` on the hub lane repo. No commit is lost, the ref leaves the working namespace, and a rename undoes it.

That is 82 + 133 = 215 refs to move. The 41 that could still land stay where they are.

## Open faberlang needs (765)

- **By sender:** 673 `operator`, 47 `mind`, about 30 `corpus-*` and other cadence roles. Claude on hub also filed as `operator`, so the sender does not separate auto-filed needs from the operator's own.
- **By age:** 264 were created in October and 489 in November.
- **Hold candidates (79):** 47 filed by the mind, and 32 cadence "`<leg>` red on faberlang-radix at `<sha>`" reports. Vivi has no need-to-want demotion, so the proposal is a `hold-triage` mark (`vivi need mark`). It is reversible with `unmark`, and Temporal intake will skip marked needs.
- **Closure review (54):** open needs whose unit work is already on `vivi/main`. Each needs a read: is it finished, or does it have scope left? This is not mechanical, so it is a list for review, not a change.

## Network: the tailnet is gone

The plan said "fix `hub.tailc3379f.ts.net` not resolving on hub". The cause is different.

- Tailscale no longer runs on hub: there is no binary and no `tailscaled`. It is stopped on the laptop.
- Hosts are reached by `.local` names: `hub.local`, `studio-01.local`, `studio-02.local`, `mini-m1.local`.

Stale tailnet references that remain:

| Where | Effect | Proposed |
|---|---|---|
| The stopped faberlang pump's gradus integration clone | `ls-remote` exit 128 every 10 minutes while the pump runs | None. It retires with the pump |
| The model proxy, `vivi-proxy.service` on `0.0.0.0:18180` | Unreachable from the laptop over the LAN. The documented rule is "tailnet only; LAN is refused" | Operator decision, because it is a security boundary: re-enable Tailscale, or allow the LAN |
| `~/CLAUDE.md` (skills repo), Local context | It sends every agent to `hub.tailc3379f.ts.net` and its proxy | Update after the proxy decision |

## Waiting on the operator

1. Archive the 215 landed or conflicting radix unit branch refs, as described above.
2. Mark the 79 hold candidates `hold-triage`.
3. The proxy and tailnet decision.
4. Who reads the 54 closure-review needs: the operator, or a review job once the runtime is up.

## Next: Phase 1

The gate is the full workspace test on a dedicated warm tree, with batch-and-bisect. Its first workload is the 41 landable branches.
