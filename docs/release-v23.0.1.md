# Vivi 23.0.1

Patch release on 23.0.0. It brings in the work that was running on the hub
or landed on `vivi/main` but was not on `main` when 23.0.0 was cut, so a host
moving to 23 loses nothing it already had. Upgrading a host from 22.x follows
the 23.0.0 notes.

## Mail

- A reply to an item only its author holds (a self-addressed need or want,
  or a task the pump sent to itself) goes back to the author and stays on the
  thread, where it was refused before for want of another recipient. The pump
  still names its recipient with `--to`.

## Pool result receipts

- One splitter reads a pool result letter's sections (facts, receipt, stderr
  tail, runner log tail) exactly as the renderer writes them: a heading counts
  only as a whole `--- <title>` line in forward order, so a receipt that
  quotes Markdown headings or a lookalike `--- receipt` line stays whole.
- The hand, repair, commit-fallback and budget-retry readers share the
  summary's boundaries instead of each splitting the letter its own way;
  rung failures and stderr evidence read the shared sections.
- Heading-rich hand and slicer receipts are covered through the fan-out,
  `vivi task show`, the CTO brief and the attempt record.
