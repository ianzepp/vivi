# Vivi 10.1.0

Vivi 10.1.0 makes the commands that read the work graph affordable on a full
mailbox. `vivi step` and `vivi trace` both re-derived shared facts once per
item: every handle resolution rescanned the active message ids, and `trace`
rebuilt its inferred-link graph by comparing every content against every other
content, twice. On the faberlang mailspace (78,408 messages, 40,718 contents,
325 ready nodes) that cost `step` 30.8 seconds and `trace` 216. Both now read
from an index built once.

The release also fixes a `vivi boot` false alarm on multi-digit goal counts, and
adds the work-graph flow reference the skill ships.

Nothing here changes the mailspace format or the CLI surface. The one output
that moves is the `trace` tie-break named below, where 10.0.0 disagreed with
itself; `step`, `board`, `need list`, `task list`, `graph ready`, `graph audit`,
and `role status` produce byte-identical output.

## `vivi step`

`step` adjudicates the ready frontier of the `backlog` graph, and it resolved
one handle per ready node. Each resolution ran a full scan of the active
message ids — the handle cache covered decoration, not token resolution — so the
cost was ready nodes × mailbox size: 325 scans of 78,408 rows on the faberlang
mailspace.

Handle resolution now reads a per-connection index, built on first use and
cleared by any write. An ambiguous handle is still reported as ambiguous rather
than guessed, and a handle that matches nothing still falls through to the id
and content-prefix paths.

## `vivi trace`

`trace` had three separate costs:

- The inferred-parent pass compared every content against every other content to
  find the newest one whose handle appeared in the child's body, and then
  compared them again by reply-stripped subject and participants, allocating a
  participant set per comparison.
- The handles in `task from` event notes went through the token resolver, whose
  miss path scans the `messages` table with a `LIKE`. Every handle in the
  faberlang notes is stale, so every one of them took that path.
- The event log was read twice, and every body was read and parsed twice.

Every body is now read once, citations are found by matching eight-character
handle windows in the body, thread candidates come from a bucket keyed by
reply-stripped subject and participants, and event handles resolve from the same
index.

Two consequences worth naming:

- **Citation ties are deterministic.** A body citing two contents stamped in the
  same second used to resolve to whichever candidate the map happened to visit
  last, so repeated runs of the same trace disagreed with each other — one seed
  returned 167 or 168 nodes at random. Ties now resolve by content id.
- **Handles that are not eight lowercase hex characters** keep the exact
  substring match. The window scan is the same test for the fixed-width shape
  every local handle has.

## Fixes

### `vivi boot` misread multi-digit completion claims

`completion_claim` collected the numerator by walking backwards from the `/` and
handed the reversed digit run to a most-significant-first parser, so any
two-digit numerator transposed: `24/26` was read as 42. The denominator was
collected forwards, which is why only one side was wrong, and the only fixture
was single-digit.

Observed live: `vivi boot` printed `MISMATCH: Status claims 42; register holds 24
done` against a goal file that was correct. The fix reads the digit run
forwards, and the fixture now covers two- and three-digit counts.

## Performance

Both mailspaces measured with the released binary, before and after:

| Mailspace | Command | 10.0.0 | 10.1.0 |
| --- | --- | --- | --- |
| faberlang — 78,408 messages, 325 ready nodes | `vivi step` | 30.8 s | 0.3 s |
| | `vivi trace <handle>` | 216 s | 2.0 s |
| mintedgeek — 11,550 messages | `vivi step` | 0.33 s | 0.04 s |
| | `vivi trace <handle>` | 4.5 s | 0.3 s |

The remaining `trace` cost is one pass over every content body, because the
inferred citation links are rebuilt per run. `vivi graph ready` and `vivi graph
audit` were already index reads and are unchanged (0.03 s and 0.5 s on
faberlang).

## Documentation

`skills/vivi/references/work-graph-flow.md` is the end-to-end procedure a
coordinating seat follows: the node model, `--depends-on` validation, the merge
task as the "on main" gate, the body format `vivi step` accepts and every
exception it can raise, the per-unit dispatch order, a command reference with
the exact `graph audit --repair` behavior, fan-out and chain examples, and known
gaps. `skills/vivi/SKILL.md` points at it.
