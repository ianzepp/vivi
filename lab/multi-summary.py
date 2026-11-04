#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Summary and hub-time estimate for multi-need lab runs (run-multi.sh).

    multi-summary.py [--replays N] <run dir>...

Per run, from the pump's records: each need's end state (landed, stuck), its
slices merged, exhausted (two red attempts) and blocked (downstream of an
exhausted slice, so never started), its task reds, need-rung reds and rework;
then totals.

The estimate replays the run's own outcomes with real durations (the
DURATIONS table in fail-summary.py): every need plans, admits and slices in
parallel; hands share the run's slots, first come first served across needs;
a slice starts only when every slice it depends on has merged; each need
merges one slice at a time; a red merge queues that slice's next attempt; an
exhausted slice holds only its dependents. A need with an exhausted slice ends
at the CTO lock once nothing else of it can land; a need that built runs its
verify (plus rework when the lab run reworked it) and merges to main. Reported:
the median and 90th percentile of the time until the last need ends, and the
median per-need end. A switched-off stage costs one tick and no model time.
"""
import argparse
import heapq
import importlib.machinery
import importlib.util
import json
import random
import sqlite3
import statistics
import tomllib
from collections import defaultdict
from pathlib import Path

LAB = Path(__file__).resolve().parent
_loader = importlib.machinery.SourceFileLoader("fail_summary", str(LAB / "fail-summary.py"))
_spec = importlib.util.spec_from_loader("fail_summary", _loader)
fs = importlib.util.module_from_spec(_spec)
_loader.exec_module(fs)
secs, HAND_SECS = fs.secs, fs.HAND_SECS


def load(run):
    plan = tomllib.loads((LAB / "scenarios" / run.name.split("-", 1)[1] / "plan.toml").read_text())
    world = plan.get("world", {})
    needs = {n["id"]: {s["id"]: s.get("deps", []) for s in n["slice"]} for n in plan["need"]}
    handles, states = {}, {}
    for line in (run / "needs.txt").read_text().split("\n"):
        parts = line.split()
        if len(parts) >= 2:
            handles[parts[1]] = parts[0]
            states[parts[0]] = parts[2] if len(parts) > 2 else "?"
    return world, needs, handles, states


def outcomes(run, handles):
    """need id -> slice id -> red flags per attempt; need id -> need-rung reds."""
    per = defaultdict(lambda: defaultdict(list))
    need_reds = defaultdict(int)
    for job in fs.jobs(run):
        p = fs.payload(job)
        need = handles.get(p.get("need"))
        if job.get("kind") == "slice-merge" and need:
            node = str(p.get("node", "")).removeprefix("slice:")
            per[need][node].append((int(p.get("attempt", 1)), "injected task failure" in fs.receipt(job)))
        elif str(job.get("id", "")).startswith("rung-need") and need:
            if "injected need failure" in fs.receipt(job):
                need_reds[need] += 1
    reds = {n: {s: [r for _, r in sorted(v)] for s, v in d.items()} for n, d in per.items()}
    return reds, need_reds


def step_rows(run, handles):
    rows = defaultdict(dict)
    path = run / "steps.txt"
    if path.exists():
        for line in path.read_text().splitlines()[1:]:
            parts = line.split("|")
            if len(parts) == 4 and parts[0] in handles:
                rows[handles[parts[0]]][parts[1]] = (parts[2], parts[3])
    return rows


def blocked_cone(deps, exhausted):
    blocked, changed = set(), True
    while changed:
        changed = False
        for s, ds in deps.items():
            if s not in blocked and s not in exhausted and any(d in exhausted or d in blocked for d in ds):
                blocked.add(s)
                changed = True
    return blocked


def replay(needs, reds, rework, slots, stages, rng, all_green=False):
    """(time the last need ends, per-need end times) on hub durations."""
    step = lambda name: secs(name) + secs("tick")
    gated = lambda stage, name: step(name) if stages[stage] else secs("tick")
    merge_secs = secs("git") + secs("task_rung") + 2 * secs("tick")
    rung = secs("need_rung") + 2 * secs("tick") if stages["need_rung"] else secs("tick")
    verify = max(gated("audit", "auditor"), rung)
    front = step("planner") + gated("admit", "admitter") + step("slicer")
    events, seq = [], 0
    def push(at, kind, need, sl, k):
        nonlocal seq
        seq += 1
        heapq.heappush(events, (at, seq, kind, need, sl, k))
    merged = {n: set() for n in needs}
    exhausted = {n: set() for n in needs}
    started = {n: set() for n in needs}
    pending, free = [], slots          # pending: (ready at, seq, need, slice, attempt)
    merge_q = {n: [] for n in needs}
    merging = {n: False for n in needs}
    ends = {}

    def attempts(n, s):
        r = reds.get(n, {}).get(s)
        return [False] if not r or all_green else r

    def release(n, now):
        """Queue every slice of n whose deps have all merged."""
        for s, ds in needs[n].items():
            if s not in started[n] and all(d in merged[n] for d in ds):
                started[n].add(s)
                pending.append((now, n, s, 0))

    def settled(n):
        live = set(needs[n]) - blocked_cone(needs[n], exhausted[n])
        return all(s in merged[n] or s in exhausted[n] for s in live)

    def finish(n, now):
        if n in ends or not settled(n):
            return
        if exhausted[n] and not all_green:
            ends[n] = now + step("cto")
        else:
            t = now + verify
            if rework.get(n) and not all_green:
                t += gated("judge", "judge") + step("slicer") + rng.choice(HAND_SECS) + secs("tick") + merge_secs + verify
            ends[n] = t + secs("git") + 2 * secs("tick")

    def kick(now):
        nonlocal free
        while free and pending:
            pending.sort()
            _, n, s, k = pending.pop(0)
            free -= 1
            push(now + rng.choice(HAND_SECS) + secs("tick"), "built", n, s, k)
        for n in needs:
            if not merging[n] and merge_q[n]:
                s, k = merge_q[n].pop(0)
                merging[n] = True
                push(now + merge_secs, "merged", n, s, k)

    for n in needs:
        push(front, "front", n, None, 0)
    while events:
        now, _, kind, n, s, k = heapq.heappop(events)
        if kind == "front":
            release(n, now)
        elif kind == "built":
            free += 1
            merge_q[n].append((s, k))
        else:
            merging[n] = False
            tries = attempts(n, s)
            red = k < len(tries) and tries[k]
            if not red:
                merged[n].add(s)
                release(n, now)
            elif k + 1 < len(tries):
                pending.append((now, n, s, k + 1))
            else:
                exhausted[n].add(s)
            finish(n, now)
        kick(now)
    return max(ends.values()), ends


def summary(run, replays):
    world, needs, handles, states = load(run)
    reds, need_reds = outcomes(run, handles)
    rows = step_rows(run, handles)
    slots = int(world.get("machines", 1)) * int(world.get("slots", 4))
    switches = world.get("stages", {})
    stages = {s: bool(switches.get(s, True)) for s in ("admit", "audit", "judge", "need_rung")}
    per_need, rework = [], {}
    for n, deps in needs.items():
        r = reds.get(n, {})
        exhausted = {s for s, v in r.items() if len(v) >= 2 and all(v)}
        merged = sum(1 for s in deps if rows[n].get(f"slice:{s}", ("", ""))[1] == "merged")
        blocked = blocked_cone(deps, exhausted)
        rework[n] = "rework1" in rows[n]
        per_need.append((n, states.get(n, "?"), len(deps), merged, len(exhausted), len(blocked),
                         sum(sum(v) for v in r.values()), need_reds.get(n, 0), rework[n]))
    rng = random.Random(1)
    runs = [replay(needs, reds, rework, slots, stages, rng) for _ in range(replays)]
    green = [replay(needs, reds, rework, slots, stages, rng, all_green=True)[0] for _ in range(replays)]
    last = sorted(r[0] for r in runs)
    each = statistics.median(statistics.median(r[1].values()) for r in runs)
    return per_need, last, each, statistics.median(green), [k for k, v in stages.items() if not v]


def mins(s):
    return f"{s / 60:.0f} min"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--replays", type=int, default=300)
    ap.add_argument("runs", nargs="+")
    args = ap.parse_args()
    for arg in args.runs:
        run = Path(arg)
        result = (run / "result.txt").read_text().split()
        per_need, last, each, green, off = summary(run, args.replays)
        print(f"{run.name}: {result[0]} landed, {result[1]} stuck of {result[2]} in {result[3]}s (lab); "
              f"stages off: {', '.join(off) or 'none'}")
        print("  need  state   slices merged exhausted blocked task-reds need-reds rework")
        for n, st, total, merged, ex, bl, tr, nr, rw in per_need:
            print(f"  {n:5s} {st:7s} {total:6d} {merged:6d} {ex:9d} {bl:7d} {tr:9d} {nr:9d} {'yes' if rw else 'no'}")
        tot = [sum(x[i] for x in per_need) for i in (2, 3, 4, 5, 6)]
        print(f"  total         {tot[0]:6d} {tot[1]:6d} {tot[2]:9d} {tot[3]:7d} {tot[4]:9d}")
        print(f"  hub estimate, last need ends: median {mins(statistics.median(last))}, "
              f"p90 {mins(last[int(0.9 * len(last))])}; median need ends at {mins(each)}; "
              f"all green: {mins(green)}")


if __name__ == "__main__":
    main()
