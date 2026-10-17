# Vivi 13.1.2

A patch release for the pump board and the work lists. The mailspace schema
stays at 8 and the pump database schema at 2; nothing to migrate.

## The board plans only the mind's needs

A `[pump.board] from` sender used to make every open need a candidate,
whatever its recipient. The sender path now carries `--to <reply_from>`, so
the pump plans open needs addressed to its mind. A need from the operator to
another role is not its work. The approval-mark path keeps its existing
narrowing to held needs and needs addressed to `reply_from`.

## A thread reply is not a new open item

`need list`, `want list`, the task lists, held lists, `boot`, and the board
now show a thread once, by the message that started it. Any message carrying
a reply-to parent link is dropped from the listing, whatever route stored it:
a `need send --reply-to` or `want send --reply-to`, a pulled reply, or a
captured reply. `show` and `thread` still read the full thread.

On the faberlang mailspace this removed 8 reply rows from
`need list --status open --to mind`, including `068e6e26` (which the operator
had added to `skip.txt` by hand), and the `Re:` replies from `want list
--to mind`.

## Compatibility

- Mailspace schema: unchanged (8). Pump database schema: unchanged (2).
- List output changes by design: replies no longer appear as their own open
  or done items. Read them with `show` on the thread root.
- A work send with `--reply-to` still mints a backlog graph node, so
  `vivi step` can still surface such a reply as a graph node. The pump board
  and the work lists ignore it.
