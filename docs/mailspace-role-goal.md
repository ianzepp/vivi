# Goal: Vivi Role (First-Class Mailspace Agent Seats)

## Summary

Elevate project-local mailspace **identities** into first-class **roles**: durable
agent seats with standing definition and execution preferences. A role owns the
mailbox name, process class, lifecycle status, freeform labels, **charter**
(standing prompt body), and capacity fields (`harness`, `provider`, `model`,
`thinking`). Fleet and parent agents treat the role record as source of truth
for “who this seat is” and “how to run it next,” instead of editing
`fleet.json`, rewriting launch strings, and reinitializing panes to change
capacity.

Mailspaces remain the home for intra-project agent communication. Roles make
the **participants** in that communication first-class, so a Mind can spawn a
sub-agent with pointer-sized boot context instead of pasting persona text and
hunting tmux state.

## Problem

Fleet is the primary consumer of Vivi mailspaces. Today the same agent seat is
split across three weak layers:

1. **Vivi identity** — name + aliases only (`mailspace identity add|list|rename`).
   Enough for routing; not enough for operations.
2. **`fleet.json` role blocks** — provider, model, thinking, harness/launch,
   persona path, wake policy, tmux targets, notes. Capacity and seat definition
   are handwritten per fleet and often duplicated across fleets.
3. **Skill prose + persona files** — memo policy, report style, lens. Cold boot
   and reinit load file paths that can drift or dual-source with local
   role-prompt overrides.

This split creates concrete pain:

- **Capacity reassignment is a ceremony.** When a subscription lane is exhausted
  (e.g. OpenAI weekly limit), every affected role in every fleet needs
  `fleet.json` edits, helper rebind scripts, pane kill/relaunch, and hope that
  launch strings still match declared model fields. Minutes of ops for a
  metadata change.
- **Mind is a config CMS.** “How many hands? What model? What harness?” require
  reading overlay JSON and naming conventions, not a mailspace query.
- **Persona is pasted, not pointed at.** For each Head consult, Mind (or a
  wrapper) re-reads persona markdown and injects it into the child prompt.
  Context bloat; dual truth if the file and the live process disagree.
- **Sub-agent fleets are blocked.** A Mind running in a TUI (e.g. Grok Build)
  that wants to consult `head-ceo` should spawn a sub-agent with:
  - role name + capacity from Vivi,
  - charter load instruction (`vivi role charter show …`),
  - task handle (`vivi task show …`),
  not a tmux doorbell loop (ensure pane → wake → paste → poll).

Without first-class roles, Vivi owns mailboxes but not **seats**. Fleet owns
process but is forced to own seat essence too. The dual-channel model stays
honest only if work truth and seat truth both live in the mailspace, and
runtime binding stays optional overlay.

## Design Decisions (locked)

| Decision | Choice |
| --- | --- |
| Noun | **`role`** — first-class CLI and storage concept |
| Mailbox key | Role **`name`** (local-part); same token used for addresses today |
| Address | **Derived**: `{name}@{prefix}.local` (prefix = first 8 characters of the mailspace id) — not independently edited |
| Process class | **`kind`** — freeform; preferred `hand` \| `head` \| `mind` \| `operator` \| `steward` |
| Tags | **`labels`** — freeform slugs (e.g. `auditor`, `floater`) |
| Lifecycle | **`status`** — preferred set (at least `active`, `parked`, `retired`) |
| Standing prompt | **`charter`** — durable text body defining the seat (not a shortcode) |
| Capacity | Top-level **`provider`**, **`model`**, **`thinking`** |
| Execution home | Top-level **`harness`**; preferred vocabulary includes **`subagent`** (run in parent TUI / spawn child agent; no external pane as process truth) |
| Bulk updates | **Not in `vivi`**. One role per mutating command; bulk via external loops/scripts |
| Runtime geometry | **Out of role essence** (tmux target, host, cwd, steward, posture stay Fleet/overlay or observation) |
| Observation | **Out of role essence** (last-ran, pane state from sensors/wake receipts — not Mind-edited role fields) |
| Naming ban | Do **not** call charter `identity` (collides with mail identity / local-part) |

### Charter vs assignment

| Layer | Content | Lifecycle |
| --- | --- | --- |
| **Charter** | Who the seat is, lens, bans, report style | Rare; edited when the seat evolves |
| **Task / need** | This unit of work | Open → done |
| **Mail** | Deliberation / report | Routing |
| **Memo** | Private durable memory after learning | Accumulates; not the base definition |

### Harness vs capacity

| Field | Means |
| --- | --- |
| `harness` | Where/how the process runs (`subagent`, `tmux`, `vivi_pty`, …) |
| `provider` | API / account lane |
| `model` | Model id |
| `thinking` | Effort / thinking level |

Rebinding harness while keeping the same model is a common, single-field
operation. Changing provider/model for a subscription wall is capacity-only;
it must not require process restart as the *storage* of the change.

### Parent handoff invariant

> A parent never inlines a role’s standing definition when the child can resolve
> `role → charter` and `handle → task` from the project mailspace.

Thin boot example:

```text
You are fleet role head-ceo.
Load charter:  vivi role charter show head-ceo --project <root>
Load task:     vivi task show <handle> --project <root>
Optional bag:  vivi board --for head-ceo --project <root> --json
Execute per charter. Report via mailspace commands.
```

Parent re-reads roles before spawn waves so capacity/harness flips apply on the
**next** execution without treating runtime restart as the config store.

## Relationship To Prior Work

| Prior work | Relationship |
| --- | --- |
| `mailspace identity add\|list\|rename` | **Predecessor.** Role absorbs and thickens the roster; identity remains the address resolution concept (name + aliases). Prefer `vivi role` as the operator/Mind surface. |
| `memo` kind (`docs/memo-kind-goal.md`) | Orthogonal. Memos are per-role private memory; charter is the standing seat definition. Policy “who may use memos” may later key off `kind`, not this goal’s core. |
| Mailspace control plane / board | Orthogonal join: bag state by `--for <name>`; do not store open-task lists on the role row. |
| Fleet skill + `fleet.json` | **Consumer migration target.** Capacity + charter move toward Vivi role as authority; Fleet keeps posture, ladders, optional runtime bindings, sensors. Dual-write acceptable during transition if Vivi is authoritative for listed fields. |
| Fleet persona files / role-prompt paths | **Seed/import sources** for `role charter set --file`, not long-term source of truth. |
| `vivi-pty` | One possible `harness` value / runtime backend; not required for `subagent`. |

## Goals

1. **First-class role records** in the project mailspace, keyed by `name`, with
   fields: `kind`, `labels`, `status`, `harness`, `provider`, `model`,
   `thinking`, `charter`, derived `address`, and existing alias behavior for
   renames.
2. **CLI surface** (simple; one role per mutation):
   - `vivi role list [--json]`
   - `vivi role show <name> [--json]`
   - `vivi role add <name> …` (kind and other fields as flags)
   - `vivi role set <name> …` (partial update of scalar fields / labels / status)
   - `vivi role charter show <name>`
   - `vivi role charter set <name> --body … | --body-file … | --file …`
   - Rename / retire (or status=`retired`) with alias preservation consistent
     with today’s identity rename semantics
3. **Address continuity:** sending mail/task/need/want/memo to a role name
   keeps working; resolve name and aliases as today.
4. **Status reporting:** `mailspace status` and/or `role list` expose enough
   for Mind census (name, kind, status, harness, provider, model, thinking;
   bag counts remain board/status joins).
5. **No bulk filters in Vivi.** Scripts loop `role set` for multi-role capacity
   flips.
6. **Docs + skill-facing examples:** pointer-style sub-agent boot; capacity set
   without pane reinit; charter as standing prompt.
7. **Migration path:** existing identities become roles with empty/default
   optional fields; optional import of persona file into charter.

## Non-goals

- Bulk update / `--where provider=` inside the `vivi` binary.
- Storing tmux target, host, cwd, steward config, fleet posture, or mind-loop
  interval on the role as required fields.
- Observed runtime state (`running`, last-ran) as Mind-edited role fields.
- Replacing Fleet process skill, sensors, or doorbell for seats that still use
  `tmux` / `vivi_pty`.
- Access control / multi-tenant privacy between roles (mailspace stays shared).
- Full persona versioning UI (content-addressed history optional later).
- Encoding process rights solely as labels (e.g. `merges_to_main` may stay
  Fleet policy or a future explicit flag — not required in v1).
- Forcing rename of existing compound names (`head-cto`); `kind` remains
  explicit metadata even when the shortcode is embedded in `name`.
- Automatic fleet.json rewrite or deletion in this goal (consumer migration
  is follow-on; Vivi must not depend on Fleet).

## Command Shape (target)

```text
vivi role list   --project <root> [--json]
vivi role show   --project <root> <name> [--json]
vivi role add    --project <root> <name> --kind <kind> \
                 [--harness subagent] [--provider …] [--model …] [--thinking …] \
                 [--status active] [--label slug …]
vivi role set    --project <root> <name> \
                 [--kind …] [--harness …] [--provider …] [--model …] [--thinking …] \
                 [--status …] [--label …] [--clear-label …]
vivi role charter show --project <root> <name>
vivi role charter set  --project <root> <name> --body '…' | --body-file PATH | --file PATH
```

External bulk (not product CLI):

```sh
for r in head-ceo head-cto hand-1; do
  vivi role set --project "$ROOT" "$r" --provider zai --model glm-5.2 --thinking high
done
```

## Invariants

1. **One seat, one name.** Role `name` is the stable mailspace key and local-part.
2. **Address is derived.** `address = "{name}@{prefix}.local"`, `prefix` being the first 8 characters of the mailspace id.
3. **Charter is standing definition**, not an assignment and not board work.
4. **Capacity fields are desired execution preferences**, not proof a process is
   live. Updating them does not require runtime restart to be considered stored.
5. **Harness `subagent`** means parent-runtime spawn/consult; external pane is
   not process truth for that seat.
6. **Mutations are single-role.** No multi-match rewrite in core CLI.
7. **Rename preserves history** via aliases (same contract as identity rename:
   do not rewrite historical message rows).
8. **Unknown `kind` / `harness` / provider strings are allowed** (freeform);
   preferred vocabularies are documented and recognized by consumers.
9. **Role is mailspace-local.** Cross-project bulk is a loop over project roots.
10. **Vivi does not parse `agent_launch` as capacity truth.** If a launch string
    exists in overlay, consumers should prefer role provider/model/thinking
    (and derive launch only when required for legacy harnesses).

## Acceptance Signals

- Existing mailspace with identities can list equivalent roles after migration
  (names preserved).
- `vivi role add auditor-1 --kind hand --harness subagent --label auditor`
  creates a seat addressable as `auditor-1@…`.
- `vivi role set hand-1 --provider zai --model glm-5.2 --thinking low` updates
  only those fields; charter unchanged.
- `vivi role set head-ceo --harness subagent` flips execution home without
  requiring other field changes.
- `vivi role charter set head-ceo --file path/to/ceo.md` stores body;
  `charter show` returns it.
- `vivi role show head-ceo --json` includes kind, status, labels, harness,
  provider, model, thinking, address, and charter (or charter digest + separate
  show — pick one consistent JSON shape in delivery).
- `vivi mail send` / `task send` to the role name still delivers.
- Rename keeps old name as alias; historical mail still resolves.
- No bulk-update subcommand exists in `--help`.
- `cargo fmt --check`, `cargo test --test hygiene`, `cargo test` pass.
- README / vivi skill surface documents role fields and pointer-style boot.

## Ground Truth (repo)

- `src/mailspace/identity.rs` — `LocalIdentity { name, aliases }`; add/rename/resolve.
- `src/mailspace.rs` — `MailspaceConfig.identities`, status rows, `mailspace.toml`.
- `src/cli/mailspace_command.rs` — `identity add|list|rename` only.
- Live fleets store capacity on `.vivi/fleet.json` hand/head blocks (`provider` /
  `agent_provider`, `agent_model`, `thinking`, `agent_launch`, `persona` path).
- Fleet skill documents identity ≠ assignment ≠ runtime; this goal moves seat
  essence + declared capacity into Vivi.

## Suggested Factory Decomposition

Delivery may split; boundaries below are guidance for production, not a commit
to phase count:

| Unit | Intent |
| --- | --- |
| **1. Role model + persist** | Extend config/store: role record fields; migrate identities → roles; list/show/add/set scalars |
| **2. Charter body** | Store/retrieve charter; set via body/file; show; size/safety limits consistent with other body intake |
| **3. CLI completeness + status** | Full `vivi role` command tree; wire address resolution; status/list columns; rename/retire |
| **4. Docs + consumer notes** | README, AGENTS/vivi skill, migration notes for Fleet (authoritative capacity/charter; bulk loops; subagent boot) |

Optional follow-on (not this goal’s acceptance): Fleet helpers read role capacity
from Vivi; stop dual-writing provider/model into `fleet.json`; persona files
become import-only.

## Open Implementation Choices (delivery may decide)

| Topic | Default bias |
| --- | --- |
| Storage | Prefer extend `mailspace.toml` and/or sqlite side table; charter body may be blob or file under `.vivi/` if large |
| Identity CLI | Keep thin wrappers or deprecate in favor of `role` with clear migration message |
| JSON shape | `show --json` includes full charter vs `charter_sha` + separate charter show |
| Default harness/status | `status=active`; harness unset or `subagent` for new roles — pick one and document |
| Label flags | Repeatable `--label` on add/set; explicit clear |

Do not block the goal on Fleet rewrite. Ship Vivi role authority first.

## Success Picture

Mind (or operator) can:

1. `vivi role list --json` and know the roster, capacity, and harness for a
   project without opening `fleet.json`.
2. Flip a seat’s provider/model with one `role set` when a subscription dies;
   next sub-agent spawn uses the new triple.
3. Consult a Head by spawning a sub-agent whose boot only points at charter +
   task handles.
4. Keep long-lived `tmux` / `vivi_pty` seats as harness values without forcing
   all fleets onto sub-agents on day one.
