# Vivi 18.1.1

Patch release on 18.1.0. It fixes a pump loop that reopened needs and
started revision planners on its own.

## Fix: lifecycle letters no longer count as amendments

The pump's amendment check hashes each planned need's thread and plans a
revision when the hash moves. It skipped only the pump's own `pump: …`
replies, so vivi's lifecycle letters counted as new content: the letters
written by `need mark`/`unmark`, the note on `need done`, and `need reopen`.
Marking, unmarking or closing a need therefore reopened it and started a
revision planner. The pump's own reopen then wrote a `reopened` letter, which
triggered the next revision once the last one landed: a loop, each turn a
planner job on a worker and model credits. (Seen on hub, need 3ea8dee9,
want e6a485a8.)

- `need show --json` gives each message an `event` field: the letter's
  `X-Vivi-Event` value (`done`, `reopened`, `marked`, `unmarked`, …), or
  `null` for an ordinary letter. The change is additive.
- The amendment check ignores event letters. The thread hash scheme moves
  to `v4`. A hash stored by an older pump is re-baselined silently, so the
  upgrade itself starts no revisions. Audit receipts bound to an older hash
  go stale and are dispatched again.
- A withdrawn need stays withdrawn: a later thread change is logged and
  re-baselined, never reopened or revised. Replan clears the withdrawal.
- A need an operator closed in the mailspace is not reopened by the
  check. A need the pump itself closed after landing still reopens on a
  genuine operator reply, which is the intended feature.
