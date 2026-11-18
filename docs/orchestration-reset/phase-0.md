# Phase 0: stop the bleeding

**Status**: 2026-11-18. The operator approved the branch archive, the need holds, the GitHub prune and the move off the tailnet, and all are applied. Two items are open: the closure-review reader and the Phase 0 gate measurement.

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

## Applied 2026-11-18

**Radix lane.** The 215 refs (82 landed, 133 code conflicts) were moved to `refs/archive/vivi/unit/*` in one atomic push, each leased on the sha the triage saw.
- `/vivi/orch/backlog` now reports 41 unit branches: 9 clean-merge and 32 conflict-pin-only.
- The script that did it is `/vivi/orch/triage/archive_units.sh`.

**Other lanes.** The same rule was applied, and the matching GitHub branches deleted (`/vivi/orch/triage/archive_lane.sh`; a GitHub delete happens only when GitHub's sha is on the lane's branch):

| Repo | Lane archived | GitHub deleted | Kept, merges clean |
|---|---|---|---|
| faberlang/inferentia | 11 (2 need, 9 unit) | 11 | `vivi/unit/67a41e9c-11-a1` |
| faberlang/gradus | 2 | 0 | `vivi/unit/cdbff248-a2` |
| vivi/vivi (GitHub ianzepp/vivi) | 2 | 0 | `vivi/unit/7f7d3c37-a1`, `vivi/unit/96d2c79b-a2` |

GitHub radix already had only 5 branches: `main`, `vivi/main`, `peer/kir`, `release/1.19`, `release/on-version-bump`. The research's figure of about 6,170 was out of date, so nothing needed pruning there.

`git-sync` never pulls a pruned `vivi/*` branch back from GitHub, and it does not sync `refs/archive/*`. An archive ref is undone with a push that renames it back.

**Need holds.** 75 of the 79 candidates are marked `hold-triage` through the vivi server.
- The 4 others are absorbed, so Vivi refuses any change: 422796df, 747421b3, 630615db, a7fbdec5.
- 30 of the marks first failed, because the need was "held" by `mind`, `planner`, `head-cso` or `hand`, and each had to be retried naming that role. See the roles note in [README.md](README.md).

**Network: the tailnet is gone (operator, 2026-11-13; instruction 2026-11-18 to allow the LAN).**
- Tailscale is not on hub, and it is stopped on the laptop. Hosts are reached as `hub.local`, `studio-01.local`, `studio-02.local` and `mini-m1.local`.
- **The model proxy already allowed the LAN.** A ufw rule admits `192.168.1.0/24` on port 18180, and a request by IP answers 401, which is correct without a token. The earlier "refused" reading was wrong: curl timed out *resolving* `hub.local` (mDNS, 5 s). `.local` lookups being slow is the real network issue. Use the IP where latency matters.
- **Instructions updated in the skills repo:** `AGENTS.md`, which is the shared `CLAUDE.md` (Hosts, a new Network line, the proxy URL), `sysadmin/SKILL.md`, `hub/SKILL.md` and `hub/references/local-pump.md`.
- **Left, needs sudo:** `vivi-proxy.service` still says "tailnet front door" with `Wants=`/`After=tailscaled.service`. This is harmless, because that unit no longer exists.
- **Left, retires with the pump:** the stopped faberlang pump's gradus integration clone still names `hub.tailc3379f.ts.net`.

## Still open

1. Who reads the 54 closure-review needs (`2026-11-18-open-needs-work-on-main.tsv`): the operator, or a review job once the runtime is up.
2. The Phase 0 gate: the hub drive under 50 GB/day idle, measured over a full day by `disk-writes` (it watches sdd now).

## Next: Phase 1

The gate is the full workspace test on a dedicated warm tree, with batch-and-bisect. Its first workload is the 41 landable branches.
