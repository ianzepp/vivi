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

## 4. Send a need that serves the want

If the bug should jump the want queue, set its priority first:

```sh
vivi want set-priority <handle> --for mind --priority P1 --reason "..."
```

This orders the want queue: `want list` sorts by priority, rank, and creation
time. It does not reorder the pump planner queue.

Send a need that serves the want after the operator accepts the report:

```sh
vivi need send --from mind --to mind --serves <handle> --subject "fix: ..." --done-when "the bug no longer reproduces" --body-file report.md
```

The serving need is a new letter with its own thread. The pump planner reads
`vivi need show <handle>` output: that thread only, truncated at 40,000
characters (`THREAD_CAP` in `src/pump/flow/step_inputs.rs`). Put the repro, the
root cause and the proposed fix in the need body, and name the want handle so
a reader can open the discussion. The want's state follows its serving needs:
it stays open while one is open and is done once all are closed.

The pump plans a need only when it has an open pump record. The serving need
is not dispatched until the Mind or operator sends one:

```sh
vivi pump send --need <handle>
```

The record is that dispatch; the pump posts its plan and attempt records on
it and closes it with the outcome (`landed`, `withdrawn`, `superseded`; a
locked run keeps its record open). `vivi pump history <handle>` lists a need's dispatches.

## 5. Close a want no need will serve

Invalid, duplicate, and won't-fix reports stay out of the need queue. Close
the want with a short reason:

```sh
vivi want done <handle> --for mind --note "<reason>"
vivi want drop <handle> --for mind --note "<reason>"
```

Use `done` for a resolved disposition and `drop` for an obsolete want. A
`done` by hand counts only while no need serves the want. Record the reason in
the note so later readers can see why no need was sent.
