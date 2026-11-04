#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Check one lab run against its scenario's [expect] table.

    expect.py <plan.toml> <run dir>

[expect] keys (each optional):
  landed = N                   how many needs must have landed
  exceptions_present = [...]   exception kinds that must appear (pump/exceptions.txt)
  exceptions_absent = [...]    exception kinds that must not appear
  steps_present = [...]        step node prefixes that must appear (steps.txt)
  briefs_contain = {role = "text"}   some logged brief of that role holds the text
  mail_present = [...]         regexes some decoded mail body must match
  mail_absent = [...]          regexes no decoded mail body may match

Prints one PASS or FAIL line per check and a final verdict; exits 1 on any FAIL.
"""
import email
import re
import sys
import tomllib
from email import policy
from pathlib import Path


def mail_bodies(run):
    for path in (run / "blobs").rglob("*.eml"):
        try:
            msg = email.message_from_bytes(path.read_bytes(), policy=policy.default)
        except Exception:  # an unreadable blob is not a lab verdict
            continue
        parts = msg.walk() if msg.is_multipart() else [msg]
        for part in parts:
            if part.get_content_maintype() == "text":
                try:
                    yield part.get_content()
                except Exception:
                    continue


def briefs(run, role):
    for path in (run / "agents").rglob(f"*-{role}.brief.md"):
        yield path.read_text(errors="replace")


def exception_kinds(run):
    text = (run / "pump" / "exceptions.txt")
    kinds = set()
    if text.exists():
        for line in text.read_text(errors="replace").splitlines():
            parts = line.split()
            if len(parts) >= 2:
                kinds.add(parts[1])
    return kinds


def main():
    plan = tomllib.load(open(sys.argv[1], "rb"))
    run = Path(sys.argv[2])
    want = plan.get("expect", {})
    results = []

    def check(ok, what):
        results.append(ok)
        print(("PASS " if ok else "FAIL ") + what)

    if "landed" in want:
        lines = (run / "needs.txt").read_text().split("\n") if (run / "needs.txt").exists() else []
        landed = sum(1 for line in lines if line.endswith(" landed"))
        check(landed == want["landed"], f"landed {landed}, expected {want['landed']}")
    kinds = exception_kinds(run)
    for kind in want.get("exceptions_present", []):
        check(kind in kinds, f"exception {kind} raised")
    for kind in want.get("exceptions_absent", []):
        check(kind not in kinds, f"exception {kind} not raised")
    steps = (run / "steps.txt").read_text() if (run / "steps.txt").exists() else ""
    nodes = {line.split("|")[1] for line in steps.splitlines() if line.count("|") >= 3}
    for prefix in want.get("steps_present", []):
        check(any(n.startswith(prefix) for n in nodes), f"a step {prefix}* ran")
    for role, text in want.get("briefs_contain", {}).items():
        check(any(text in b for b in briefs(run, role)), f"a {role} brief contains {text!r}")
    bodies = list(mail_bodies(run)) if ("mail_present" in want or "mail_absent" in want) else []
    for pattern in want.get("mail_present", []):
        check(any(re.search(pattern, b) for b in bodies), f"mail matches {pattern!r}")
    for pattern in want.get("mail_absent", []):
        check(not any(re.search(pattern, b) for b in bodies), f"no mail matches {pattern!r}")
    verdict = all(results)
    print(f"VERDICT {'PASS' if verdict else 'FAIL'} ({sum(results)}/{len(results)})")
    return 0 if verdict else 1


if __name__ == "__main__":
    sys.exit(main())
