# Vivi 13.3.0

A minor release for watching and debugging the pump while it runs.

## `vivi pump log`

`vivi pump log` prints the project's pump log without needing to know where
it lives. It shows the last 50 lines (`-n`), follows new lines (`-f`), and
filters by merge lane (`--lane 2` for `default/2`), by unit or need handle as
a whole token (`--unit`), or by text (`--grep`); filters combine.
`--exceptions` reads the exceptions file instead. A missing log exits 1 and
names the path.

## Live merge-lane progress

Every line a merge lane writes starts with `lane <target>/<n>: `, and each
step is logged as it happens instead of only when the batch ends: each unit
merge (`merge <uid> 3/8 ok`), lane clone creation, formatting and the style
commit, the start and end of every shared check and unit proof, and each
publish step (fetch, merge of a moved tip, push attempt, pushed tip).
`status.txt` ends with one line per lane: idle, busy (batch size, batch age,
current step and its age), in backoff, or gone.

## Compatibility

- Pump database schema: unchanged (5).
- Mailspace schema: unchanged (8).
- `status.txt` gains one line per merge lane after the stall lines.
