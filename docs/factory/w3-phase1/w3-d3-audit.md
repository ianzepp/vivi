# W3-D3 independent audit — server-level MCP endpoint, keys, grants, audit

verdict: RESIDUAL

- **Auditor**: independent (did not implement). Correctness/evidence pass; the
  adversarial grant-check pass is a separate report (`w3-d3-adversarial.md`).
- **Repo**: `vivi-server` worktree `need-w3-d3`, tip `6604df0`, base `main`
  `ff1adb7`, path-patched via `.cargo/config.toml` to vivi 21 at
  `../vivi-pin` (vivi `f289ace8`). `Cargo.lock` is dirty from the patch and
  uncommitted — correct under the patch rules.
- **Authority**: `vivi/docs/factory/wave3-completion-goal.md` §3.4 (W3-D3).
- **Date**: 2026-10-24.

## Findings

### P1

None.

### P2

**P2-1. Self-service key mint/revoke writes no audit line (key/grant change
unlogged).** The goal's audit spec (§3.4): "one line per write tool call and
**per key/grant change**". `docs/contract.md:463-465` repeats it ("and per
key/grant change"). Every audit line is written from `src/mcp/server.rs`
(grep: no `state.audit` use outside it); the self-service route
`POST /v1/projects/{id}/keys` creates a principal + key + grant
(`src/keys.rs:460-510`, `ServerKeyStore::mint`) and `DELETE
/v1/projects/{id}/keys/{key_id}` revokes the server twin
(`src/keys.rs:620-645`) with no audit line. A host reviewing key administration
through `DATA_DIR/audit.jsonl` sees host-tool mints only; every self-service
principal/grant creation is invisible to the trail. Either audit those two
route outcomes or narrow the contract's "per key/grant change" claim to
`/v1/mcp` explicitly — as written, code and contract disagree.

### P3

**P3-1. Principal aliasing edge: `create_project` key and a no-identity
self-service key share one principal.** `create_project` mints principal
`<member>@<project_id>` with grant `identity: Some("operator")`
(`src/mcp/host.rs:227-246`); a later self-service mint by the same member with
no acting identity resolves the **same** principal name (`src/keys.rs:473-477`)
and adds a second grant with `identity: None` (`src/grants.rs:275-278` adds
grants not already held). `resolve_grant` takes the first exact-id grant
(`src/mcp/server.rs:411-413`), so each key can resolve the other's grant —
acting identity and displayed level swap. Benign while
`HOSTED_DEFAULT_IDENTITY=operator` (both paths then act as operator);
mis-binding with a different default. Deviation (b) fixed this for
two *named* identities but not for the None-vs-create_project pair. The
fixture authors know ("the first grant for a project wins",
`tests/mcp_hosted.rs` `server_key` comment). Ledger note or a one-line fix
(make `create_project` mint `<member>.operator@<project_id>`).

**P3-2. Contract wording: audit `project_id`/`key_id` are omitted, not
`null`.** `docs/contract.md:467-470` says "`project_id` (null for host tools)".
`AuditRecord` uses `skip_serializing_if` (`src/audit.rs:27-33`), so host-tool
lines omit `project_id` and host-token lines omit `key_id`. The unit test
itself pins absence (`tests/mcp_server.rs:368`, `.get("project_id").is_none()`).

**P3-3. Boot-retire marker write failure re-arms the sweep.** If writing
`DATA_DIR/api_keys.retirement` fails (warn only, `src/grants.rs:588-593`), the
next boot re-runs the sweep. A mirror `api_keys.json` minted in between
survives only when a `.retired` sibling exists (`src/grants.rs:611-613`
skips when `gone.exists()`); a project with **no** pre-upgrade key file whose
mirror was minted before the failed-marker boot would lose that mirror (rooms
+ letter wire auth for that key) at the next boot. Double-failure edge; a
`retired`-marker write failure could also `tracing::warn!` forever.

**P3-4. No rate limit before the grant wall; refused writes append audit
lines unthrottled.** `allow_write` runs only after grant resolution
(`src/mcp/server.rs:332-335`); a refused write (missing project, ungranted
project) is audited (`refused`, `src/mcp/server.rs:803-817`) but not counted
against any bucket. A hostile live key can grow `audit.jsonl` at full HTTP
request rate. Parity with the old routes' write-only limiting; operational,
not a tenant-boundary break (the key is revocable). Consider counting
refused writes against the key's own bucket.

**P3-5. Cross-tenant negative tests exist for read+write, not literally "per
tool family".** `tests/mcp_server.rs:336-379` pins a read (`overview`) and a
write (`send`) refusal on a durable project, plus the unknown-project
same-text. Hosted/live/operator families have no dedicated cross-tenant
negative. Structurally redundant — the wall (`resolve_grant`) runs before
every dispatch family and the test proves the text — but the goal's "a
negative test per tool family" is not literally met.

**P3-6. `rooms.rs` change is a wire-visible field, not a comment.** The task
list said "rooms.rs comment"; the actual change rewrites the room JSON's
`mcp` field from `/v1/projects/{id}/mcp` to `/v1/rooms/{id}/mcp`
(`src/rooms.rs:92`). Correct (the old value named a dead route) and documented
(`docs/contract.md:46-48`), with `tests/rooms.rs` updated — but it is a
behavior change on the rooms API surface, disclosed here as such.

**P3-7. `tools_list` copies only index 0 of the host catalog.**
`src/mcp/server.rs:144` pushes `super::host::tools_list()["tools"][0].clone()`;
a second host tool would be silently unlisted. Today's catalog has exactly
`create_project`, so no behavior defect.

**P3-8 (sideband, pre-existing — not a D3 defect).**
`scripts/project-sync-smoke.sh` and `scripts/two-checkout-smoke.sh` are mode
`100644` at this branch, at base `ff1adb7`, and on `main`; `AGENTS.md`'s merge
gate runs them directly (`scripts/project-sync-smoke.sh`), which fails with
`permission denied` on a fresh checkout. Unchanged by this need; ledger note.

## Deviation rulings

**(a) Boot retirement runs once per data dir (marker
`DATA_DIR/api_keys.retirement`) instead of every boot — AGREE.** The rulings
keep the rooms endpoint unchanged and keep keys on the letter wire; the
self-service mint therefore re-creates `projects/<id>/api_keys.json` as a
mirror for those consumers (`src/keys.rs:492-506`). A every-boot retirement
would rename that mirror at the next restart and silently kill rooms/letter
auth for a live key — strictly worse than the goal's sentence, which was
written before the mirror consequence was visible. The goal's actual intent
(pre-upgrade keys die, one log line per project naming the dead-key count) is
met and pinned: probe confirms rename + marker + the exact log line
(`retired 2 legacy api key(s) of project p1`); `tests/mcp_server.rs:862-919`
pins rename-once, mirror survival across boots, and sweep-skipping. Residual
edge in P3-3.

**(b) Self-service principals: `<member>@<project>` without an acting
identity, `<member>.<role>@<project>` with one — AGREE.** Grants hang off the
principal and `verify` returns the whole grant list
(`src/grants.rs:447-469`); two mints with different acting identities on one
shared `<member>@<project>` would alias each other's grants — the second key
would act with the first's identity, and per-call identity binding
(`Caller.acting_as`) would be wrong. Distinct principals per acting identity
keep each key's grant exact, at the cost of the goal's simpler naming. It is
documented (`docs/contract.md:488-492`, `src/keys.rs:470-474`) and the goal's
real requirement (grant-per-principal resolution) is preserved. Residual
aliasing edge in P3-1.

## Scope check (item 6)

`git diff --stat ff1adb7..need/w3-d3` touches only: `src/{audit,grants,keys,
lib,main,mcp,mcp/host,mcp/live,mcp/live/pump,mcp/server,rooms}.rs`,
`docs/contract.md`, `README.md`, `CHANGELOG.md`, `scripts/{hosted,mcp,
two-checkout}-smoke.sh`, and `tests/` (11 files, 3 new). All inside the D3
file list; `src/projects_api.rs` needed no change (the grants answer rides the
`keys.rs` handler). Only nuance: P3-6 (rooms field vs "comment"). No scope
leaks.

## No test weakened (item 2)

`git log ff1adb7..need/w3-d3`: 4 task commits + 4 `--no-ff` merges
(`d2defde` W3D3-1, `ac5de2a` W3D3-2, `94442d6` W3D3-3, `c060717` W3D3-4, plus
`657787f` main sync). Reviewed every deleted line in
`git diff ff1adb7..need/w3-d3 -- tests/` (156 deletions vs 2227 insertions):

- Old-route pins (`/v1/projects/{id}/mcp`, `/v1/host/mcp`) map to new-route
  equivalents; both old routes now have explicit 404 tests
  (`tests/mcp.rs`, `tests/host_create_project.rs:127-142`,
  `tests/rooms.rs`).
- Old-auth pins remap to the ruled truth, not to something weaker: identity
  tokens on `/v1/mcp` become refusal tests pinning the exact hint text
  (`tests/mcp_hosted.rs:819`, `tests/host_create_project.rs:230-270`); the
  old host-endpoint 403s become (ruled) 401 for non-keys and tool-error "not
  a host principal" for server keys (`tests/host_create_project.rs:225-269`).
- Tool-behavior pins survive on the new wire: rate limit
  (`over 2 writes/min for this key`, re-keyed per (project, key) per the
  goal, `tests/mcp_hosted.rs:771-816`), operator-rule refusals
  (`tests/pump_control.rs:233,266`, `tests/mcp_server.rs:382`),
  `mcp_path` = `/v1/mcp` + `pump_list` over it
  (`tests/host_create_project.rs:209-221`), member-token transport-tool
  access moved to a keys-only test (`tests/mcp_hosted.rs:958`).
- The member-token "keeps transport tools" test became a refusal test — that
  is the ruling ("Identity tokens are not accepted on `/v1/mcp`"), recorded
  in the test comment, and rooms still keep the member-token path.
- `tests/keys.rs` unchanged and green (mirror behavior).

No silent behavior loss found. `scripts/mcp-smoke.sh` adds `done_when` to its
send — required by C3's gate, not a weakening.

## Required tests vs "Tests that must exist" (item 5)

| Required | Where | Status |
| --- | --- | --- |
| Call without `project` refused | `tests/mcp_server.rs:323` (exact text, non-string too) | exists, passes |
| Ungranted project = same text as unknown | `tests/mcp_server.rs:336,369-371` | exists, passes |
| Operator tool on member grant | `tests/mcp_server.rs:382`, `tests/pump_control.rs:233,266` | exists, passes |
| Host tool without host | `tests/mcp_server.rs:688`, `tests/host_create_project.rs:225` | exists, passes |
| One audit line per write | `tests/mcp_server.rs:604,660`, `tests/audit.rs` | exists, passes |
| Old key file renamed at boot | `tests/mcp_server.rs:862`, `tests/grants.rs` | exists, passes |
| `vivi mcp tools` + `vivi mcp call overview` e2e | `scripts/mcp-smoke.sh:82-104` + probe | exists, passes |
| Negative per tool family (cross-tenant) | `tests/mcp_server.rs:336` (read+write, durable) | partially — P3-5 |

## Correctness read (item 3)

- **Grant resolution order**: exact id first, then hosted name among grants
  only; durable/chat by id only; two names → error naming both ids
  (`src/mcp/server.rs:407-436`). ✓
- **Never dispatch on the raw argument**: `project` is removed from the tool
  arguments and dispatch receives the resolved grant's own id
  (`src/mcp/server.rs:336-339`). Name/unknown share one refusal text, so no
  probing; dead projects and unregistered members read ungranted/refused
  (`src/mcp/server.rs:438-459`). ✓
- **Member/operator re-checks per call**: `dispatch_tool` feeds the grant's
  registered identity to the live tools; `authorize`/`require_operator`
  re-derive the host's-own-member + operator-kind rule per call
  (`src/mcp/live/pump.rs:141-165`, `src/mcp/live.rs:90-104`); the stored
  `level` is metadata only. Pinned by test (`tests/mcp_server.rs:382`). ✓
- **Host-token path**: constant-time compare (`src/mcp/server.rs:94-101`,
  `constant_time_eq`); unset token → no built-in host principal, keys still
  authenticate, host tools refuse (`tests/mcp_server.rs:720-733`,
  `tests/host_create_project.rs:126-142`). ✓
- **Audit completeness**: `is_write` (`src/mcp/server.rs:767-778`) covers
  exactly the catalog's write surface — `send, reply, done, reopen, mark,
  unmark, depend, pump_control, pump_send`, `memo` save, `pump_rungs` set,
  `pool_bands` set/unset — plus host tools `create_project, key_mint,
  key_revoke, grant_add, grant_remove`. No write tool lacks an audit line on
  `/v1/mcp`; the gap is the off-MCP route (P2-1). Reads (`overview, list,
  show, thread, graph, board, pump, pump_list, pump_show, memo list/show,
  pump_rungs show, pool_bands show, key_list, projects`) are not logged. ✓
  (with P2-1)
- **Rate-limit keying**: (project id or `host`) × key id hash through the
  shared limiter; inner dispatch gets an unlimited limiter so writes count
  once (`src/mcp/server.rs:778-794`, `src/mcp.rs:346-355`). ✓ (P3-4 for the
  pre-wall path)
- **Error-text fidelity**: `project is required` and
  `this key has no grant on <named>` byte-match vivi's fake-server pins
  (`vivi tests/support/project_server_mcp.rs:131-138`, probe-verified);
  host-tool, grant-admin, and rate-limit texts match contract verbatim. ✓
- **Stores**: `keys.json` 0600 atomic tmp+rename, undecodable file = boot
  error (never empty store), digests only, constant-time digest compare,
  revoked excluded, ≤32 live keys/principal (`src/grants.rs:170-470`).
  `audit.jsonl` 0600 append-only, single write per line, failures logged not
  swallowed silently into success (`src/audit.rs:59-89`). ✓

## Docs truth (item 4)

`docs/contract.md` "Server MCP (wave 3)" checked line by line against code:
routes, auth set (server key / host token / refusal texts), `project`
resolution rules, tools tables (project + host), grant shape and per-call
re-check, audit + rate-limit paragraphs, self-service section (grants in the
answer, mirror, twin revocation), one-time retirement — all match the code
and my probes, except P3-2 (null vs omitted) and P2-1 ("per key/grant
change" overstates). `README.md` and `CHANGELOG.md` entries are accurate
(rooms "no server key", `HOST_OPERATOR_TOKEN` row, D3 bullet matches
behavior). `AGENTS.md` untouched and contains no stale MCP route text, so
nothing was required there. Historical phase docs (`phase-03`, `phase-04`)
still name the old routes — stale-by-default history docs, out of D3 scope.

## Probes and checks (commands, rc, duration)

Burgus, warm target, 2026-10-24. Patch active (`.cargo/config.toml` →
`../vivi-pin`, vivi `f289ace8`).

| Check | Command | rc | Duration | Result |
| --- | --- | --- | --- | --- |
| Build | `cargo build --bin vivi-server` | 0 | 0.15 s | warm |
| Live probes (21 checks) | `bash /tmp/w3d3-probe.sh` (script kept; boots `target/debug/vivi-server` on 127.0.0.1, temp `DATA_DIR`) | 1* | 0.94 s | 20 PASS + 1 harness-only FAIL (see below) |
| Old routes 404 | probe | — | — | `/v1/projects/<id>/mcp` 404, `/v1/host/mcp` 404 ✓ |
| `project is required` | probe (exact text) | — | — | ✓ exact |
| `this key has no grant on <B>` | probe (key on A → tool on B, exact text) | — | — | ✓ exact; unknown project same text ✓ |
| Legacy retirement | probe (pre-placed 2-key `api_keys.json`) | — | — | renamed `.retired`, marker file, log line names count ✓ |
| Audit per write | probe (baseline → write +1, read +0, refused write +1; shape) | — | — | ✓ (refused-no-project line omits `project_id`, as designed) |
| vivi client e2e | `VIVI_MCP_SERVER=… VIVI_MCP_API_KEY=… vivi mcp --project-id <A> tools` / `call overview` (vivi-pin `target/debug/vivi`) | 0 | — | both answer; overview names the project ✓ |
| Identity-token refusal / host token | probe | — | — | hint text ✓; `key_list` answers for the token ✓ |
| Targeted suites | `cargo test --test mcp_server --test grants --test audit --test keys --test host_create_project --test mcp --test mcp_hosted --test rooms` | 0 | 8.7 s | 119 passed, 0 failed, 1 ignored |
| MCP smoke (with vivi client leg) | `VIVI=…vivi-pin/target/debug/vivi scripts/mcp-smoke.sh` | 0 | 1.3 s | 10/0 (incl. `vivi mcp tools`, `vivi mcp call overview`) |
| Hosted smoke | `VIVI=… scripts/hosted-smoke.sh` | 0 | 4.0 s | 23/0 |
| Project-sync smoke | `VIVI=… bash scripts/project-sync-smoke.sh` (file is 644, see P3-8) | 0 | 6.7 s | 84/0 |
| Full suite | not rerun by this auditor | — | — | parent's record: rc 0, 53 s on `6604df0` |
| Two-checkout smoke | not rerun by this auditor | — | — | parent's record: 46/0 (~2 min leg) |

\* The probe script's one FAIL is the auditor's own quoting bug (grep against
escaped JSON); the send it "failed" succeeded (`isError:false`, handle
returned, audit line written). Two earlier runs of the same probe confirmed
every other check; the final transcript of the last run is reproducible from
`/tmp/w3d3-probe.sh`.

## Verdict

RESIDUAL. The endpoint, grant wall, stores, audit (on `/v1/mcp`), cutover,
docs and tests are correct and evidenced; nothing blocks the merge. P2-1
(audit misses self-service key/grant changes, contract overstates) should be
fixed inside the need or explicitly waived by the operator; the P3s go to the
ledger.

## pass 2

verdict: CLEAN

Fix commit `d8fb261` (task W3D3-5, merged `--no-ff` as `bd8b849`; no `Cargo.lock`
or `.cargo/` in the commit, working-tree lockfile dirt remains the patch's).
The commit also closes findings from the separate adversarial pass (principal
collision, mirror lifetime, fallback actor); those were re-read here for new
defects only.

### Pass-1 findings re-verified as fixed

- **P2-1 FIXED — self-service mint/revoke each write exactly one audit line.**
  Code: `src/keys.rs` `mint_handler` success arm writes one
  `self_service_mint` line; `revoke_handler` writes one `self_service_revoke`
  line (principal from `holder_of_key` when a twin exists). Live probe
  (`/tmp/w3d3-probe2.sh`): refused mint (bad label) → 0 lines; mint → exactly
  1 `self_service_mint`; `DELETE` → 204 + exactly 1 `self_service_revoke`;
  both lines carry `key_id`/`principal`/`project_id`/`tool`/`outcome`/`summary`.
  Unit pin: `tests/mcp_server.rs
  self_service_mint_and_revoke_each_write_one_audit_line` (green). Contract
  documents the two tool names (`docs/contract.md` audit paragraph).
- **P3-2 FIXED — `key_id`/`project_id` serialize as `null`, never omitted.**
  `src/audit.rs` dropped `skip_serializing_if` on both; `tests/audit.rs
  two_records_append_two_lines_with_null_key_and_project_fields` pins it.
  Live: probe run's `audit.jsonl` shows `"project_id": null` on the
  refused-no-project line.
- **P3-7 FIXED — every host tool is cloned.** `src/mcp/server.rs` `tools_list`
  now `extend`s the whole host catalog array instead of `[0]`.
- **P3-1 FIXED (bonus, via the adversarial fix) — derived principals never
  silently share.** `ServerKeyStore::mint_derived` derives
  `<member>[.<role>]@<project>` and refuses (`409 principal_conflict`,
  message naming both derivations) when the existing principal holds any
  grant with a different `(member, identity)` pair. Pin:
  `tests/mcp_server.rs a_self_service_mint_never_shares_a_derived_principal`;
  contract documents the 409 and the `.`-in-member-names case.

### Fix-introduced-defect read (none found)

- **Mirror-revocation order** — mirror first, server twin second, on all
  three paths: host `key_revoke` revokes the mirror entry in every project
  the key's principal is granted on, then `revoke_key`
  (`src/mcp/server.rs:660-672`); host `grant_remove` revokes that
  principal's entries in that one project's file first, then `remove_grant`
  (`src/mcp/server.rs:687-702`); HTTP `DELETE` revokes the per-project
  (mirror) registry first, then the twin (`src/keys.rs:651-667`). The
  failure direction is safe: a mirror storage failure aborts before anything
  dies (key live everywhere); the reverse worst case is mirror-dead +
  store-live — over-dying on the weaker (grant-free) surface, never
  outliving the store. Pins: `a_host_key_revoke_kills_the_per_project_mirror`
  (dead on `/v1/mcp` **and** rooms; mirror entry `revoked_at` set),
  `a_host_grant_remove_revokes_that_projects_mirror_only`.
- **`mint_derived` collision guard runs under the store lock** —
  `self.state()` is held across both the collision scan and `mint_locked`
  (`src/grants.rs:337-399`), so two concurrent mints cannot interleave
  check-then-upsert. The guard is deliberately one-sided (host `key_mint`
  and `create_project` still upsert by name): the host is a trusted minter,
  and the `create_project` reverse order cannot occur (its project is fresh,
  so no self-service principal can pre-exist on that id).
- **`fallback_actor` own-member rule** — a key with no acting identity on a
  hosted project acts as `HOSTED_DEFAULT_IDENTITY` only when its member is
  the mailspace's own member (`mailspace_id` equality, now the shared
  `is_own_member` that `require_operator` also uses,
  `src/mcp/live.rs:106-129`, `src/mcp/live/pump.rs:157`); any other member
  acts as its own name, which the mail tools resolve as a role or refuse.
  This closes the "non-own member silently acts as the operator default"
  escalation for live tools; operator tools keep their own per-call
  re-check. Pins: `tests/mcp_hosted.rs
  a_member_level_key_acts_as_the_member_not_the_default_operator`
  (bot key sends as `bot`; `pump_control` refused; own-member key still
  `operator`) and `a_key_without_identity_sends_as_the_default_operator`.
  Rooms/durable callers are unaffected (identity tokens keep transport
  tools; non-hosted projects skip the branch).

### Endpoint otherwise unchanged (probes re-run)

`bash /tmp/w3d3-probe.sh` (fresh boot, temp `DATA_DIR`): 20 PASS + the one
known harness quoting FAIL (the send itself succeeds — handle `a0215ef2`,
`isError:false`). Both chosen probes PASS: old routes 404
(`/v1/projects/<id>/mcp`, `/v1/host/mcp`); one audit line per `/v1/mcp` write
(self-service mint line = baseline 1; write → 2; read → 2; refused write → 3),
plus unchanged: exact `project is required`, exact
`this key has no grant on <B>`, unknown-project same text, legacy retirement
(rename + marker + log line), identity-token refusal text, host-token
`key_list`, `vivi mcp tools` / `vivi mcp call overview` (vivi-pin client).

### Commands and outcomes (burgus, 2026-10-24, warm target)

| Check | Command | rc | Duration | Result |
| --- | --- | --- | --- | --- |
| Build | `cargo build --bin vivi-server` | 0 | 0.15 s | warm at `bd8b849` |
| Targeted suites | `cargo test --test mcp_server --test audit --test grants --test keys --test mcp_hosted --test mcp --test host_create_project --test rooms` | 0 | 8.4 s | 124 passed, 0 failed, 1 ignored (mcp_server 17, mcp_hosted 29, audit 4 — up from pass 1) |
| Full probe | `bash /tmp/w3d3-probe.sh` | 1* | 1.5 s | 20/21 substantive PASS (*harness quoting bug, send verified successful) |
| Self-service audit mini-probe | `bash /tmp/w3d3-probe2.sh` | 0 | 0.3 s | refused mint 0 lines; mint 1; revoke 1; shape OK |
| Tests diff review | `git diff 6604df0..need/w3-d3 -- tests/` | 0 | — | only strengthens: audit shape test now pins `null` fields; 5 new tests; no assertion removed |

### Remaining findings (all P3, ledger)

- **P3-3** (carried): failed `api_keys.retirement` marker write re-arms the
  boot sweep; a mirror minted meanwhile can be lost on a project with no
  `.retired` sibling (double-failure edge; `src/grants.rs:588-593`).
- **P3-4** (carried): refused writes bypass `allow_write`, so an unthrottled
  live key can grow `audit.jsonl` at request rate (`src/mcp/server.rs`
  resolve-before-limit order).
- **P3-5** (carried): cross-tenant negatives cover read+write at the single
  choke point, not literally one per tool family.
- **P3-8** (carried, pre-existing at base and main): `project-sync-smoke.sh`
  and `two-checkout-smoke.sh` are mode 644.
- **P3-9** (new, minor): a `mint_derived` re-mint with the same
  `(member, identity)` but a changed `level` appends a second grant on the
  same project (`mint_locked`'s exact-match `contains`,
  `src/grants.rs:275-278`); the first grant keeps winning id resolution, so
  the new level never applies there. Cosmetic — `level` is display metadata
  and the tools re-derive the rule per call — and pre-existing from the
  pass-1 `mint` design, surfaced now only because the collision guard made
  derivation explicit.

No open P1/P2. Pass-1 P2-1, P3-1, P3-2, P3-7 are fixed and verified; the fix
introduced no new defect found. Full suite and the two long smokes were not
re-run in pass 2 (targeted suites + probes re-run here; pass-1 full-suite and
smoke records stand for the shared code paths, and `bd8b849`'s diff is
confined to the re-tested surfaces).
