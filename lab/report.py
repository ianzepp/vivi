#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Timeline report for one lab run, from the pump's own job records.

    report.py <run dir> [--slots N]

Reads <run dir>/pump/pump.db (the `jobs` entry of the meta table: every
durable job with queued_at, dispatched_at and applied_at) and prints:
- the run's wall time, first job queued to last job applied;
- per job kind: count, queue wait (queued -> dispatched) and run time
  (dispatched -> applied), mean and max;
- per pool: peak and mean busy slots while the run was active;
- ready-but-idle seconds: seconds when at least one job waited to dispatch
  while fewer than the total slots were busy;
- per unit: hand applied -> its slice merge applied, and the merge chain.
"""
import argparse
import json
import sqlite3
import statistics
from collections import defaultdict
from pathlib import Path


def load_jobs(run):
    db = sqlite3.connect(f"file:{run / 'pump' / 'pump.db'}?mode=ro", uri=True)
    (value,) = db.execute("select value from meta where key = 'jobs'").fetchone()
    return list(json.loads(value).values())


def kind_of(job):
    jid = job["id"]
    if jid.startswith("slice-") and "-merge-" in jid:
        return "slice-merge"
    if jid.startswith("slice-"):
        return "hand"
    if jid.startswith("merge-main-"):
        return "merge-main"
    if jid.startswith("rung-"):
        return f"rung-{jid.split('-')[1]}"
    if jid.startswith("step-"):
        return job.get("role") or job.get("kind") or "step"
    return job.get("kind") or jid.split("-")[0]


def stats(values):
    if not values:
        return "-"
    return f"mean {statistics.mean(values):5.1f}s  max {max(values):5.1f}s"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run", type=Path)
    ap.add_argument("--slots", type=int, default=0, help="total slots, for idle accounting")
    args = ap.parse_args()
    jobs = [j for j in load_jobs(args.run) if j.get("queued_at")]
    done = [j for j in jobs if j.get("dispatched_at") and j.get("applied_at")]
    t0 = min(j["queued_at"] for j in jobs)
    t1 = max(j.get("applied_at") or j["queued_at"] for j in jobs)
    print(f"run: {args.run.name}")
    print(f"jobs: {len(jobs)} ({len(done)} dispatched and applied); wall {t1 - t0}s\n")

    by_kind = defaultdict(list)
    for j in jobs:
        by_kind[kind_of(j)].append(j)
    print(f"{'kind':14} {'n':>4}  {'queue wait':28} {'run (dispatch->applied)':28}")
    for kind, js in sorted(by_kind.items(), key=lambda kv: min(j["queued_at"] for j in kv[1])):
        wait = [j["dispatched_at"] - j["queued_at"] for j in js if j.get("dispatched_at")]
        run = [j["applied_at"] - j["dispatched_at"] for j in js if j.get("dispatched_at") and j.get("applied_at")]
        print(f"{kind:14} {len(js):4}  {stats(wait):28} {stats(run):28}")

    # Busy slots per pool, per second: a job holds a slot from dispatch to applied.
    busy = defaultdict(lambda: defaultdict(int))
    waiting = defaultdict(int)
    for j in done:
        for t in range(j["dispatched_at"], j["applied_at"]):
            busy[j.get("pool") or "?"][t] += 1
    for j in jobs:
        end = j.get("dispatched_at") or t1
        for t in range(j["queued_at"], end):
            waiting[t] += 1
    span = range(t0, t1)
    print("\npool    peak  mean busy slots")
    for pool in sorted(busy):
        series = [busy[pool][t] for t in span]
        print(f"{pool:6} {max(series):5}  {statistics.mean(series):5.2f}")
    if args.slots:
        total = [sum(busy[p][t] for p in busy) for t in span]
        idle_ready = sum(1 for t, b in zip(span, total) if waiting[t] and b < args.slots)
        print(f"\nready-but-idle: {idle_ready}s of {len(span)}s "
              f"(a job waited while fewer than {args.slots} slots were busy)")

    hands = {j["id"]: j for j in jobs if kind_of(j) == "hand"}
    merges = {j["id"].split("-merge-")[0]: j for j in jobs if kind_of(j) == "slice-merge"}
    lat, gaps = [], []
    prev_end = None
    for hid, merge in sorted(merges.items(), key=lambda kv: kv[1].get("dispatched_at") or 0):
        hand = hands.get(hid)
        if hand and hand.get("applied_at") and merge.get("applied_at"):
            lat.append(merge["applied_at"] - hand["applied_at"])
        if prev_end and merge.get("dispatched_at"):
            gaps.append(merge["dispatched_at"] - prev_end)
        prev_end = merge.get("applied_at")
    if lat:
        print(f"\nunits: {len(merges)} slice merges")
        print(f"  hand applied -> merge applied: {stats(lat)}")
        mruns = [m["applied_at"] - m["dispatched_at"] for m in merges.values()
                 if m.get("dispatched_at") and m.get("applied_at")]
        print(f"  slice merge run:               {stats(mruns)}  (sum {sum(mruns)}s)")
        print(f"  gap between merges:            {stats(gaps)}")


if __name__ == "__main__":
    main()
