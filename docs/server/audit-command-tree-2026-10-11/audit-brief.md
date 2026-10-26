# Vivi command-tree audit against the project server — shared brief

## The question
The vivi client now syncs a project mailspace with a project server (`vivi sync`):
letters (raw `.eml`, byte for byte) are pushed and pulled, filed by address. The
owner wants an end-to-end audit of the **whole vivi command tree** against the
server: for every command, **what works, what does not, and what silently
diverges** when the mailspace is a synced one. You audit one group of commands.

## Ground rules
- READ-ONLY on source. Do not edit, commit, or build anything under
  `/Users/ianzepp/work/ianzepp/vivi` or `/Users/ianzepp/work/ianzepp/vivi-server`.
  Both binaries are already built; use them as they are.
- Work only under your own WORKDIR (given below) and your own PORT. Never touch
  another agent's directory or port. Never contact any server except your own
  127.0.0.1 one. No network egress, no staging, no real credentials.
- Never run `cargo`, never `rm -rf` outside your WORKDIR, never wipe `/tmp`.
- Ground truth is the binary's behavior and its `--help`, then the source.
  Docs in the repos are stale by default: verify any claim by running it.

## Harness
`bash SCRATCH/audit-setup.sh WORKDIR PORT` boots a private server and two synced
checkouts and writes `WORKDIR/env.sh`. `source WORKDIR/env.sh` gives `$VIVI`,
`$A`, `$B`, `$PA`, `$PB`, `$BASE`, `$PID`, `$W`.

- A = alice, roles `mind` and `hand`, addresses `mind@$PA.local`, `hand@$PA.local`
- B = bob, roles `mind` and `hand`, addresses `mind@$PB.local`, `hand@$PB.local`
- Cross-mailspace addressing is by prefix: A writes to B with `--to hand@$PB.local`.
- Always pass `--project "$A"` or `--project "$B"` (never rely on cwd).
- Sync with `"$VIVI" sync --project "$A"` (push then pull). After a command
  that should propagate, sync the sender, then sync the receiver, and look.
- You may re-run the setup script (same WORKDIR and PORT) to get a clean world
  whenever a test needs a fresh start. `kill $(cat WORKDIR/server.pid)` when done.
- Look at both sides, and at the server when useful: records are at
  `GET $BASE/v1/projects/$PID/items` (needs a bearer; the harness does not mint
  one for you — prefer judging from the two checkouts and `vivi sync status`).
  Local state is in `<checkout>/.vivi/` (`mail.sqlite`, `blobs/`, `sync/state.json`).

## What to establish for EVERY command in your list
Exercise it for real in the synced world (a command with side effects: run it on
a mailspace that has held letters from the other side, not an empty one).

1. **Class** — one of:
   - `SYNCED-WRITE`: produces a letter that `vivi sync` pushes (need/task/want/
     mail send, reply, notes sent as mail, ...).
   - `SYNCED-READ`: reads letters, and must show both locally sent and pulled
     ones (list, show, thread, search, ...).
   - `LOCAL-ONLY`: changes or shows state that never leaves this checkout
     (marks, stars, closing a need, memos, rules, role edits, graph, ...).
   - `REMOTE`: talks to a server itself (sync, chat, pool, ...).
   - `N/A`: help, version, diagnostics with no mailspace meaning.
2. **Result** — one of:
   - `WORKS`: does what a user expects, in a synced mailspace, including with
     remote addresses and pulled letters.
   - `WORKS-LOCAL`: works, and being local-only is the sensible design.
   - `DIVERGES`: works locally but leaves the two sides disagreeing in a way a
     user would not expect and may not notice (e.g. closed here, still open
     there). Say exactly what disagrees and where.
   - `BROKEN`: errors, panics, wrong output, loses data, or mishandles a remote
     address / pulled letter. Give the exact command and output.
   - `UNTESTED`: could not be exercised (say why: needs a paid provider, a
     missing tool, an interactive TTY, a destructive effect outside WORKDIR).
3. **Evidence** — the exact command you ran (with `$VIVI`/`$A`/`$B` as in the
   harness) and the decisive line of output. Short. No transcripts.

Be adversarial, not just happy-path: remote vs local addresses, a letter pulled
from the other mailspace, a reply, a role that is renamed/unknown, an item the
other side already closed, running the command twice, running it before and
after a sync, and a second checkout of the same mailspace id. Exit code matters.

## Output
Write ONE file: `SCRATCH/audit-GROUP.md` (GROUP is yours), in this shape:

```
# Audit: <group>
## Summary
<3-6 lines: counts by Result, and the 3 most important findings>
## Matrix
| command | class | result | evidence |
| --- | --- | --- | --- |
## Findings
### F1 <title> — <BROKEN|DIVERGES>
- command / repro (exact, copy-pasteable with the harness env)
- observed vs expected
- likely cause (file and function, if you looked), severity (blocks use | surprises | cosmetic)
```

Then reply with a **receipt of at most 12 lines**: the file path, counts per
Result, and the findings' one-line titles. Do not paste the matrix in the reply.
A finding must be reproducible from your file alone.
