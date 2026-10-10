# Vivi 10.2.0

Vivi 10.2.0 adds a chat client and marks on needs and wants. `vivi chat`
connects two checkouts through a room on `vivi-server`. A mark such as `bug`
is an append-only event on a need or a want.

The mailspace schema stays at 8. An existing `.vivi/` opens with this binary.
Local sends still mint store-local ids. Marks stay in the mailspace that
wrote them.

## Chat

`vivi chat` is a client. This binary does not host rooms. The server is the
sibling repository `vivi-server`.

```sh
vivi chat room create [--seats N] [--label <seat>]
vivi chat join <room> [--label <seat>]
vivi chat rooms
vivi chat send <room> [--to <label>] [--subject <s>] (--body <text> | --body-file <path>)
                 [--kind need|task|want] [--reply-to <handle|id>] [--ref <handle|id>]...
vivi chat read [room] [--mark-read] [--all]
vivi chat watch [room] [--once]
vivi chat flush
vivi chat pull [room]
vivi chat resync [room]
vivi chat prune --older-than <age> [--dry-run] [room]
vivi chat leave <room>
vivi chat status
```

Settings are the `[chat]` table in the project's `.vivi/mailspace.toml`:

```toml
[chat]
server = "https://vivi-server-production.up.railway.app"
notify = true
name = "pharos/vivi"
recipient = "mind"
```

Until `server` is set, the client uses the placeholder
`https://vivi-chat.example`. The host this project deploys answers at
`https://vivi-server-production.up.railway.app`. `name` overrides the seat
label; otherwise the label is `<short-host>/<project>`. `recipient` is the
local role that receives items on `pull` and `resync`. Without it, pull
refuses and chat stays print-only.

One ED25519 keypair per mailspace is created on first use at
`.vivi/chat/identity.ed25519` (mode 0600). The chat directory is mode 0700
and writes its own `.gitignore`, so the key, cursors, and outbox stay
uncommitted. `join` and `status` print the key fingerprint.

`send` writes the outbox, then tries to deliver. Delivery prints
`sent <id> to <room>`. A failure prints `queued <id> for <room>`. The next
chat command flushes the outbox, and so does `vivi chat flush`. Each send
mints one id, `msg_` plus 24 lowercase hex characters, and every retry sends
that same id. The server uses it as the idempotency key.

`leave` drops the room from this checkout. The seat on the server stays.

## Work items across checkouts

`send --kind need|task|want` sends a work item. `--reply-to` and repeatable
`--ref` thread it. Each accepts a local handle or a full `msg_` id, and a
full id is sent even when this mailspace has not seen it. The receipt prints
`handle` followed by the eight-character prefix the other checkout will show.

`vivi chat pull` files what arrived since that room's sync cursor. Needs,
tasks, and wants land in those folders and mint a backlog node the way a
local send does. Plain chat lands in `inbox`. This seat's own sends come
back as `sent` copies. Each item is stored under the id it was minted with,
so the short handle matches on every checkout that pulled it. Items
addressed to another seat are counted and not filed. A wire id that is not
`msg_` plus 24 hex characters, which is how messages sent before this
release look, is filed under a store-local id, and the summary says so.

Pull does not reopen a need you closed. Its cursor is separate from
`read --mark-read`. If the room sends an id this mailspace already holds
with different bytes, that item is refused, named on stderr, and the rest
of the pull continues.

## Prune and resync

The room is the log. The mailspace holds a replica. `vivi chat prune
--older-than 30d` evicts room items dated before the cutoff and drops their
blobs. Ages also take `12h` and `90m`. `--dry-run` prints the same selection
and writes nothing. Nothing prunes on its own. Prune never touches an open
need, task, or want, a starred or absorbed item, or mail created locally.
It does not need the server or `recipient`.

`vivi chat resync` replays the room from cursor zero through the same ingest
as pull. Evicted items come back under the same ids. Local state stays as it
was: a closed need stays closed, and a read flag stays read. A later `pull`
resumes at its cursor and does not restore what prune removed.

## Board

When a room is joined, `notify` is on (the default), and the server answers,
`vivi board` appends one line:

```text
chat: 2 unread in <room> (last: "<snippet>")
```

or `chat: no new messages` when the probe finishes and nothing is unread.
The probe does not advance the read cursor, and it spends at most 1.5
seconds. A missing `[chat]` table, `notify = false`, no joined room, or an
unreachable server leaves the board as it was. `vivi board --json` never
includes the line.

## Marks

A need or a want can carry free slugs. The mark is an event on the item.
The message bytes stay as they were sent.

```sh
vivi need mark <handle> bug regression --for <identity>
vivi need unmark <handle> bug --for <identity>
vivi want mark <handle> bug --for <identity>
vivi want unmark <handle> bug --for <identity>
```

Each slug is its own event, written in one transaction. A repeat that does
not change the folded set appends nothing and prints `already marked` or
`already unmarked`. The slug is letters, numbers, dot, dash, and underscore,
compared exactly, so `bug` and `Bug` are different marks.

`show` prints `Marks: bug regression` when the folded set is non-empty,
sorted. `show --json` adds a `marks` array, `[]` when empty. `need list` and
`want list` take a repeatable `--mark` and keep an item only when it carries
every named slug. The human list columns stay as they are. JSON list records
gain the same `marks` array. `task list --json` gains it too, because tasks
share that printer. This release has no mark commands for tasks, so that
array stays empty unless mark events already exist on the item.

Marks survive promote, done, reopen, and drop. They do not change graph
readiness, `vivi step`, or `need bind`. They are not stored as want
metadata, and `chat pull` does not carry them. An absorbed item refuses a
new mark and still shows the ones it has. An item with no `bug` mark has no
type. `feature` is just another slug.

## Documentation

`skills/vivi/SKILL.md` describes chat and marks. `docs/need-want-marks-goal.md`
is the marks contract. The chat delivery records are
`docs/factory/vivi-chat-phase-01-delivery.md` and
`docs/factory/vivi-chat-phase-02-delivery.md`.
