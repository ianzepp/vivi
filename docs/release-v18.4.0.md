# Vivi 18.4.0

Minor release on 18.3.0. It adds `vivi project create`: one step from
nothing to a registered, pumped project with its own git repo.

## `vivi project create`

```sh
vivi project create <name> --projects-root P --lanes-root L --git-url-prefix U \
  [--pool studio] [--gate "cargo test"] [--branch main] \
  [--no-pump] [--no-serve] [--initial-commit] [--json]
```

In order, it:

1. checks the name (lowercase letters, digits, `-._`, at most 64 characters)
   and refuses an existing repo, project root or registration;
2. makes a group-shared bare repo at `L/<name>.git`
   (`git init --bare --shared=group`), so a git-shell user and the pump user
   that share a group can both write it;
3. inits the mailspace at `P/<name>` with the starter `[pump]`; config from a
   base mailspace above `P` (`base = true`) is inherited;
4. writes the project's own values: `[pump] branch`, the pool's `remote_url`
   (`U<name>.git`, for a pool a base defines) and the gate command;
5. optionally makes one empty commit on the branch (`--initial-commit`),
   authored by the effective `[pump]` committer or `vivi <vivi@localhost>`;
6. registers the project in the host registry with its pump and serve flags.

If any step fails, every earlier step this call made is undone; anything
that already existed is never touched. A project that is still missing
items registers anyway and holds until they are filled, and the result says
which items are missing.

The library entry point is `vivi::project_create::create` (or `create_at`
with an explicit registry file). It prints nothing and returns the project
root, repo path, git URL, mailspace id, initial commit and readiness.
vivi-server's host `create_project` tool calls it.

`mailspace init`'s setup (roles, starter `[pump]`) is now
`Mailspace::init_with_defaults`, shared by the CLI and project create.
