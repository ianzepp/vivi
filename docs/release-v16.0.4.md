# Vivi 16.0.4

Patch on 16.0.3.

## `vivi pump withdraw`: close a need that has no mail thread

A pump need could be closed only through the mail board
(`reconcile_done_needs` reads `vivi need list --status done`). The repair
needs the Vivi 15 pump created (`pumpfix-g<tip>` for gate fixes,
`pumpfix-q<id>` for quarantine repairs) have no mail thread: they exist only
in `pump.db` and the plan files. Under 16 their need branches sit frozen, the
need-audit dispatch fails on every tick ("cannot read full need thread ...
message not found"), and their stuck units linger.

```
vivi pump withdraw <NEED>... --reason <TEXT>
```

`--reason` is required. Like `vivi pump requeue`, the command only records a
request (`withdraw <need> requested; the pump applies it at its next tick`);
the running pump owns `pump.db` state and applies the request at its next
tick, right after requeue requests. Applying it:

1. withdraws every unfinished unit of the need, **stuck units included**
   (merged, no-defect and replaced units are left alone);
2. adds the need to the closed set, so nothing re-plans, dispatches or merges
   it;
3. makes the need-audit, repair re-audit, need-merge/freeze and landing steps
   skip it, and retires a finished audit for it;
4. drops the need-branch record. Branch cleanup jobs need a verified landing,
   which a withdrawn need never has, so the remote `vivi/need/<id>` branch is
   left for manual deletion and the log names it;
5. logs `withdrew need <need>: N unit(s) (<reason>)` and records a
   `need-withdrawn` exception.

An unknown need id is dropped with a log line and does not stop the tick. The
command works for any need. Withdrawing a real need also leaves its mail item
open (the log says so): close that on the board.

Upgrade note: after upgrading, withdraw the leftover repair needs, for example
`vivi pump withdraw pumpfix-g1a2b3c4 pumpfix-q7 --reason "15-era repair need, no mail thread"`.
`vivi pump log --exceptions` lists the needs whose `need-audit-dispatch`
exceptions repeat.

## Fallback audit requirements can be echoed

16.0.3 audited a need without `done_when:` lines against its messages by making
each message's text (up to 4000 characters) one requirement. A receipt must
repeat each requirement's text exactly, and model auditors do not reproduce a
long multi-line body byte for byte, so every such audit was rejected ("need
audit changed the text of requirement ..."). The exact-text check stays. The
fallback requirement is now one short line per source message:
`Deliver what need message <handle> asks for (full text in the original need thread).`
The auditor already receives the thread as `original_need_thread`.

## Job briefs stay under the server's letter cap

A revision planner task for a need with a long thread and many units was
about 123 KB, over the 64 KiB letter cap of the project server, so the send
failed ("letter is 123091 bytes, over the 65536-byte cap") and was retried
silently every tick.

- The revision planner prompt now carries at most 44,000 bytes of input JSON.
  The need thread is cut first (ending with
  `[truncated; full thread via `vivi need show <need>` or in <path>]`), and
  unit briefs are shortened when the units alone take over half the budget. The
  full input stays in `<pump home>/needs/<need>.json`.
- Every pump job brief is checked just before it is sent. A brief over the cap
  keeps its head and its last 4 KiB (the output contract of a prompt) around a
  marker, is saved in full as `<pump home>/job-briefs/<job>.full.txt`, and
  records a `job-brief-truncated` exception, so a cut is never silent.

## Known issue

A need audit that runs out its worker budget never reports a result. The
`[pump.dispatch] budget` (default 600 s) is also the budget of need-audit
jobs; an auditor that needs longer exits 124 and the pump records
`need-audit-result ... has no successful worker exit` on each retry. Raise
`[pump.dispatch] budget` for now; a separate audit budget is not in this
release.
