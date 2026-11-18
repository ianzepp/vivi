#!/usr/bin/env python3
"""Bisect logic of `lander`, with compose and gate replaced by stubs (no git, no ssh).

  python3 test_lander.py
"""
import importlib.machinery
import importlib.util
import os
import re
import sys
import tempfile
import types

here = os.path.dirname(os.path.abspath(__file__))
loader = importlib.machinery.SourceFileLoader("lander_mod", os.path.join(here, "lander"))
spec = importlib.util.spec_from_loader("lander_mod", loader)
mod = importlib.util.module_from_spec(spec)
loader.exec_module(mod)


class Fake(mod.Lander):
    """bad: branches that fail alone; pairs: sets that fail only together; conflicts: always conflict."""

    def __init__(self, bad=(), pairs=(), conflicts=()):
        a = types.SimpleNamespace(out=tempfile.mkdtemp(prefix="lander-test-"), hosts="h1,h2", batch=8,
                                  gate_timeout=1)
        super().__init__(a)
        self.bad, self.pairs, self.conflicts = set(bad), [set(p) for p in pairs], set(conflicts)
        self.base = "main"
        self.base_fail = {"t::old"}
        self.gate_calls = 0

    def compose(self, base, branches, record):
        inc = [b for b in branches if b not in self.conflicts]
        if record:
            for b in branches:
                if b in self.conflicts:
                    self.ejected.setdefault(b, {"reason": "conflict", "files": ["x"]})
        sha = base + "+" + ",".join(inc) if inc else None
        return {"candidate": sha, "ref": None, "base": base, "excluded": [],
                "included": [{"branch": b} for b in inc]}

    def _gate(self, sha, want_host=None):
        self.gate_calls += 1
        members = set(re.split("[+,]", sha)[1:])
        fails = ["t::old"] + ["t::" + b for b in sorted(members & self.bad)]
        for p in self.pairs:
            if p <= members:
                fails.append("t::pair-" + "-".join(sorted(p)))
        return {"status": "ok", "failures": fails, "cached": False}


def run(queue, batch, **kw):
    f = Fake(**kw)
    for i in range(0, len(queue), batch):
        f.resolve(queue[i:i + batch])
    landed = sorted(b for l in f.landed for b in l["branches"])
    return f, landed


def check(name, got, want):
    if got != want:
        print("FAIL %s: got %r want %r" % (name, got, want))
        sys.exit(1)
    print("ok   %s" % name)


Q = ["b%d" % i for i in range(1, 9)]
f, landed = run(Q, 8)
check("clean batch lands whole in one gate", (landed, f.gate_calls), (sorted(Q), 1))

f, landed = run(Q, 8, bad=["b3"])
check("one bad branch is the only ejection", (sorted(f.ejected), landed), (["b3"], sorted(set(Q) - {"b3"})))
check("  its failing test is named", f.ejected["b3"]["new_failures"], ["t::b3"])

f, landed = run(Q, 8, bad=["b2", "b7"])
check("two bad branches", (sorted(f.ejected), landed), (["b2", "b7"], sorted(set(Q) - {"b2", "b7"})))

f, landed = run(Q, 8, bad=Q)
check("all bad", (sorted(f.ejected), landed), (sorted(Q), []))

f, landed = run(Q, 8, conflicts=["b4"])
check("conflict ejected without a gate failure", (sorted(f.ejected), f.ejected["b4"]["reason"]), (["b4"], "conflict"))

f, landed = run(Q, 8, pairs=[("b4", "b5")])
check("a pair that fails only together: one of them goes", (len(f.ejected), len(landed)), (1, 7))
check("  the second of the pair is the one ejected", sorted(f.ejected), ["b5"])

f, landed = run(Q, 8, pairs=[("b2", "b7")], bad=["b5"])
check("pair across halves plus a bad one", (sorted(f.ejected), len(landed)), (["b5", "b7"], 6))
print("all ok")
