# Vivi 12.0.4

A patch release for `vivi pump`. The mailspace schema stays at 8; nothing to
migrate.

## Leftover failures of a finished fix plan are planned again

When a gate shows new failures, the pump hands them to a gate-fix plan. When
every unit of that plan had settled but some of its failures were still red,
the pump raised `red-after-fix` once and kept the failures owned by the
finished plan. Nothing planned them again, so promotion was held for good. On
the live pump that was 448 failures under one plan.

At the next gate, those failures are now released and planned again at the
gate's tip, together with any new failures. They go under the gate's fix need,
or `<need>-r<k>` when that id is taken. Promotion stays held while they are
owned. `[pump.gate] max_replans` (default 2) caps how many times one failure is
planned again; after that, `red-after-fix` is raised for a person, as before.
Set it to 0 to keep the old behaviour.

## priority.txt

`[pump] priority_file` (default `priority.txt` in the pump home) lists need
handles, one per line, and is re-read every tick. Units of those needs:

- dispatch first, ahead of fix units and then everything else;
- merge first;
- pass `HOLD-MERGE`;
- get the same write-scope lock bypass as fix units, so feature work in flight
  does not hold them back.

## Upgrading

Install 12.0.4 and restart the pump.
