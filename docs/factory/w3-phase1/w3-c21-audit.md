# W3-C21 audit — vivi-server builds and passes on vivi 21

verdict: CLEAN

Auditor: independent (did not implement). Read-only on git; no files in the repo touched.
Repo: `/Users/ianzepp/work/ianzepp/vivi-server-w3/need-w3-c21`, branch `need/w3-c21`, tip `82d2a6d`
(6 task merges: W3C21-1..-6). Base `main` = `59411be` line.
Authority: `~/work/ianzepp/vivi/docs/factory/wave3-completion-goal.md` §2.2.

Patch truth at audit time: `.cargo/config.toml` (git-ignored, untracked) patches `vivi` to
`/Users/ianzepp/work/ianzepp/vivi-server-w3/vivi-pin` at `49c413c0`, which equals vivi local
`main` (`git rev-parse main` = `49c413c0474228dc5979fd553b783c5dc5819393`). Working tree has a
local-only `Cargo.lock` modification; the branch diff for `Cargo.lock` is empty (nothing staged
or committed), `.cargo/` is untracked. Hygiene per goal §2.2 holds.

## Findings

No P1. No P2.

- **P3-1 (stale prose, W3-C3 scope)** — `scripts/two-checkout-smoke.sh:251-252`: comment says the
  `--depends-on` handle "travels in the letter's X-Vivi-Depends-On header". vivi 21 carries deps in
  front matter; the header writer/reader is gone from vivi. No assertion depends on the header
  (`grep -rn "X-Vivi-Depends-On" scripts/` matches only this comment), and the smoke passes (parent
  evidence 46/0; steps assert handle/content parity). Text-only; the header surface is rewritten by
  W3-C3 (its done_when owns `X-Vivi-Depends-On`).
- **P3-2 (sanctioned deferral, verify W3-C3 lands)** — `src/mcp.rs:373` still advertises
  `depends_on` in the `send` tool schema, and `src/mcp/live/write.rs:60-66` parses it, but the value
  is no longer carried into the letter (`SendRequest.depends_on` is gone in vivi 21). A caller
  passing `depends_on` on a work kind today loses it silently (non-work kinds are still refused).
  This is exactly the goal's compile-table ruling ("drop the field; `depends_on` goes into front
  matter in W3-C3") and the code carries a comment saying so — not a defect of this need, but the
  release must not ship before W3-C3 wires it.
- **Noted, not a finding (out of need scope, goal §3.5/W3-D4)** — `src/mcp/live/pool_bands.rs:67,73`
  still uses raw `config_edit::apply`. Known W3-D4 work item; audit scope item 7 says note only.

## Evidence per check

### 1. Full gate rerun

Command: `cargo test --no-fail-fast` (branch tip, path patch active).
Expected duration stated before the run: 60–120 s (parent measured 54 s; budget 10 min per goal
ruling 12). **rc=0, elapsed 32 s, 24 test binaries all `ok`, 269 passed / 0 failed / 3 ignored.**
Log kept at `/tmp/w3c21-audit-test.log`. Matches the parent's claim (rc 0; 54 s).

### 2. done_when grep + archival records

Command: `grep -rn "body_has_labeled_clause\|with_pump\|complete_open_needs\|promote it" src tests docs`
→ **rc=1, zero hits.** Required-empty: passes.

`docs/audit-command-tree-2026-10-11/` holds broader `promote` mentions (audit-work.md, README.md,
repro scripts). These are genuinely dated records: the directory is date-named, contents are an
audit brief/findings/repro from 2026-10-11 describing then-observed vivi behavior (e.g. `want
promote` DIVERGES findings F9/F10). Historical observation, not current instruction; none match the
done_when patterns. Also swept `README.md`, `CHANGELOG.md`, `AGENTS.md`, `docs/contract.md`,
`docs/phase-*.md` for `promote|closeout|with_pump|pump_gate|complete_open_needs|complete-but-open`:
only CHANGELOG history entries remain (including the entry that records `pump_gate`'s removal and
the old health body in a past version's notes) — correct as history.

### 3. No test weakened

`git log --oneline main..need/w3-c21`: 12 commits (6 task commits + 6 merge commits), matching the
parent's six merges. `git diff main..need/w3-c21 -- tests/` read in full (780 diff lines). Every
assertion change classified:

| Change | Class |
| --- | --- |
| `hosted_index.rs`, `measure_index.rs`, `pump_records.rs`, `mcp_hosted.rs` fixtures: prose `done_when:` bodies → `+++` front matter; wants get `question`; `SendRequest.depends_on` field dropped (deps move into body front matter at `mcp_hosted.rs` reads_reflect / depend test) | Sanctioned repin: v21 removed the prose-clause path and the `depends_on` field |
| `mcp_hosted.rs` `a_body_without_done_when_is_noted_like_the_cli` → `a_need_body_without_done_when_is_refused` + new `a_hosted_send_stores_a_format_one_letter_with_done_when` | **Strengthened**: old pinned the removed `note` surface (`body_has_labeled_clause`); new asserts the send is refused, the message names `done_when`, no letter is left behind, and the stored raw bytes carry `X-Vivi-Format: 1` + front matter |
| `pump_control.rs`: 3 refusal tests prepend `set_project_mode(Running)`; assertions unchanged; new `a_fresh_project_starts_stopped` pins `Stopped` | Sanctioned repin (goal Work 5) + new required test; nothing weakened |
| `pump_health.rs` rewrite: v19 unit/merge/blocked fixture → paused control + exceptions + fresh tick; asserts the **exact sorted field set** (13 keys, no removed field can return silently), verdict, reasons, per-platform writer, tick/status ages, schema, `>= since` window, `changed_since` = `{since, exceptions}` strictly-after count | **Strengthened**: old pinned removed fields (`units`, `merges`, `blocked`); new is an exact-set pin of the v21 shape |
| `pump_rungs.rs`: `rung_table` helper gains `pools` (asserted, more precise); tests seed rung pools before `set`; new `a_set_with_no_effective_rung_pools_is_refused_with_the_guard_error` (refused, "rolled back", "effective pools are empty", config byte-identical) | Sanctioned repin (goal Work 4: tests set pools first) + new guard test |
| `host_create_project.rs`: rung fixtures create a real base pool (`pools: ["rung"]`); new `create_project_with_rungs_and_pools_lands_every_command` (all four rungs land, readiness reflects them, no `pump.rungs` missing, no "effective pools" detail) | Sanctioned repin + the required create_project rungs+pools test |

No assertion was deleted without a replacement of equal or greater strength; every repin names a
surface vivi 21 removed (goal §2.2 rulings). Nothing weaker beyond those.

### 4. Production scope

`git diff --stat main..need/w3-c21`: src touched in exactly the four allowed places —
`src/mcp.rs` (tool descriptions: pump drops unit/merge/blocked/complete-open text; pump_rungs says
"a set is refused while the rung table has no effective rung pools"; board adds `serving`, drops the
`pump` schema flag), `src/mcp/host/rungs.rs` (pools/timeout written **before** rung commands;
`write_settings` now `config_edit::guarded`), `src/mcp/live/read.rs` (drops `with_pump`), and
`src/mcp/live/write.rs` (drops `body_has_labeled_clause` + note block, drops `depends_on` from
`SendRequest`, want-reopen text now says "send a need that serves it (`serves`) instead"). Scripts
(`hosted-smoke`, `project-sync-smoke`, `two-checkout-smoke`) add `--done-when` / v1 bodies — test
tooling, in scope for tasks -5/-6.

No raw `config_edit::apply` remains in the rungs write path: `grep -rn "config_edit" src/` →
`host/rungs.rs:116` uses `config_edit::guarded`; the only raw `apply` left in src is
`live/pool_bands.rs:67,73` (W3-D4, noted above). No route, keys, projects, or other write path
appeared or disappeared (no other src file in the diff).

### 5. Live spot-checks

Expected duration stated first (~5–20 s warm each; actual <1 s test time each):
- `cargo test --test mcp_hosted a_hosted_send_stores_a_format_one_letter_with_done_when -- --exact`
  → **1 passed, 0 failed** (ok).
- `cargo test --test pump_control a_fresh_project_starts_stopped -- --exact`
  → **1 passed, 0 failed** (ok).

### 6. Docs truth vs the pin

- `docs/contract.md` pump-health section now says "body follows vivi 21". Field table matches
  `vivi-pin/src/pump/health.rs:68-84` exactly (13 fields; no `units`/`merges`/`blocked`/
  `complete_open_needs`). Verdict rules match `vivi-pin/src/pump/health/verdict.rs` (stalled =
  control running + writer silent past `stale_after` or no-writer + stale `status.txt`; degraded =
  control paused/stopped/unreadable, writer unknown, or no writer + fresh `status.txt` while
  running — the old stuck/quarantined/stall-exception clause is gone, correctly). Window semantics
  are code-true: `views::exceptions` filters `*at >= window` (doc: "at or after `since`");
  `views::changed_since` counts `*at > since` (doc: "strictly after"). Not-ready fold text updated
  to vivi 21.
- `docs/phase-04-hosted-projects.md` rungs sentence now says a set is refused while the rung table
  has no effective rung pools, naming vivi 21's guarded `RungsConfig::set` rollback; matches the
  live behavior proven by the new pump_rungs refusal test ("rolled back", "effective pools are
  empty") in the green suite.
- No v20 claims remain in either section; CHANGELOG mentions are history entries.

### 7. Known leftover (not fixed, per scope)

`pool_bands` raw `config_edit::apply` at `src/mcp/live/pool_bands.rs:67,73` — W3-D4 scope, outside
this need. Recorded here only.
