#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Red-and-retry summary for lab runs, with a real-time estimate.

    fail-summary.py [--replays N] <run dir>...

The lab tests mechanics: model calls are instant (or nearly), so its wall time
says nothing about the hub. Per run this prints what the pump did, from its
own records (pump/pump.db, steps.txt): hand jobs and second attempts, slice
merges and injected task reds, slices that ran out of attempts, need rung runs
and injected need reds, and the flow's steps.

Then it estimates the run's time on the hub by replaying the same outcomes
(the same slice attempts red, the same need reds, the same rework) with real
durations: one model call per step, hands over the run's slots, one slice
merge at a time per need, a red merge queuing that slice's next hand. Hand
durations are drawn from data/hub-hand-secs.txt (563 hub deliveries) on each
replay; the result is the median and 90th percentile over the replays, and
the same replay with every merge green, for the cost of the reds. A stage the
scenario switched off ([world] stages) costs one tick and no model time. A run
that ended locked is estimated to its CTO step, the point where it stops.

Durations (seconds) and where each comes from are in DURATIONS below.
"""
import argparse
import heapq
import json
import random
import sqlite3
import statistics
import tomllib
from collections import defaultdict
from pathlib import Path

LAB = Path(__file__).resolve().parent
HAND_SECS = [int(x) for x in (LAB / "data" / "hub-hand-secs.txt").read_text().split()]

# (seconds, source). "assumed" marks a number nothing has measured yet.
DURATIONS = {
    "planner": (743, "hub log: median of 214 planner jobs"),
    "admitter": (332, "assumed: the hand median"),
    "slicer": (332, "assumed: the hand median"),
    "auditor": (332, "assumed: the hand median"),
    "judge": (332, "assumed: the hand median"),
    "cto": (743, "assumed: the planner median"),
    "task_rung": (21, "radix ./scripta/test --stage 1, measured 19-23 s"),
    "need_rung": (60, "assumed: cargo check --workspace --tests plus stage 1"),
    "git": (3, "lab: median slice merge job run, git work only"),
    "tick": (24, "hub log: the pump collects about every 24 s"),
}


def secs(name):
    return DURATIONS[name][0]


def jobs(run):
    db = sqlite3.connect(f"file:{run / 'pump' / 'pump.db'}?mode=ro", uri=True)
    (value,) = db.execute("select value from meta where key = 'jobs'").fetchone()
    return list(json.loads(value).values())


def receipt(job):
    outcome = job.get("outcome") or {}
    if isinstance(outcome, str):
        return outcome
    return outcome.get("receipt", "") or ""


def payload(job):
    p = job.get("payload") or {}
    return json.loads(p) if isinstance(p, str) else p


def steps(run):
    rows = []
    path = run / "steps.txt"
    if path.exists():
        for line in path.read_text().splitlines()[1:]:
            parts = line.split("|")
            if len(parts) == 3:
                rows.append(parts)
    return rows


def world_of(run):
    plan = LAB / "scenarios" / run.name.split("-", 1)[1] / "plan.toml"
    return tomllib.loads(plan.read_text()).get("world", {})


def slots_of(run):
    world = world_of(run)
    return int(world.get("machines", 1)) * int(world.get("slots", 4))


def stages_of(run):
    """The scenario's [pump.flow.stages] switches: stage -> runs."""
    switches = world_of(run).get("stages", {})
    return {s: bool(switches.get(s, True)) for s in ("admit", "audit", "judge", "need_rung")}


def outcomes(all_jobs, rows):
    """Per slice node, the red flag of each merge attempt, in attempt order.
    A slice that never reached a merge gets one green attempt: in the replay
    it builds and merges like any other, unless the run stops first."""
    per = defaultdict(list)
    for j in all_jobs:
        if j.get("kind") == "slice-merge":
            p = payload(j)
            per[p.get("node")].append((int(p.get("attempt", 1)), "injected task failure" in receipt(j)))
    out = {node: [red for _, red in sorted(v)] for node, v in per.items()}
    for node, _, _ in rows:
        if node.startswith("slice:") and node not in out:
            out[node] = [False]
    return out


def replay(attempts, slots, rework, locked, rng, stages, all_green=False):
    """Seconds from the need's dispatch to its merge to main (or to the lock),
    on hub durations, as an event loop over the run's own outcomes."""
    merge_secs = secs("git") + secs("task_rung") + 2 * secs("tick")
    step = lambda name: secs(name) + secs("tick")  # a model step, applied on the next tick
    # A switched-off stage answers by rule on the next tick: no model time.
    gated = lambda stage, name: step(name) if stages[stage] else secs("tick")
    t0 = step("planner") + gated("admit", "admitter") + step("slicer")
    events = []  # heap of (time, seq, kind, task, attempt index)
    seq = 0
    pending = [(task, 0) for task in attempts]  # hands waiting for a slot, FIFO
    merges = []  # built attempts waiting for the merge, FIFO
    free, merging, end = slots, False, t0

    def push(at, kind, task, k):
        nonlocal seq
        seq += 1
        heapq.heappush(events, (at, seq, kind, task, k))

    def kick(now):
        nonlocal free, merging
        while free and pending:
            task, k = pending.pop(0)
            free -= 1
            push(now + rng.choice(HAND_SECS) + secs("tick"), "built", task, k)
        if not merging and merges:
            task, k = merges.pop(0)
            merging = True
            push(now + merge_secs, "merged", task, k)

    kick(t0)
    while events:
        now, _, kind, task, k = heapq.heappop(events)
        end = now
        if kind == "built":
            free += 1
            merges.append((task, k))
        else:
            merging = False
            reds = attempts[task]
            red = not all_green and k < len(reds) and reds[k]
            if red and k + 1 < len(reds):
                pending.append((task, k + 1))
        kick(now)
    if locked and not all_green:
        # An exhausted slice holds only its dependents: every other slice
        # merges first, then the build fails and the CTO locks.
        return end + step("cto")
    rung = secs("need_rung") + 2 * secs("tick") if stages["need_rung"] else secs("tick")
    verify = max(gated("audit", "auditor"), rung)
    t = end + verify
    if rework and not all_green:
        hand = rng.choice(HAND_SECS) + secs("tick")
        t += gated("judge", "judge") + step("slicer") + hand + merge_secs + verify
    return t + secs("git") + 2 * secs("tick")  # merge to main


def summary(run, replays):
    result = (run / "result.txt").read_text().split() if (run / "result.txt").exists() else ["?", "?"]
    all_jobs = jobs(run)
    hands = [j for j in all_jobs if j.get("kind") == "hand"]
    merges = [j for j in all_jobs if j.get("kind") == "slice-merge"]
    rungs = [j for j in all_jobs if str(j.get("id", "")).startswith("rung-need")]
    rows = steps(run)
    slices = [r for r in rows if r[0].startswith("slice:")]
    nodes = {n: v or s for n, s, v in rows}
    flow = [f"{n}={v or s}" for n, s, v in rows if not n.startswith("slice:") and n != "sentinel"]
    attempts = outcomes(all_jobs, rows)
    rework = "rework1" in nodes
    rng = random.Random(1)
    locked = result[0] != "landed"
    stages = stages_of(run)
    est = sorted(replay(attempts, slots_of(run), rework, locked, rng, stages) for _ in range(replays))
    green = sorted(replay(attempts, slots_of(run), rework, locked, rng, stages, all_green=True) for _ in range(replays))
    q = lambda xs, p: xs[min(len(xs) - 1, int(p * len(xs)))]
    return {
        "run": run.name,
        "result": result[0],
        "secs": result[1] if len(result) > 1 else "?",
        "hand_jobs": len(hands),
        "second_attempts": sum(1 for j in hands if j["id"].split("-")[2] != "a1"),
        "slice_merges": len(merges),
        "task_reds": sum(1 for j in merges if "injected task failure" in receipt(j)),
        "slices_not_merged": sum(1 for r in slices if r[2] != "merged"),
        "need_rungs": len(rungs),
        "need_reds": sum(1 for j in rungs if "injected need failure" in receipt(j)),
        "flow": " ".join(flow),
        "locked": locked,
        "off": [k for k, v in stages.items() if not v],
        "est_median": statistics.median(est),
        "est_p90": q(est, 0.9),
        "green_median": statistics.median(green),
    }


def mins(s):
    return f"{s / 60:.0f} min"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--replays", type=int, default=500)
    ap.add_argument("runs", nargs="+")
    args = ap.parse_args()
    for arg in args.runs:
        s = summary(Path(arg), args.replays)
        off = ", ".join(s["off"]) or "none"
        print(f"{s['run']}: {s['result']} in {s['secs']}s (lab); stages off: {off}")
        print(f"  hand jobs {s['hand_jobs']} ({s['second_attempts']} second attempts); "
              f"slice merges {s['slice_merges']} ({s['task_reds']} injected task reds); "
              f"slices not merged {s['slices_not_merged']}")
        print(f"  need rungs {s['need_rungs']} ({s['need_reds']} injected need reds)")
        print(f"  flow: {s['flow']}")
        label = "to the lock" if s["locked"] else "to land"
        print(f"  hub estimate {label}: median {mins(s['est_median'])}, p90 {mins(s['est_p90'])}; "
              f"same run all green: {mins(s['green_median'])}")
    print("durations used:")
    for name, (value, source) in DURATIONS.items():
        print(f"  {name:10s} {value:5d} s  {source}")


if __name__ == "__main__":
    main()
