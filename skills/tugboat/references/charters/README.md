# Tugboat charters

Standing charters for the Tugboat seats, written to be installed as Vivi role
charters. The protocol is [`../../SKILL.md`](../../SKILL.md); these files are the
per-seat standing law the protocol's [Standing charters](../../SKILL.md#standing-charters-spawn--unit-resume)
section says a seat loads on spawn.

Every command and field a charter names is part of the `vivi` CLI in this
repository. A charter refers to no outside skill, tool, or host. Where a
project must supply something (the gate commands, the integration branch, the
install procedure), the charter says "the task names it" or "the project's
rules name it", and the Mind puts it in the task body or in a project rule
(`vivi rule`).

## Installing

A charter is the role's standing prompt. One role per class, never one per
unit:

```bash
ROOT=/path/to/project
C=skills/tugboat/references/charters   # in the vivi checkout

# The model field is a band (see the roster); that is Tugboat policy. Vivi
# also accepts a model slug listed in the pool's harness.models.
vivi role add hand --project "$ROOT" --kind hand --model P2-S0
vivi role charter set hand --project "$ROOT" --body-file "$C/hand.md"

# A Head is its lens file followed by the shared block.
tmp=$(mktemp)
cat "$C/head-cto.md" "$C/head-shared.md" > "$tmp"
vivi role add head-cto --project "$ROOT" --kind head --model P0-S2
vivi role charter set head-cto --project "$ROOT" --body-file "$tmp"
rm "$tmp"
```

A spawned seat then loads its charter and one handle, nothing else:

```bash
vivi role charter show <role> --project "$ROOT"
vivi task show <handle> --project "$ROOT"
```

## The charter shape

Every charter follows the same three-part form:

1. **A spawn pointer.** The first line, no heading: what the seat is, its one
   job, and what it must not do. This is the opening of a spawn prompt.
2. **Standing law.** The durable rules the seat enforces on itself without the
   Mind present. Self-enforcing law is why these charters work on small models.
3. **Role sections.** Identity, job, do-not, and the report contract (how the
   seat answers on its handle and closes it).

A charter describes the frame, lens, and process of a seat. It is not a log and
not a status board. Fold a correction into the body: keep the rule, drop the
incident, the dates, the handles, and the item lists.

## The roster

`Band` is the row from the protocol's model table; a task overrides it with the
front matter key `band = "P1"`.

| Charter | Seat | Band | What it is |
| --- | --- | --- | --- |
| [`mind.md`](mind.md) | Mind | `P1-S2` | The Mind's practice layer on the protocol: decision routing, dispatch and report checks, gating verdict pairs, merge and verify, capacity and tree hygiene |
| [`hand.md`](hand.md) | Hand | `P2-S0` | Implements one logical change, commits, settles its handle with the receipt |
| [`planner.md`](planner.md) | Planner | `P1-S2` | Goal-forge, goal-check, and delivery lowering into unit graphs, with oracle and row-quality rules |
| [`triage.md`](triage.md) | Triage | `P2-S1` | Read-only registry reconciliation: one disposition per want, need, or goal, against the live tree |
| [`auditor.md`](auditor.md) | Auditor | `P1-S1` | Frozen-range review; verdict tier `clean_pass`, `residual`, `block_range`, `block_ship` |
| [`cadence.md`](cadence.md) | Cadence | `P4-S0` | Read-only board review; tells the Mind what should run |
| [`head-shared.md`](head-shared.md) | Head (shared) | — | The law every Head carries: posture, gate honesty, report contract |
| [`head-ceo.md`](head-ceo.md) | head-ceo | `P0-S2` | Priority and goal truth |
| [`head-cto.md`](head-cto.md) | head-cto | `P0-S2` | Gate honesty, architecture, and the need-definition gate |
| [`head-cxo.md`](head-cxo.md) | head-cxo | `P0-S2` | Purity and complexity |
| [`head-cpo.md`](head-cpo.md) | head-cpo | `P0-S2` | Product direction and audience value |
| [`head-cmo.md`](head-cmo.md) | head-cmo | `P0-S2` | Positioning, launch, editorial |
| [`head-cso.md`](head-cso.md) | head-cso | `P0-S2` | Security and threat model |
| [`test.md`](test.md) | test | `P2-S0` | Runs the test stage the task names; fixes nothing |
| [`runner.md`](runner.md) | runner | `P2-S0` | Runs one named surface once and maps its output to a verdict |
| [`lint.md`](lint.md) | lint | `P3-S0` | Clears the project's early static gate |
| [`merge.md`](merge.md) | merge | `P2-S0` | Integrates branches onto an integration branch in the Mind's order |
| [`release.md`](release.md) | release | `P2-S0` | Runs the mechanical release sequence the task lists |
| [`docs.md`](docs.md) | docs | `P3-S0` | Audits documentation claims against live code and repairs them |
| [`website.md`](website.md) | website | `P3-S0` | Keeps the public site in step with a release |

The six Heads are one role family with six lenses. Each lens file is short; the
shared block is written once and appended at install time, as above.

## What is not shipped

The protocol also defines a **Hater** seat, and a project may run gate seats
beyond the ones above (a canary, a benchmark). They have no charter here. The
"Charter must include" column of the protocol's Standing charters table says
what to write; a project that runs those seats writes its own.

## Editing

- A charter that names a project, a path, a host, a repository, or a tool
  belongs to that project's mailspace, not here. Put project law in a project
  rule or in the task body.
- Name only what exists in this repository. Before citing a command or flag,
  check it against `vivi --help`.
- Do not add a role's posture dials, per-unit instances, or dated amendments.
  A role is a class; the task carries the unit.
