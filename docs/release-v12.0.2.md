# Vivi 12.0.2

A documentation patch for `vivi pump`. The mailspace schema stays at 8;
nothing to migrate.

## The vivi skill covers running the pump

- **Finding the skill.** The skill's description now names the pump, so an
  agent finds it for pump work.
- **Running the pump.** The Pump section gains a "Running the pump" part:
  - bring-up, with a paused first tick, a supervisor, and the restart rule
    (touch `STOP`, wait for the exit, remove `STOP`, start);
  - getting needs in, through a configured sender or the approval mark;
  - what a planner's proof commands must be;
  - reading `status.txt`, `log.txt` and `exceptions.txt`;
  - a checklist for when nothing is dispatching;
  - external gate runners and remote mergers;
  - rolling back.
- **Design records ship inside the skill.** They move from `docs/pump/` to
  `references/pump-contract.md` and `references/pump-protocols.md` in the
  skill. Links from an installed skill now resolve, and
  `vivi skills show vivi --file references/pump-contract.md` prints them.
- **Stale text updated.** The 12.0.1 overlap rule, the `withdrawn` state, and
  external gate intake in the tick are now described correctly.

## Upgrading

Install 12.0.2. To refresh an installed copy of the skill, run
`vivi skills install <dir> --force`.
