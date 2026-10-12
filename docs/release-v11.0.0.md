# Vivi 11.0.0

Vivi 11.0.0 makes a project one shared mailspace across checkouts. `vivi sync`
pushes and pulls letters, byte for byte, through a project on `vivi-server`.
Close and reopen travel as letters too, so every checkout agrees on what is
done. A worker's `done` is now a report; the Mind or the author accepts it.
Pools can be named, and rules are shared by every seat.

The mailspace schema stays at 8. An existing `.vivi/` opens with this binary.
Syncing needs `vivi-server` 1.0.0 or later.

## Breaking changes

- **`[sync]` is smaller.** `recipient` and `[sync.names]` are gone, and a
  leftover key is ignored. Push takes the recipients from each letter's own To
  and Cc. `vivi sync join` registers the mailspace id.
- **Addresses carry the mailspace id.** Every mailspace has a UUID id in its
  `mailspace.toml`. The first 8 characters are the domain of every address it
  mints (`role@3f9a1c7e.local`). Letters written under the old name domains
  still resolve.
- **`vivi chat` is removed.** It shipped in 10.2.0. `vivi sync` is the
  cross-checkout surface. A `[chat]` table in `mailspace.toml` is ignored, and
  `vivi board` no longer ends with an unread-chat line. The sync key stays at
  `.vivi/chat/identity.ed25519`, so names already registered keep working.
- **`[pool]` is rejected.** A mailspace holds pools as `[pools.<name>]` tables,
  each written out in full. The error names the new form.
- **A worker's `done` is a report.** Only the item's author, or a role of kind
  `mind`, closes work for everyone. Any other role's `done` moves its own copy
  and completes nothing (see Close, reopen, and report).

## Project sync

```sh
vivi sync                      # push, then pull
vivi sync push | pull | status | directory | join
vivi sync pull --since 7d      # first pull only: start at a window
vivi sync pull --refs [--depth N]
vivi sync fetch <handle>...
vivi sync resync
vivi sync prune --older-than 30d [--dry-run]
```

A letter is the exact `.eml` file. It keeps its bytes, its content hash and
its id on every checkout. The server stores letters and never rewrites one.
Pull files each letter under the roles its To and Cc addresses name, checks
the envelope against the letter, and refuses a From whose prefix the server
did not stamp for its sender.

A mailspace works with no `[sync]` and no server. Its state is a function of
the letters it holds. Sync only moves letters.

A pull reads pages of 500 and saves its cursor after each page, so an
interrupted pull resumes. `--since` starts a fresh mailspace at a window and
records a floor, which `sync status` shows. `fetch` looks letters up by their
eight-character handle. `--refs` follows `X-Vivi-Depends-On` and handles named
in a subject or body. `prune` evicts old bound letters locally and never
touches open work, starred or absorbed items, or letters that never travelled;
`resync`, `fetch` or `--refs` restores them byte for byte.

Syncs on one checkout take a lock under `.vivi/sync/`; a second waits ten
seconds, then exits 1. A corrupt `sync/state.json` is rebuilt by `sync resync`.
A mailspace id that disagrees with its `.vivi/mailspace.id` copy is refused
by every command, naming both files. `.vivi/` ignores itself.

## Work that is not yours

- Work held for another mailspace's role mints no backlog node. `step`,
  `graph ready` and `boot` leave it out.
- Your copy of work you handed to another mailspace stays a tracking node and
  is never dispatched. `step` reports it as `awaiting_remote` naming the
  recipient, `graph ready` has an `awaiting` line, and its dependents stay
  blocked.
- A letter for a role this mailspace lacks is held without a node, counted in
  the pull summary, `sync status` and `boot`. `role add`, `role rename` and
  `mailspace identity add|rename` adopt held letters and say how many.
- `X-Vivi-Depends-On` becomes a prerequisite edge on every filing route. A
  dependency not yet held is remembered and linked when it arrives.
- `graph audit` gains `foreign_held`, `missing_edge` and `event_state`;
  `--repair` fixes them.

## Close, reopen, and report

Closing and reopening are letters, not local flags. An event is a small reply
on the item with `X-Vivi-Event: done|reopened`, `X-Vivi-Event-Of` and
`X-Vivi-Event-Prev`. Every holder of the item applies it. The state of an item
is the head of its event chain, not a server order. `show` prints `State:`
once an event exists.

The author, or any role of kind `mind` in the mailspace that authored the
item, closes and reopens it for everyone. A command prints
`done <handle>; event <id>`. Any other role reports and prints
`done <handle>; reported`.

A worker's `done` is a report, and it completes nothing. It moves the
reporter's own copy and nothing else: the backlog node stays open, its
dependents stay blocked, and a bound need's join does not fire. The author,
the holder of the sent copy, or a role of kind `mind` accepts with `done`
on any handle of the item. The acceptance completes the node, unlocks
dependents, and fires the join, which closes the need and writes its event
when this mailspace authored it. A mind in the assignee's mailspace accepts
locally; its close is still a report to the author.

```sh
vivi task done <handle> --for <worker> --note "<receipt>"   # report
vivi task done <handle> --for <mind>                        # accept
vivi task reopen <handle> --for <mind> --note "<what failed>"
```

Reported work is visible. `vivi step` lists it as `awaiting_acceptance`,
naming who reported and the command that accepts, and never as a dispatch.
`vivi graph ready` has a `reported` line and a `reported=N` count.
`vivi graph activate` refuses it, `vivi step --apply` completes nothing for
it, and `vivi graph audit` does not call it drift. `vivi graph complete`
remains the manual override.

## Undeliverable letters

With `[sync]` configured, a letter over 64 KiB is refused at compose. A
letter whose From claims another mailspace is never uploaded: `sync push` exits
non-zero and lists it, and `sync status` shows a `not uploaded` line. A push
that hits the rate limit stops, says how many letters are left, exits non-zero,
and resumes on the next run. `mail deliver` accepts remote recipients. `watch`
wakes for letters that arrive by sync.

## Pools

`[pools.<name>]` tables define any number of pools. `--pool` selects one and
may be omitted when only one exists. `vivi pool scale --pool <name> <N>` grows
a pool by creating and seeding slots and shrinks it by destroying free ones.
Providers are `fly`, `docker`, `ssh`, `local` (worktree slots) and `runpod`.
`dispatch --read-only` publishes nothing. `doctor --test` runs a live self-test with an
embedded repository. Porcelain lines gain a pool field.

## Rules and skills

`vivi rule` saves project rules shared by every seat: `save`, `list`, `show`,
`search`, `supersede`, `retire`, `absorb`.

`skills/` ships three agent skills: `vivi` (the CLI), and `tugboat` and
`speedboat` (two operating protocols over a Vivi backlog). Each depends only
on the vivi skill.

## Upgrading

1. Install 11.0.0. No schema migration is needed.
2. Remove `recipient` and `[sync.names]` from `[sync]`; they are ignored now.
3. Rewrite a `[pool]` table as `[pools.<name>]`.
4. Run `vivi graph audit` once per mailspace. `--repair` mints missing nodes
   and edges for work sent by older binaries.
5. If you used `vivi chat` (10.2.0), move to `vivi sync`. Nothing under
   `.vivi/chat/` needs to go: the key there is the sync key. The `[chat]`
   table can be deleted; it is dropped the next time vivi rewrites the file.
