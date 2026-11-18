# -*- coding: utf-8 -*-
"""Measure bytes written to disk per vivi mailspace operation as the store grows.

Linux only. A reaped child's I/O counters are added to its parent, so the
difference in this process's /proc/self/io `write_bytes` across one
subprocess.run is what that command caused the block layer to write
(dirtied pages), excluding /dev/null and pipes.

usage: python3 vivi_write_audit.py <workdir> [sizes...]
"""
import json
import os
import subprocess
import sys
import time

WORK = sys.argv[1]
SIZES = [int(s) for s in sys.argv[2:]] or [100, 1000, 3000]
PROJ = os.path.join(WORK, "proj")
BODY = os.path.join(WORK, "body.md")
VIVI = "vivi"


def io():
    out = {}
    with open("/proc/self/io") as f:
        for line in f:
            k, v = line.split(":")
            out[k] = int(v)
    return out


def run(*args, check=True):
    before = io()
    t = time.time()
    r = subprocess.run([VIVI, *args, "--project", PROJ], stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, text=True)
    dt = time.time() - t
    after = io()
    if check and r.returncode != 0:
        sys.exit(f"vivi {' '.join(args)} failed: {r.stderr.strip()[:400]}")
    return r, after["write_bytes"] - before["write_bytes"], dt


def size_of_store():
    vd = os.path.join(PROJ, ".vivi")
    total = files = 0
    for root, _, names in os.walk(vd):
        for n in names:
            files += 1
            total += os.path.getsize(os.path.join(root, n))
    db = os.path.join(vd, "mail.sqlite")
    return {"store_mb": round(total / 1e6, 2), "files": files,
            "db_mb": round(os.path.getsize(db) / 1e6, 2) if os.path.exists(db) else 0}


def handle_of(stdout):
    for tok in stdout.replace(",", " ").split():
        t = tok.strip("()[]`'\":")
        if len(t) == 8 and all(c in "0123456789abcdef" for c in t):
            return t
    return None


def main():
    os.makedirs(PROJ, exist_ok=True)
    with open(BODY, "w") as f:
        f.write(("A need body of about a kilobyte. " * 32) + "\n")
    if not os.path.isdir(os.path.join(PROJ, ".vivi")):
        run("mailspace", "init")
    sent = 0
    results = []
    send_wb = []
    for target in SIZES:
        t0 = time.time()
        while sent < target:
            r, wb, _ = run("need", "send", "--from", "operator", "--to", "operator",
                           "--subject", f"audit need {sent}", "--body-file", BODY,
                           "--done-when", "the audit says so")
            send_wb.append(wb)
            sent += 1
        grow_s = time.time() - t0
        last = send_wb[-20:]
        row = {"needs": sent, **size_of_store(), "grow_s": round(grow_s, 1),
               "send_kb_last20_mean": round(sum(last) / len(last) / 1024, 1)}
        r, wb, dt = run("need", "send", "--from", "operator", "--to", "operator",
                        "--subject", "probe", "--body-file", BODY,
                        "--done-when", "the audit says so")
        h = handle_of(r.stdout)
        row["send_kb"] = round(wb / 1024, 1)
        row["send_s"] = round(dt, 2)
        for label, args in [
            ("mark", ["need", "mark", h, "probe-mark", "--for", "operator"]),
            ("done", ["need", "done", h, "--for", "operator"]),
            ("board", ["board"]),
            ("list", ["need", "list", "--for", "operator"]),
            ("status", ["mailspace", "status"]),
        ]:
            r2, wb2, dt2 = run(*args, check=False)
            row[f"{label}_kb"] = round(wb2 / 1024, 1)
            row[f"{label}_s"] = round(dt2, 2)
            if r2.returncode != 0:
                row[f"{label}_err"] = r2.stderr.strip()[:120]
        results.append(row)
        print(json.dumps(row), flush=True)


if __name__ == "__main__":
    main()
