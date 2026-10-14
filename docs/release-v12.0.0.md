# Vivi 12.0.0

Vivi 12.0.0 adds `vivi pump`, which works through a backlog of needs without
an operator in the loop, and makes a need's state travel between the members
of a synced project. It also adds the MCP bridge, role-based pool dispatch,
and new sync verbs for admission and API keys. The mailspace schema stays at 8.
An existing `.vivi/` opens with this binary.

This is a major release because synced projects change behaviour: done, reopen,
absorb, marks and binds now travel as event letters. Upgrade every member of a
synced project together (see Upgrading).

Vivi is now MIT licensed; the repository carries a `LICENSE` file.

## vivi pump

`vivi pump run | once | status` runs a project's backlog as a loop:

1. It plans open needs: from configured senders, or any open need carrying the
   approval mark (`pump` by default).
2. It dispatches the planned units to pools, picking the pool from each unit's
   kind.
3. It merges delivered branches into an integration branch in batches, running
   the configured checks.
4. It gates the integration branch periodically. New failures go to a gate-fix
   planner, and merging never stops for a red gate.
5. It fast-forwards `main` when a gate shows no new failure and no owned
   failure is still red, so `main` never gets redder.

The pump is project-neutral. Everything project-specific is in the
`[pump]` tables of `mailspace.toml`:

- **Commands:** the planner, format, shared checks and gate commands.
- **Pools and unit kinds:** `[pump.dispatch.kinds.<name>]`, each with a
  `pool`, an optional `max` and an optional `skip_proof`.
- **Model lanes.**
- **Committer identity:** `committer_name` and `committer_email` are required.
  Every pump commit uses them.
- **Hand rules file.**
- **Reserved-paths file:** paths another session holds.
- **External gate results:** `results_dir`, `results_wait_seconds` and
  `ignore_file`.

The config is validated at start. Validation refuses a gate with no command,
promotion with no shared checks, a missing committer, an unreadable hand-rules
file and an undefined default kind.

Operators steer the pump with files in its home: `STOP`, `PAUSE`,
`HOLD-MERGE`, `FORCE-GATE`, `REMOTE-MERGE` and `skip.txt`. A second pump on the
same home refuses to start. The design record is in
[`docs/pump/contract.md`](pump/contract.md) and
[`docs/pump/protocols.md`](pump/protocols.md), and the config reference is in
the `vivi` skill under "Pump".

## Need state travels through sync

Done, reopen, absorb, marks and binds are now sent as mail-kind letters with an
`X-Vivi-Event` header.

- **Same result on every member.** Each member applies the events in the same
  order: by instant, then causal depth, then Message-ID. When two members
  disagree, the newest event on the need wins. Events dated more than 10
  minutes in the future wait until the clock catches up.
- **Role checks on receipt.** The receiving side checks the sender's role. An
  operator-kind role may accept, close and reopen any need.
- **Absorb waits.** Absorb happens only after a quiet period:
  `[absorb] quiet_hours`, default 24.
- **Addressed sending.** `[sync] send = "addressed"` sends only letters
  addressed outside the mailspace.
- **Held needs.** `vivi need list --held` lists held needs.

## Sync

- **Admission:** `vivi sync key`, `create` and `vouch`. Governance letters from
  the postmaster are filed under no role. Refusals name the verb that would
  fix them.
- **API keys:** `vivi sync api-key mint | list | revoke`; `mint --out` writes
  the key to a file.
- **Push options:**
  - `push --since <age|date>` sends only letters dated in a window;
  - `push --drain` waits out the server's rate limit;
  - a rate-limited push now exits 0.
- **Keep-local:** `vivi sync keep-local` keeps chosen letters off the server.
- **Server epoch:** a server epoch on list responses resets a pull cleanly
  after a server reset.
- **Message-IDs:** a Message-ID's right-hand side is the mailspace address
  domain.

## MCP bridge

`vivi mcp` serves a project's mailspace to an MCP client, using a project API
key. A bridge pool gives each dispatch its own key. The key is delivered only
to the runner, revoked when the dispatch is collected, and swept at
`destroy --all`. The `{mcp_config}` harness placeholder points the worker at a
generated MCP config file.

## Pools

- **Role-based dispatch.** `--model` is optional. The addressee's role picks a
  band (P0 to P5), the band picks the model and reasoning level, and the role's
  charter becomes the standing prompt. Read-only roles dispatch read-only. The
  `{reasoning}` harness placeholder carries the level.
- **Extra refs.** `--with` and `with:` lines deliver extra refs to the runner.
  The runner fetches them, deepens to the merge base, and can publish a merge
  made by the agent.
- **Fetch depth.** A configurable fetch depth, default 50.
- **Restore paths.** A `restore` setting lists paths the runner restores before
  it commits.
- **Short hashes.** `--from` accepts an abbreviated hash.
- **Fly root disk.** `[pools.<name>.fly] rootfs_gb` sets the size of the Fly
  root disk.

## Smaller changes

- `task`, `need` and `want show --latest` prints only the newest reply.
- `send` prints a node line naming the backlog node.
- `done` prints one line when it completes a bound need.
- Vivi exits quietly when stdout closes.
- The README is rewritten against the actual command surface.
- The skill's committer examples use a placeholder identity.

## Upgrading

1. Install 12.0.0 on every member of a synced project. A member still on 11.x
   does not apply need events, so its board will drift from the others until
   it upgrades.
2. No schema migration is needed.
3. If you run `vivi pump`, add a `[pump]` section. The skill's Pump section has
   the reference and a two-kind example.
