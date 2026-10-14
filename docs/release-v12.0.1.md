# Vivi 12.0.1

A patch release for `vivi pump`. The mailspace schema stays at 8; nothing to
migrate.

## Scope overlap matches globs segment by segment

The pump decides two things with one check: whether two units would write the
same files, and whether a unit touches a path in the reserved file. That check
cut a glob at its first wildcard and compared directory roots. A reserved
`crates/*/Cargo.toml` therefore became `crates`, and every unit anywhere under
`crates/` was held. On the first live run that was 312 of 361 pending units.

Two scopes now overlap only when some file could match both:

- `*`, `?` and `[...]` match within one path segment;
- `**` matches any number of segments;
- a glob with no `/` (such as `*.rs`) matches at any depth;
- a scope still covers everything under it, so `crates/faber` overlaps
  `crates/*/Cargo.toml`.

When both sides have a glob at the same position, the check assumes they meet.

## Upgrading

Install 12.0.1 and restart the pump.
