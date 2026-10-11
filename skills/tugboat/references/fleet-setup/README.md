# Fleet setup — one fleet's charters, as a template

Tugboat's protocol is [`SKILL.md`](../../SKILL.md). It deliberately does not
name a project's seat roster, test ladder, or checkout isolation — those are
project law. This directory is one fleet's answer to that layer: 17 class-role
charters, kept as a template for a Mind standing up a new fleet.

Copy the **shape**, not the tokens. Every charter here has been stripped of the
machine paths, project names, repo names, and tooling names it was written
with; what identifies the project now reads as a placeholder (`<packet-tool>`,
`<goal-doc-path>`, `the project's install script`). Fill those in per project.

## Provenance

| | |
| --- | --- |
| Extracted | 2026-09-17, read-only |
| Source | the Vivi mailspace of a fleet running this protocol |
| Method | `vivi role charter show <role> --project <root>` |
| Files | 17 charters |

## The charter shape

Every charter follows the same three-part form, and that form is the most
transferable thing in this directory:

1. **A one-paragraph spawn pointer.** First line, no heading: what the seat is,
   its one job, and what it must not do. This is the text a Mind pastes into a
   spawn, and it is the whole opening prompt in the protocol's
   [handoff example](../../SKILL.md#handoff-example).
2. **`Standing law:`** — the durable rules the seat enforces on itself without
   asking. Self-enforcing law is why these charters work on small models: the
   seat does not need the Mind present to know what it may not do.
3. **Role sections.** Identity, job, do-not, report contract.

A charter describes the **frame, lens, and process** — what the seat is for, how
it judges, and how it works. It is not a log and not a status board. A correction
is folded into the body: keep the rule and drop the incident, the dates, the
handles, and the item lists. Anything that goes stale the moment a queue closes,
a lane merges, or a task settles does not belong here, because the next reader
has to maintain it and the seat has to keep re-deriving it. Provenance belongs in
the fleet's mail and git history.

## The roster

`Band` is the row from the protocol's model table. "Placeholders" names what the
charter leaves for the project to fill in.

| Charter | Seat | Band | Placeholders |
| --- | --- | --- | --- |
| `mind.md` | mind | `P1-S2` | Barely a charter (a few lines): it points at the protocol and names the packet tool. The Mind's law is `SKILL.md`. |
| `hand.md` | hand | `P2-S0` | Packet tool; the project's language skill and idiom-audit skill; the ladder environment |
| `planner.md` | planner | `P1-S2` | The goal-doc path convention; which ladder commands never go on a Hand |
| `auditor.md` | auditor | `P1-S1` | The language skill and idiom lens; the validation binary |
| `cadence.md` | cadence | `P4-S0` | The occupancy signal; the process catalog is that fleet's operations — keep `A tick`, `Citation discipline`, `Mail body`, `Refuse` |
| `head-ceo.md` | head-ceo | `P0-S2` | The map docs; the posture dial |
| `head-cto.md` | head-cto | `P0-S2` | The design standard named in its decision-routing section |
| `head-cxo.md` | head-cxo | `P0-S2` | The map docs |
| `head-cpo.md` | head-cpo | `P0-S2` | The map docs |
| `head-cmo.md` | head-cmo | `P0-S2` | The map docs |
| `head-cso.md` | head-cso | `P0-S2` | The product and infrastructure the threat model covers |
| `test.md` | test | `P2-S0` | The ladder: which stages are yours, the full-suite flag, the lane tooling |
| `lint.md` | lint | `P3-S0` | The static gate and lint commands; the early-ladder boundary |
| `merge.md` | merge | `P2-S0` | The integration branch, the consistency check, the merge order's dependency direction |
| `release.md` | release | `P2-S0` | The install script and release manifest. The "dispatch is the decision" law is portable as written |
| `docs.md` | docs | `P3-S0` | The docs-audit skill; the doc surfaces |
| `website.md` | website | `P3-S0` | The site, its generator, its toolchain precondition |

The six Heads are one role family with six lenses, and five of them carry a
byte-identical 190-line persona block (`ceo`, `cxo`, `cpo`, `cmo`, `cso`;
`head-cto` is that block plus 27 lines). Keep that block in **one** file in a new
fleet and have each Head charter reference it.

## Standing up a new fleet

1. **Create the class roles in Vivi**, one per seat, with the band from the
   protocol's model table. Class roles only — one mailspace reached 137 roles
   because every unit got a numbered instance, and the class charter is what
   replaced that.
2. **Adapt a charter per role**, keeping the three-part shape, and fill the
   placeholders: the packet tool, the ladder command, the lanes, the map docs.
3. **Factor the shared layer** — the Heads' persona block, and anything else two
   charters repeat (the merge-debt rule appears in `hand`, `lint`, and `test`).
4. **Spawn by pointer.** The spawn prompt is the charter's first paragraph plus
   one handle, never the whole file.
5. **Fold corrections into the body.** Keep the rule, drop the incident. A
   charter that accumulates dated amendments has stopped describing the seat and
   started recording one fleet's history, and the seat is left maintaining a log
   nobody wants.

## Known warts

- **Head duplication**, as above — ~950 duplicated lines across the six Heads.
- **Two rules are repeated** verbatim across the runner and integrator seats:
  the exit-discipline close-your-own-handle rule in `lint`, `merge`, and `test`,
  and the merge-debt rule in `hand`, `lint`, and `test`. A new fleet should
  write each once.
- **Amendment scaffolding survives in places**, dated and incident-specific: a
  kill list of closed handles, a named queue, a banned-item list. It is stale by
  construction. Fold the rule into the body, delete the scaffolding.
