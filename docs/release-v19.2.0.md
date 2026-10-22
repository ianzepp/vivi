# Vivi 19.2.0

Minor release on 19.1.5: dependency edits after send, plan checks at ingest,
and revisions and planners that see everything they need.

## Dependency edits after send

`vivi need|task|want depend <handle> --add <h>… --remove <h>…` changes an
item's dependencies after it was sent. Each change is a `dependency-added` or
`dependency-removed` event letter (one `X-Vivi-Event-Dep` header per
dependency) that every member applies, like marks and binds. Per dependency
the newest letter by (instant, Message-ID) controls and wins over the
send-time `X-Vivi-Depends-On` header, so removing an original edge works and
members converge whatever the arrival order. Every route that builds edges
(attach, pull, settle, `graph audit --repair`) folds through the same state,
so a removed edge never comes back. Only the item's author, a mind or an
operator may change dependencies. `need show` lists them with their state.

Use case: a release-tracking need whose depends-on set is the release scope;
`vivi graph` reports it ready when every dependency is done.

vivi-server 1.11.0 adds the matching MCP `depend` tool for hosted projects.

## Plan checks at ingest

The pump checks every plan before minting units and re-asks the planner with
the findings when one fails:

- Bad-plan reasons are always logged in full (`bad-plan <unit>: <reason>`).
- `[[pump.plan.proof_rules]]` (new, optional): a unit whose write scope
  matches `scope` (glob) needs a proof command matching `proof` (regex);
  `reason` is shown to the planner. `vivi config validate` checks the rules.
- Proofs that cannot fail are rejected: a unit whose only proofs are negated
  searches, or a `cargo test` filter found in neither the brief nor the
  unit's write-scope files.
- A first plan that waits on needs that have not landed is held
  (`waits on <needs>`, recorded as `waits_on`) and re-planned when they land.
- A merged unit is never dispatched again; a unit claiming `merged` that the
  pump has not merged, or duplicating a merged unit, is rejected.
- Write-scope overlaps with live units of other needs are logged
  (`overlap-warn`), and overlapping units are never started in the same tick.
- A rejected first plan is re-asked twice with the findings (60 s, 120 s),
  then held with the findings as its reason. Revisions use the existing
  re-ask budget and carry the findings too.

## Revisions and planners

- A revision request carries the whole prior plan with full briefs, each unit
  labeled `merged`, `in-flight`, `pending`, `withdrawn` or `replaced` and
  whether it may be amended, plus the thread messages new since the last
  plan. Only the thread gives way to the size budget; a plan too large to
  send is refused with a `revision-input` exception rather than cut.
- Planner jobs get every configured target as a read-only extra checkout
  (`vivi pool dispatch --also-target <id>`), at `$WORK/targets/<key>/repo`,
  named in the brief.
- The pool receipt cap rises from 64 KiB to 256 KiB, so a revision plan that
  echoes full briefs keeps its end marker.

## Upgrading

Move vivi-server to 1.11.0 with this release to get the MCP `depend` tool.
