# Vivi 13.1.0

A minor release for reading large mailspaces. The mailspace schema stays
at 8; nothing to migrate. Output formats are unchanged — the contents
outline is the only new text, and it heads `need show` and `want show`.

## A table of contents heads need and want show

Text `need show` and `want show` now open with a `Contents (N message(s))`
list: one line for the item, then one indented line per reply (date,
handle, subject). The outline reads message rows only, so it lists every
reply in the thread even when the message blocks stop at the 50-message
thread cap. The pump's planner captures `need show` from the top and
truncates it at 40,000 characters, so a Mind reading a planned need now
sees the full reply inventory even when later reply bodies are cut off.
`--json` and `--latest` are unchanged.

## Faster reads on large mailspaces

Measured on a 103k-message mailspace (best of three warm runs):

| Command | 13.0.1 | 13.1.0 |
|---|---|---|
| `want show` | 0.32s | 0.10s |
| `need show` | 0.44s | 0.11s |
| `want dump --status all` | 2.77s | 1.00s |
| `boot` | 1.29s | 0.53s |

- **One connection per mailspace.** Each `storage()` call used to open a
  new SQLite connection, re-running pragmas, permission checks, and a
  handle-index rebuild; a single `want show` opened three. One connection
  is now cached on the mailspace and shared by every caller in the
  command, and the handle index builds once.
- **Dump stops reading blobs to learn a kind.** `want/need/task dump`
  with `--status all` or `--status done` scans the done and sent roles,
  and kind matching read and MIME-parsed every one of those blobs. The
  send and done commands on an item's event log now decide the kind —
  newest deciding event wins, so a want promoted to a need stays a need —
  and only an undecided filter or a `--body` search reads the blob.
- **Thread loads only its own links.** Thread assembly and the show
  outline loaded the whole link table per call; a recursive query now
  fetches exactly the thread's connected component. Mailbox-wide loads
  remain only on the opt-in inference path.

`board`, `board --json`, `mailspace status`, and the list commands are
unchanged in speed and output.

## Compatibility

- Mailspace schema: unchanged (8).
- No new configuration. No output format changes beyond the contents
  outline described above.
- Items are classified for dump filtering by their event log before any
  blob header is consulted. Headers without an event log are not
  producible by Vivi; an imported message carrying an `X-Vivi-Kind`
  header but no Vivi events now classifies by role and log alone.
