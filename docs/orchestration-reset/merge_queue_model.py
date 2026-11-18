# -*- coding: utf-8 -*-
"""Merge-queue model: how fast can units land on main, and what breaks?

A discrete-event simulation of the landing problem, driven by rates measured
on hub (see README.md in this folder). No dependencies; python3 3.9+.

    python3 merge_queue_model.py            # the scenario table
    python3 merge_queue_model.py --seed 7   # another random seed

Model
-----
* Units arrive as a Poisson stream at `rate` per hour (finished agent work).
* Each unit is truly bad with probability `p_bad`: it breaks main.
* A gate run takes `gate_min` minutes (+-10% jitter) on one of `runners`.
* A gate run on a set of units fails if any unit is bad, or spuriously with
  probability `flake` (a noisy oracle: pins, wording, skipped crates).
* Strategies:
    serial      one unit per gate run (one at a time per runner, units
                tested independently against main; a pass merges).
    batch       up to `batch` units per gate run; a failed batch is
                bisected (halves run in parallel on free runners) until the
                failing singletons are ejected. Mergify/Trunk style.
    yolo        batch merged without waiting for the gate (what hub did on
                Nov 14: "merge every landing regardless of green").
Measured outputs over `days` simulated days:
    landed/h, median and p90 hours from arrival to merge, queue at end,
    good units wrongly ejected, bad units that reached main.
"""

import argparse
import heapq
import random
import statistics


def gate_time(rng, gate_min):
    return gate_min * rng.uniform(0.9, 1.1) / 60.0


def gate_passes(rng, units, flake):
    if any(u["bad"] for u in units):
        return False
    return rng.random() >= flake


class Sim:
    def __init__(self, rng, rate, p_bad, gate_min, runners, flake, strategy, batch, days):
        self.rng, self.rate, self.p_bad = rng, rate, p_bad
        self.gate_min, self.runners, self.flake = gate_min, runners, flake
        self.strategy, self.batch, self.horizon = strategy, batch, days * 24.0
        self.queue, self.events, self.free = [], [], runners
        self.landed, self.ejected_good, self.bad_on_main, self.ejected_bad = [], 0, 0, 0
        self.seq = 0
        self.pending_bisect = []  # unit groups waiting for a runner

    def push(self, t, kind, payload=None):
        self.seq += 1
        heapq.heappush(self.events, (t, self.seq, kind, payload))

    def run(self):
        t = 0.0
        while t < self.horizon:
            t += self.rng.expovariate(self.rate)
            self.push(t, "arrive", {"t": t, "bad": self.rng.random() < self.p_bad})
        while self.events:
            now, _, kind, payload = heapq.heappop(self.events)
            if now > self.horizon:
                break
            if kind == "arrive":
                self.queue.append(payload)
            elif kind == "done":
                self.free += 1
                self.finish(now, payload)
            self.dispatch(now)
        return self.report()

    def start(self, now, group, top):
        self.free -= 1
        self.push(now + gate_time(self.rng, self.gate_min), "done", {"units": group, "top": top})

    def dispatch(self, now):
        if self.strategy == "yolo":
            while self.queue:
                group = self.queue[: self.batch]
                del self.queue[: self.batch]
                for u in group:
                    self.land(now, u)
            return
        while self.free > 0 and self.pending_bisect:
            self.start(now, self.pending_bisect.pop(0), False)
        size = 1 if self.strategy == "serial" else self.batch
        # A new top-level batch waits until earlier bisections are drained,
        # and (for batching) only one top-level batch is in flight at a time:
        # each batch is tested on top of the previous one.
        while self.free > 0 and self.queue and not self.pending_bisect:
            if self.strategy == "batch" and self.top_in_flight():
                break
            group = self.queue[:size]
            del self.queue[:size]
            self.start(now, group, True)

    def top_in_flight(self):
        return any(e[2] == "done" and e[3]["top"] for e in self.events)

    def finish(self, now, payload):
        units = payload["units"]
        if gate_passes(self.rng, units, self.flake):
            for u in units:
                self.land(now, u)
            return
        if len(units) == 1:
            if units[0]["bad"]:
                self.ejected_bad += 1
            else:
                self.ejected_good += 1
            return
        half = len(units) // 2
        self.pending_bisect += [units[:half], units[half:]]

    def land(self, now, u):
        self.landed.append(now - u["t"])
        if u["bad"]:
            self.bad_on_main += 1

    def report(self):
        hours = self.horizon
        lat = sorted(self.landed) or [0.0]
        return {
            "landed_per_h": len(self.landed) / hours,
            "median_h": statistics.median(lat),
            "p90_h": lat[int(0.9 * (len(lat) - 1))],
            "queue_end": len(self.queue),
            "good_ejected": self.ejected_good,
            "bad_on_main": self.bad_on_main,
        }


SCENARIOS = [
    # label, rate/h, p_bad, gate_min, runners, flake, strategy, batch
    ("today: serial, 30-min gate", 15, 0.10, 30, 2, 0.10, "serial", 1),
    ("today: yolo batches of 20", 15, 0.10, 30, 2, 0.10, "yolo", 20),
    ("batch+bisect, 30-min gate", 15, 0.10, 30, 2, 0.10, "batch", 8),
    ("batch+bisect, 30-min, clean oracle", 15, 0.10, 30, 2, 0.00, "batch", 8),
    ("batch+bisect, 8-min fast gate", 15, 0.10, 8, 2, 0.00, "batch", 8),
    ("+ pre-verified units (p_bad 3%)", 15, 0.03, 8, 2, 0.00, "batch", 8),
    ("peak 40/h, pre-verified, fast gate", 40, 0.03, 8, 2, 0.00, "batch", 12),
    ("peak 40/h, pre-verified, 30-min gate", 40, 0.03, 30, 2, 0.00, "batch", 16),
    ("peak 40/h, noisy oracle 10%", 40, 0.03, 8, 2, 0.10, "batch", 12),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--days", type=float, default=7.0)
    ap.add_argument("--trials", type=int, default=5)
    args = ap.parse_args()
    head = f"{'scenario':38} {'in/h':>5} {'out/h':>6} {'med h':>6} {'p90 h':>6} {'queue':>6} {'good-ej':>7} {'bad-main':>8}"
    print(head)
    print("-" * len(head))
    for label, rate, p_bad, gate, runners, flake, strat, batch in SCENARIOS:
        rows = []
        for i in range(args.trials):
            rng = random.Random(args.seed * 1000 + i)
            rows.append(Sim(rng, rate, p_bad, gate, runners, flake, strat, batch, args.days).run())
        avg = {k: statistics.mean(r[k] for r in rows) for k in rows[0]}
        print(
            f"{label:38} {rate:5} {avg['landed_per_h']:6.1f} {avg['median_h']:6.1f} {avg['p90_h']:6.1f}"
            f" {avg['queue_end']:6.0f} {avg['good_ejected']:7.0f} {avg['bad_on_main']:8.0f}"
        )


if __name__ == "__main__":
    main()
