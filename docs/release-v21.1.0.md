# Vivi 21.1.0

Minor release on 21.0.0, delivered with wave 3 phase 2. The v1 letter
format is now complete — every letter vivi writes carries its facts in
front matter — and sync receive stops silently filing letters it cannot
read. Pairs with vivi-server 2.1.0.

## The letter format completes (wave 3 B4)

- Every writer composes through the gate: done/reopened events carry
  `verdict`/`branch`/`commit`/`repos` as close front matter; report
  notes carry the same facts under a `[close]` table; pool result notes
  carry `[pool] {event, record}`; memos carry `tags`; rules carry
  `scope` and `supersedes`; property letters and task-from letters are
  format-1 with an empty front. An empty front stores prose as written —
  no `+++` fence.
- The seven content headers (`X-Vivi-Verdict/-Repo/-Branch/-Commit/
  -Tag/-Rule-Scope/-Supersedes`) are gone from every writer. A format-1
  letter that carries one is REFUSED at parse (`of_message` names the
  header); legacy letters stay readable as history with their headers
  ignored.
- Vouch letters (`vivi sync vouch`) are format-1 mail — vivi 21.0.0
  wrote them legacy, which a vivi-server 2.1.0 refuses. The join flow
  needs both sides at these versions.

## Sync receive: quarantine, never silent (wave 3 B5)

- A synced format-1 letter that will not parse (an unknown front key
  from a newer member, a broken fence) is held at
  `.vivi/quarantine/<sha256>.eml` with a `.json` receipt, is not filed,
  and `vivi sync pull` exits non-zero naming it. A second pull is
  silent. The same content arriving again through a fresh window is
  reported again — the sender must learn.
- `vivi sync quarantine list | retry <id> | drop <id>` clears held
  letters; `vivi doctor` shows the held count; the pump raises
  `sync-letter-quarantined` exactly once per content id.

## Split-ownership lanes (wave 3 E3)

- `vivi project lane add <project-root> <target-id> --lanes-root R
  [--git-url-prefix P] [--seed]`: one command that does every half the
  caller can write (bare lane repo, clone into the target path, guarded
  `[pump.targets.<id>]` write) and prints the exact command for the
  other owner's half. `vivi project create` behaves the same way on a
  host whose lanes folder belongs to another user. On Linux the lane
  repo copies group and ACL from an existing sibling (best effort, with
  a warning off Linux).

## Library surface (wave 3 D1/D4)

- `vivi::pump::config::{show, validate, migrate}`,
  `vivi::pool::config::{list_view, show_view}`,
  `vivi::pump::flow::view`, the pump action entry points
  (`requeue`, `withdraw`, `replan`, `answer`, `unlock`),
  `vivi::pump::logview::view`, and
  `vivi::pool::config_allow::{validate_allow, set_allow}` — the
  value-returning surface vivi-server 2.1.0's MCP tools call; the CLI
  commands are printers over the same functions.

## Upgrade note

- Move with vivi-server 2.1.0 (its durable append now refuses legacy
  letters; its hosted push refuses unreadable format-1 letters with the
  parser's reason). Old letters stay history everywhere; no schema
  changed.
