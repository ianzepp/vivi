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

BODY_FILE is the task body: a letter that opens with TOML front matter
between two `+++` lines, then free prose. done_when and write_scope live in
the front matter as lists, one done_when entry per outcome:

    +++
    done_when = ["cargo test --lib x passes"]
    write_scope = ["src/x.rs"]
    +++

    goal: docs/goal.md
    unit: U-1

The prose below the fence keeps `key: value` lines (goal, unit, delivery,
sanity, validation, est_basis, est_work_tokens, cohesion, split_reason).
A body with no front matter is an old-format letter and FAILs (vivi step
reports letter_invalid). Front matter is parsed with tomllib (Python 3.11+);
older Pythons use a small reader that accepts only `key = "text"`,
`key = 'text'` and arrays of those strings, and FAILs on anything else.

--root defaults to the current directory; the ledger defaults to
<root>/.tugboat/estimate-ledger.json.

Checks:
  1. Front matter present; done_when and write_scope are non-empty lists of
     non-empty strings.
  2. Pointer present: (delivery) or (goal + unit).
  3. If est_basis is present, it resolves: "pilot" or a ledger class.
  4. If sanity/validation is present: no workspace-wide / all-targets flags.
  5. Single logical change: done_when has one entry, or the prose records a
     cohesion/split_reason line. --strict-outcome also rejects semicolon lists.
  6. If est_work_tokens is present, it is a range; high end above 15k is
     treated as a mega-bag (refuse).

--record appends an actuals row to the ledger (optional closeout calibration).
"""

import argparse
import json
import os
import re
import sys

FENCE = "+++"
REQUIRED_LISTS = ["done_when", "write_scope"]
NO_FRONT = (
    "task body has no front matter: it is an old-format letter "
    "(vivi step reports letter_invalid)"
)

FORBIDDEN_FLAGS = ["--workspace", "--all-targets", "--all-features", "--all"]


class FrontError(Exception):
    pass


def split_front(text):
    """Split at the fence like src/letter.rs split_pieces: (front, prose)."""
    lines = text.splitlines(keepends=True)
    i = 0
    while i < len(lines) and not lines[i].strip():
        i += 1
    if i >= len(lines) or lines[i].strip() != FENCE:
        raise FrontError(NO_FRONT)
    for j in range(i + 1, len(lines)):
        if lines[j].strip() == FENCE:
            return "".join(lines[i + 1 : j]), "".join(lines[j + 1 :])
    raise FrontError("task body front matter is not closed: no second +++ line")


_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", '"': '"', "\\": "\\"}


def parse_front_fallback(front):
    """Minimal TOML reader: key = "text" | 'text' | [strings, ...]; # comments."""
    data = {}
    lines = front.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        i += 1
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_-]*)\s*=\s*(.*)$", line)
        if not m:
            raise FrontError(f"unsupported front matter line: {stripped}")
        key, rest = m.group(1), m.group(2)
        if key in data:
            raise FrontError(f"duplicate front matter key: {key}")
        # Gather a multi-line array until its closing bracket.
        if rest.lstrip().startswith("["):
            buf = rest
            while not array_closed(buf):
                if i >= len(lines):
                    raise FrontError(f"unclosed array for key {key}")
                buf += "\n" + lines[i]
                i += 1
            data[key] = parse_array(buf, stripped)
        else:
            value, tail = parse_string(rest.lstrip(), stripped)
            check_tail(tail, stripped)
            data[key] = value
    return data


def parse_string(src, ctx):
    """Parse one quoted string at the start of src; return (value, remainder)."""
    if not src or src[0] not in "\"'":
        raise FrontError(f"unsupported front matter line: {ctx}")
    quote = src[0]
    out = []
    k = 1
    while k < len(src):
        c = src[k]
        if c == quote:
            return "".join(out), src[k + 1 :]
        if quote == '"' and c == "\\":
            k += 1
            if k >= len(src) or src[k] not in _ESCAPES:
                raise FrontError(f"unsupported escape in front matter line: {ctx}")
            out.append(_ESCAPES[src[k]])
        else:
            out.append(c)
        k += 1
    raise FrontError(f"unterminated string in front matter line: {ctx}")


def check_tail(tail, ctx):
    t = tail.strip()
    if t and not t.startswith("#"):
        raise FrontError(f"unsupported front matter line: {ctx}")


def array_closed(buf):
    """True once the `[` that opens buf has its `]` outside any string."""
    quote = None
    k = 0
    while k < len(buf):
        c = buf[k]
        if quote:
            if quote == '"' and c == "\\":
                k += 1
            elif c == quote:
                quote = None
        elif c in "\"'":
            quote = c
        elif c == "#":
            while k < len(buf) and buf[k] != "\n":
                k += 1
        elif c == "]":
            return True
        k += 1
    return False


def parse_array(buf, ctx):
    src = buf.lstrip()[1:]
    items = []
    while True:
        src = skip_space(src)
        if not src:
            raise FrontError(f"unclosed array in front matter line: {ctx}")
        if src[0] == "]":
            check_tail(src[1:], ctx)
            return items
        value, src = parse_string(src, ctx)
        items.append(value)
        src = skip_space(src)
        if src.startswith(","):
            src = src[1:]
        elif not src.startswith("]"):
            raise FrontError(f"unsupported front matter line: {ctx}")


def skip_space(src):
    while src:
        if src[0].isspace():
            src = src[1:]
        elif src[0] == "#":
            nl = src.find("\n")
            src = "" if nl < 0 else src[nl + 1 :]
        else:
            break
    return src


def parse_front(front):
    try:
        import tomllib
    except ImportError:
        return parse_front_fallback(front)
    try:
        return tomllib.loads(front)
    except tomllib.TOMLDecodeError as e:
        raise FrontError(f"front matter is not valid TOML: {e}")


def parse_body(text):
    """Parse the prose part's key: value lines into a dict. Multiline values are joined."""
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


def validate(front, fields, ledger, strict_outcome=False):
    problems = []
    for field in REQUIRED_LISTS:
        value = front.get(field)
        if not isinstance(value, list) or not value:
            problems.append(f"front matter needs {field} = [...] (a non-empty list)")
        elif not all(isinstance(v, str) and v.strip() for v in value):
            problems.append(f"front matter {field} must hold only non-empty strings")

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

    done_when = front.get("done_when")
    if isinstance(done_when, list) and "cohesion" not in fields and "split_reason" not in fields:
        n = len(done_when)
        semi = any(isinstance(v, str) and ";" in v for v in done_when)
        if n > 1 or (strict_outcome and semi):
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
    try:
        front_text, prose = split_front(text)
        front = parse_front(front_text)
    except FrontError as e:
        print("FAIL — Hand refuses this task body:")
        print(f"  - {e}")
        return 1
    fields = parse_body(prose)
    problems = validate(front, fields, ledger, strict_outcome=args.strict_outcome)

    if problems:
        print("FAIL — Hand refuses this task body:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print("PASS — task body satisfies the Tugboat contract.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
