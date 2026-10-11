#!/usr/bin/env python3
"""Deterministic task-body gate for Tugboat Hand dispatch.

Validates a Hand task body against the Tugboat contract. Exit 0 = PASS
(dispatchable), 1 = FAIL (Hand refuses, see reasons).

A Hand assignment is capital. This gate keeps the bag a pointer to one
logical change so the seat can turn over. It refuses multi-family
done_when lists. Project-specific integration or suite gates belong in
that project's agents/charter docs, not here.

Usage:
    check-task-body.py BODY_FILE [--root ROOT] [--ledger LEDGER_JSON] [--strict-outcome]
    check-task-body.py BODY_FILE --record CLASS ACTUAL_CALLS ACTUAL_ELAPSED_S [--root ROOT] [--ledger LEDGER_JSON]

BODY_FILE is the task body as `vivi task show <handle>` prints it (key: value
lines). --root defaults to the current directory; the ledger defaults to
<root>/.tugboat/estimate-ledger.json.

Checks:
  1. Pointer present: (delivery) or (goal + unit); plus done_when and write_scope.
  2. If est_basis is present, it resolves: "pilot" or a ledger class.
  3. If sanity/validation is present: no workspace-wide / all-targets flags.
  4. Single logical change: done_when is one outcome, or the body records a
     cohesion/split_reason line. --strict-outcome also rejects semicolon lists.
  5. If est_work_tokens is present, it is a range; high end above 15k is
     treated as a mega-bag (refuse).

--record appends an actuals row to the ledger (optional closeout calibration).
"""

import argparse
import json
import os
import re
import sys

REQUIRED_FIELDS = [
    "done_when",
    "write_scope",
]

FORBIDDEN_FLAGS = ["--workspace", "--all-targets", "--all-features", "--all"]


def parse_body(text):
    """Parse key: value lines into a dict. Multiline values are joined."""
    fields = {}
    current = None
    for line in text.splitlines():
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(.*)$", line)
        if m:
            current = m.group(1)
            fields[current] = m.group(2).strip()
        elif current and line.strip():
            fields[current] += " " + line.strip()
    return fields


def load_ledger(path):
    if not os.path.exists(path):
        return {"classes": {}, "units": []}
    with open(path) as f:
        return json.load(f)


def save_ledger(path, ledger):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(ledger, f, indent=2, sort_keys=True)
        f.write("\n")


def check_sanity(text, label):
    """Optional sanity: no workspace-wide / all-targets flags."""
    problems = []
    if not text:
        return problems
    for flag in FORBIDDEN_FLAGS:
        if flag in text:
            problems.append(f"{label} uses forbidden flag {flag}")
    return problems


def count_outcomes(done_when):
    """Heuristic: 1 outcome if a single clause; N if enumerated (1) (a) 2)."""
    enumerated = re.findall(r"\([0-9a-z]\)|\b[0-9a-z]\)", done_when, flags=re.I)
    if enumerated:
        return len(enumerated)
    return 1


def token_high_end(text):
    """Return the high end of a 3-8k / 20-32k range as thousands, or None."""
    m = re.search(r"(\d+)\s*[-–]\s*(\d+)\s*k?\b", text, flags=re.I)
    if not m:
        return None
    hi = int(m.group(2))
    # bare numbers like 20000 vs 32k: if both > 100 treat as raw tokens / 1000
    lo = int(m.group(1))
    if hi >= 100 and lo >= 100:
        return hi / 1000.0
    return float(hi)


def validate(fields, ledger, strict_outcome=False):
    problems = []
    for field in REQUIRED_FIELDS:
        if field not in fields or not fields[field]:
            problems.append(f"missing required field: {field}")

    has_delivery = bool(fields.get("delivery"))
    has_goal_unit = bool(fields.get("goal")) and bool(fields.get("unit"))
    if not has_delivery and not has_goal_unit:
        problems.append(
            "missing pointer: need delivery: <path> or goal: <path> plus unit: <id>"
        )

    if "est_basis" in fields and fields["est_basis"].strip():
        basis = fields["est_basis"].strip()
        if basis == "pilot":
            pass
        elif basis not in ledger.get("classes", {}):
            problems.append(
                f"est_basis '{basis}' does not resolve in ledger classes "
                f"(available: {sorted(ledger.get('classes', {}).keys()) or 'none — seed the ledger or use pilot'})"
            )

    for label in ("sanity", "validation"):
        if label in fields:
            problems.extend(check_sanity(fields[label], label))

    if "done_when" in fields and "cohesion" not in fields and "split_reason" not in fields:
        n = count_outcomes(fields["done_when"])
        if n > 1 or (strict_outcome and ";" in fields["done_when"]):
            problems.append(
                f"done_when has {n} outcomes with no cohesion:/split_reason: — "
                "split into one-logical-change Hands"
            )

    if "est_work_tokens" in fields and fields["est_work_tokens"].strip():
        raw = fields["est_work_tokens"]
        if not re.search(r"\d+\s*[-–]\s*\d+", raw):
            problems.append(
                f"est_work_tokens '{raw}' is not a range like '3-8k'"
            )
        else:
            hi = token_high_end(raw)
            if hi is not None and hi > 15:
                problems.append(
                    f"est_work_tokens '{raw}' is a mega-bag (high end {hi}k > 15k) — "
                    "split so the Hand seat can turn over"
                )

    return problems


def record(ledger, cls, calls, elapsed, note=""):
    classes = ledger.setdefault("classes", {})
    classes[cls] = {
        "last_actual_tool_calls": int(calls),
        "last_actual_elapsed_s": int(elapsed),
        "last_note": note,
        "units": classes.get(cls, {}).get("units", []) + [int(calls)],
    }
    ledger.setdefault("units", []).append(
        {
            "class": cls,
            "actual_tool_calls": int(calls),
            "actual_elapsed_s": int(elapsed),
            "note": note,
        }
    )


def main():
    ap = argparse.ArgumentParser(description="Tugboat task-body gate")
    ap.add_argument("body_file")
    ap.add_argument("--root", default=os.getcwd())
    ap.add_argument("--ledger")
    ap.add_argument("--strict-outcome", action="store_true")
    ap.add_argument("--record", nargs=3, metavar=("CLASS", "CALLS", "ELAPSED_S"))
    args = ap.parse_args()

    ledger_path = args.ledger or os.path.join(args.root, ".tugboat", "estimate-ledger.json")
    ledger = load_ledger(ledger_path)

    if args.record:
        cls, calls, elapsed = args.record
        record(ledger, cls, calls, elapsed, note=args.body_file)
        save_ledger(ledger_path, ledger)
        print(f"recorded {cls}: {calls} calls / {elapsed}s -> {ledger_path}")
        return 0

    with open(args.body_file) as f:
        text = f.read()
    fields = parse_body(text)
    problems = validate(fields, ledger, strict_outcome=args.strict_outcome)

    if problems:
        print("FAIL — Hand refuses this task body:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print("PASS — task body satisfies the Tugboat contract.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
