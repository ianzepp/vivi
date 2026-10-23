# Manual bug triage

This is the SUGGESTED way to handle a bug detected outside the pump. The flow
is manual and outside the pump by design: code review and triage run outside
the pump because they catch different classes of failures than pump-run
review. No pump automation is part of this flow; that may come later if the
manual process proves successful.

Planners run before implementation; auditors run after implementation. A bug
in implemented code is an auditor job. The auditor identifies what is broken
and why, not which commit or agent caused it.

## 1. File a want

Write the report in `bug.md`, then file it as a want addressed to the Mind:

```sh
vivi want send --from <finder> --to mind --subject "bug: ..." \
  --question "What is the root cause, and what is the fix?" --body-file bug.md
```

A want must carry a `question`: without `--question` (or a `question` key in a
whole-letter `bug.md` that opens with `+++` front matter) the send is refused
and nothing is stored.

The body must include symptoms, exact reproduction steps and commands, what
happened versus what should have happened, the version, and relevant logs.
Keep the commands exact enough for another person to run. The `want` kind is
deliberate: the pump starts runs only for needs with an open pump record
(`src/pump/flow/intake.rs`), so it never picks up a want.

Mark the want so it can be found with other bug reports:

```sh
vivi want mark --for mind <handle> bug
```

Use the handle printed for the delivered want, not the sender's sent copy.

## 2. Audit the report

The mailspace auditor role runs outside the pump, either at the operator's
direction or through an orchestrator. Reproduce the issue, trace it through
the code to its root cause, and reply on the want thread:

```sh
vivi mail reply --from <auditor> <handle> --body-file report.md
```

The reply lands on the want thread. Add `--to mind` when the auditor's report
should also appear in the Mind's inbox. Keep findings focused on the behavior
and cause; do not assign blame to a commit or agent.

Use this report shape in `report.md`:

```text
Summary

Repro confirmed (yes/no + the command)

Root cause (files, functions, why)

Proposed fix

Regression test that would catch it

Risk and scope

Could not confirm
```

State what was directly confirmed and what remains uncertain. If reproduction
fails, record the exact command and result under “Could not confirm.”

## 3. Review the report

The operator reads the whole thread and reviews the finding:

```sh
vivi want show <handle>
```

If the report needs more investigation, reply on the same thread. The added
reply remains with the original report for review.

## 4. Promote accepted work

If the bug should jump the want queue, set its priority before promotion:

```sh
vivi want set-priority <handle> --for mind --priority P1 --reason "..."
```

This orders the want queue: `want list` sorts by priority, rank, and creation
time. It does not reorder the pump planner queue.

Send a need that serves the want after the operator accepts the report:

```sh
vivi need send --from mind --to mind --serves <handle> --subject "fix: ..." --done-when "the bug no longer reproduces"
```

Promotion carries the original bug and the auditor's reply into the need. The
pump planner reads `vivi need show <handle>` output: the whole thread, with the
original body and replies, truncated at 40,000 characters (`THREAD_CAP` in
`src/pump/flow/step_inputs.rs`). Ordinary `need show` prints up to 50 messages at depth 50.
Replies stay attached because threads link by message id and promotion moves
the same record from Wants to Needs. Planners see replies; no separate fix is
needed.

The pump plans a need only when it has an open pump record. A promoted need
is not dispatched until the Mind or operator sends one:

```sh
vivi pump send --need <handle>
```

The record is that dispatch; the pump posts its plan and attempt records on
it and closes it with the outcome (`landed`, `gave-up`, `withdrawn`,
`superseded`). `vivi pump history <handle>` lists a need's dispatches.

## 5. Close out without promotion

Invalid, duplicate, and won't-fix reports stay out of the need queue. Close
the want with a short reason:

```sh
vivi want done <handle> --for mind --note "<reason>"
vivi want drop <handle> --for mind --note "<reason>"
```

Use `done` for a resolved disposition and `drop` for an obsolete want. Record
the reason in the note so later readers can see why it was not promoted.
