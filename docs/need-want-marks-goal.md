# Goal: Marks on needs and wants

**Status**: pending — 0/3 delivered

## Summary

A need or a want can be marked with a free slug, such as `bug`, when it is
filed or any time after. The mark is an append-only event on that item. The
message bytes stay as they were sent. `mark` and `unmark` are the commands.
The same pair exists on `need` and on `want`.

## Problem

Needs and wants are the work items, and they read a lot like GitHub issues,
but the only stored kind is the lifecycle kind: `need`, `want`, or `task`.
Nothing records whether an item is a bug, a regression, or anything else.
Subject and body are free prose. `want set-priority` can store a lane after
the fact, and that lane is a local metadata upsert, last write per key wins.
It is not a classification, and it does not travel.

The mailspace already refuses the IMAP way of fixing this. A mark is not a
flag rewritten on the message. Phase 02 records closure as an event and
forbids edits that mutate. This goal is the local form of that rule for
classification. Syncing those events through Vivi Server is a later goal.
Phase 03 is still a draft, and `mailspace_events.event_id` is a store-local
integer, so this goal does not pretend the events already replicate.

## Commands

```sh
vivi need mark <handle> <slug>... --for <identity>
vivi need unmark <handle> <slug>... --for <identity>
vivi want mark <handle> <slug>... --for <identity>
vivi want unmark <handle> <slug>... --for <identity>
```

`--project` is optional on all four, as it is on `done`. `--for` is required
on `unmark` as well as `mark`. The slug is positional and repeatable. Each
slug is its own event. One invocation that names two slugs writes two events
in one transaction, or writes neither if the command fails.

```sh
vivi need mark abcd1234 bug regression --for mind
vivi need unmark abcd1234 bug --for mind
```

Receipts, one line per slug:

```text
marked abcd1234 bug
abcd1234 already marked bug
unmarked abcd1234 bug
abcd1234 already unmarked bug
```

A repeat that does not change the folded set appends nothing.

## Event

No new table. Each change appends one row to `mailspace_events` on the
item's message id:

| Column | Value |
| --- | --- |
| `command` | `need mark`, `need unmark`, `want mark`, or `want unmark` |
| `event_type` | `marked` or `unmarked` |
| `actor_identity` | the `--for` identity |
| `note` | the slug and nothing else |
| `subject` | the item's subject, as other events already store it |

The folded set for an item is one entry per slug. Walk the item's events in
log order (`occurred_at`, then `event_id`). The latest `marked` or
`unmarked` for that slug wins. A slug with no such event is absent. Two
`marked` rows for the same slug still fold to present, so a duplicated row
is harmless. The command itself checks the fold first and skips the append
when the slug is already in the requested state.

`note` is the slug because the event row has no payload column. Mark events
do not carry a freeform reason. A reason is mail.

The authority is the log. Marks are not written to `mailspace_item_metadata`.
That table stays the want priority map it is now.

## Rules

- The slug uses the role-label character set: letters, numbers, dot, dash,
  underscore. Comparison is exact and case-sensitive. `bug` and `Bug` are
  different marks. An empty slug or any other character fails before a write,
  with `invalid mark '<value>'; use letters, numbers, dot, dash, or underscore`.
- At least one slug is required.
- `--for` must be the identity that holds the item. The failure is the
  existing cross-identity error, which names the holder.
- An absorbed item refuses `mark` and `unmark`, with the same absorbed error
  the other mutations use. Marks already on it still read.
- Marks survive `want promote`, `done`, `reopen`, and `drop`. Promotion keeps
  the message id, and the events hang off that id.
- Marks do not change graph readiness, node kind, `vivi step`, `need bind`,
  or want priority and lane.
- An unmarked item has no type. Absence of `bug` does not mean `feature`.
  `feature` is just another slug when someone marks it.

## Read

`need show` and `want show` print one line when the folded set is non-empty,
slugs sorted:

```text
Marks: bug regression
```

An item with no marks omits the line. `show --json` adds a `marks` array on
the thread message, `[]` when empty, sorted the same way.

`need list` and `want list` take a repeatable `--mark`. An item matches when
it carries every named slug. The human list columns stay as they are. JSON
list records gain a `marks` array, same contents as show. `--mark` composes
with the filters those lists already have, including `--status`.

```sh
vivi need list --for mind --mark bug
vivi need list --for mind --mark bug --mark regression
```

## Register

| Unit | Status |
| --- | --- |
| mark-commands | pending |
| mark-read | pending |
| mark-docs | pending |

`mark-commands` is the four commands, the event append, the fold, the
receipts, the holder check, the absorb refusal, the slug check, and CLI
tests for those paths. `mark-read` is the show line, the show JSON array,
the list filter, the list JSON array, and tests that a mark survives promote
and that `step` and `bind` ignore it. `mark-docs` is `skills/vivi/SKILL.md`
and the needs-and-wants example in `README.md`.

## Non-goals

- Tasks. The same verbs can be added later. This goal does not.
- Role labels, goal display labels, graph labels, and chat seat labels.
- A closed `bug` / `feature` field, or a new work kind.
- Vivi Server, chat pull, and any traveling id for the event. Another
  checkout does not see a mark made here.
- Changing want priority, rank, repo, lane, or `set-priority`.
- A note or reason flag on `mark`.
- Board rendering of marks.
