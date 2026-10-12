# Vivi 11.1.0

Vivi 11.1.0 embeds the agent skills (`vivi`, `tugboat`, `speedboat`) in the
binary, so an agent on any machine reads skill text that matches the CLI it
is running. The mailspace schema stays at 8. An existing `.vivi/` opens with
this binary.

## Skills

```sh
vivi skills list
vivi skills show <name> [--file <relative path>]
vivi skills install <dir> [--force]
```

`list` prints each skill's name, one-line description, and the binary
version (`vivi --version`). `--json` adds the supporting file paths.

`show` prints `SKILL.md` and lists every supporting file an agent can
request, because skills link each other by relative path. `--file` prints
one of those files.

`install` writes the same `vivi/`, `tugboat/`, `speedboat/` layout the
repository uses, so relative links keep working. `--force` overwrites.
No mailspace is required.

This replaces the README's symlink instructions.

## Upgrading

1. Install 11.1.0. No schema migration is needed.
2. The [11.0.0](release-v11.0.0.md) upgrade steps still apply if you have
   not taken them.
