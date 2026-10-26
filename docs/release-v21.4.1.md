# Vivi 21.4.1

Patch release on 21.4.0. Pairs with vivi-server 2.4.0.

## Fix: a new lane has `vivi/main` from the start

21.4.0 made a new target's main `vivi/main`, but a lane made by
`vivi project create` or `vivi lane add` only had the upstream branch until
the pump first ran. In that window, reading the lane failed with an unknown
revision: the server's `target_log` and `target_show` on a project that had
just been created.

A lane made with a seed commit or a source now points `vivi/main` at the tip of
its branch when it is made. A `vivi/main` branch that the source brought is
kept as it is. An empty lane (no seed, no source) has no commit to point at, so
the pump still makes the branch the first time it finds it missing.
