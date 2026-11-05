# Vivi 23.0.2

Patch release on 23.0.1, from the hub's first hours on 23.

## Fixes

- **The server logs again.** The default log filter named the modules as
  they were before the crate split (`vivi::server`, `vivi::pump`), so a 23
  server printed no info line at all: no home, no projects, no listening
  line. The default is now
  `vivi_server=info,vivi_core::pump=info,tower_http=info`. A `log` key in
  `config.toml` still overrides it.
- **A main rung on a closed task no longer stalls its target.** Each target's
  main rung runs on one long-lived task. When that task had been closed, its
  queued job was refused every tick ("dispatch needs an open task") and,
  being in flight, kept the target from ever being scheduled again, so no
  main rung (and no repair flow) followed any land. A queued main rung job
  whose task is closed is now dropped, and a closed task is replaced by a new
  one.
- **The hand contract states the check results.** `[[check]]` `result` is
  exactly `pass`, `fail` or `skipped`, with detail in the prose. A hand that
  wrote `pass (24 tests OK)` was sent back for an envelope fix, which cost a
  whole job.
