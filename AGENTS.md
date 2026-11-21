# Vivi Agent Guide

## Project

Vivi is the project mailspace for private agents: durable, project-local
coordination state — tasks, needs, wants, mail, memos, roles, goals, and
executable work graphs — driven by the `vivi` CLI. Around the mailspace sit
pools and declared slots, the pump and its flows, the model proxy, a project
server with a read-only web console, and sync.

The codebase is Rust, Edition 2024, with the toolchain pinned in
`rust-toolchain.toml`. Treat `Cargo.toml`, `README.md`, and the code as the
source of truth when these instructions drift.

## Sibling repositories

Vivi was split out of one repository on 2026-09-21. This repo owns the
mailspace half and everything built on it; the email and PTY halves live in
the siblings:

| Repo | Owns |
| --- | --- |
| `vivi` (this repo) | Project mailspaces, roles, goals, work graphs, pools and slots, the pump, the proxy, the project server and console, the `vivi` binary, and the `vivi`, `tugboat` and `speedboat` agent skills |
| `vivi-mail` | IMAP, SMTP, the direct Proton API, sync, send, the local email archive, search, drafts, and the `~/.vivarium/` home |
| `vivi-pty` | The project-scoped PTY runtime adapter |

The three share no code and no data. The mailspace lives at
`<project>/.vivi/`; the email archive lives under the mail root that
`vivi-mail` owns. Do not add a dependency from this repo onto either sibling.

## Current Shape

- Package: `vivi`
- Binary: `vivi`, defined explicitly with `autobins = false`. A `vivi-server`
  link to it runs the same server as `vivi server run`
- Storage: project-local SQLite at `<project>/.vivi/mail.sqlite`, with
  markdown bodies and blobs under the same directory. The schema version is
  `STORAGE_SCHEMA_VERSION` in `crates/vivi-storage/src/storage/schema.rs`
  (12); a binary refuses a store with a newer version
- Project mailspaces: tasks, needs, wants, mail, memos, roles, **goal path
  registry** (pointers to on-disk factory/campaign goal files), and
  **executable work graphs** (Mermaid import, ready frontier, task-attempt
  binding)
- Pools and slots: `vivi pool` dispatches tasks to preseeded machines. A
  mailspace may declare `[[slot]]` entries, one working tree per repository
  and host; `vivi slot` reads the `slot_runs` index of the jobs they ran
- The pump (`vivi pump`) runs the shipped flows (`flows/*.mmd`) over needs
- User-level **model proxy** (`vivi proxy`): one loopback front door that
  path-routes to native Codex/Kimi adapters, supervised children, or an
  already-running upstream. Config is `<config_dir>/vivi/proxy.toml`.
- Project server (`vivi server`): hosted mailspaces, MCP and rooms over HTTP,
  with the read-only web console under `/ui`

### Workspace crates

The root `Cargo.toml` is its own workspace root with the `vivi` package and
the members `crates/*`:

| Crate | Holds |
| --- | --- |
| `vivi-core` | The CLI definitions (`cli/`), mailspace, pump, pools, routing, sync and MCP client. Most code lives here |
| `vivi-storage` | The SQLite message store, schema and `slot_runs` index |
| `vivi-mail` | The letter format and EML composition |
| `vivi-error` | `VivariumError` |
| `vivi-util` | Small shared helpers |
| `vivi-skills` | The skill files embedded in the binary |
| `vivi-proxy` | The model proxy |
| `vivi-server` | Hosted projects, MCP endpoint and rooms |
| `vivi-console` | The read-only web console that the server mounts under `/ui` |
| `vivi-mind` | The hub-wide mind service (`vivi mind`) |
| `vivi-monitor` | The terminal monitor (`vivi monitor`) |
| `vivi-doctor` | The `vivi doctor` checks |
| `vivi-report` | The `vivi report` views |
| `vivi-update` | `vivi update`, `versions` and `uninstall` |

### Separate workspaces and tools

These are not members of the root workspace. `cargo build` and
`cargo test` at the root never compile them.

| Path | What it is | How to test it |
| --- | --- | --- |
| `sim/` | The pump simulator: the real pump loop against a simulated world on a virtual clock (own Cargo workspace) | `cd sim && cargo run --release` |
| `temporal/` | The Temporal runtime for large projects: need, slice and lander workflows and the `vivi-temporal` worker (own Cargo workspace) | `cd temporal && cargo test`; needs the `temporal` CLI on `PATH` |
| `lab/lander/` | Python `compose`, `gate` and a dry-run batch-and-bisect `lander` | `python3 lab/lander/test_lander.py` |
| `deploy/temporal/` | Install scripts for a self-hosted Temporal server and Postgres under systemd | none; read its README |

The pump, flow, pool and slots stay supported for smaller projects. The
Temporal runtime is the option for large ones.

## Agent skills

Agent-facing guidance ships as three skills under `skills/`, and the same
trees are embedded in the binary (`vivi skills list|show|install`):

- [`skills/vivi/SKILL.md`](skills/vivi/SKILL.md): the CLI. Load it for
  mailspace, role, goal, graph, and pool usage. Verify live `--help` before
  exact flags.
- [`skills/tugboat/SKILL.md`](skills/tugboat/SKILL.md) and
  [`skills/speedboat/SKILL.md`](skills/speedboat/SKILL.md): the two operating
  protocols over a Vivi backlog. Each depends only on the vivi skill; the
  operator chooses one.

Skills here are portable. They carry no home-directory paths, host names,
provider model slugs, or names of skills outside this repo; a host binds those
in its own overlay. Describe only behavior the binary ships.

## Work Graphs (project mailspace)

Executable topology lives in project-local `mail.sqlite` tables
(`work_graphs`, `work_graph_nodes`, `work_graph_edges`, revisions, events,
attempts). Mermaid is import/export evidence; readiness is derived from
normalized edges + node state.

| Command family | Role |
| --- | --- |
| `vivi graph import` / `apply` | Atomic create / revise from narrow Mermaid `flowchart` |
| `vivi graph show` / `export` | Mermaid topology only |
| `vivi graph ready` | Compact ready/blocked/active/gates frontier (status loops; `counts` line, `--kind` filter) |
| `vivi graph audit [--repair]` | Backlog citizenship check; repair mints/completes/settles drifted items |
| `vivi graph connect` | Post-hoc prerequisite edge between two backlog items (local only) |
| `vivi need\|task\|want depend` | Add/remove prerequisites after the send; travels as `dependency-added` / `dependency-removed` letters; newest per dependency wins |
| `vivi graph complete` / `activate` | Lifecycle receipts; activate binds a task attempt and refuses operator gates |
| `vivi board --graph` | Frontier projection without replacing task/need board items |
| `vivi need bind` | Lowering: bind unit tasks to a need; join completes the need |
| `vivi step [--apply <handle>]` | Manifest of dispatches/exceptions over the `backlog` graph; apply completes settled items |
| `vivi goal add` / `list` / `show` / `drop` | Register goal file paths; board always surfaces them |
| `vivi trace` | **Communication** tree — not work-graph topology |

**Backlog citizenship.** Every `task` / `need` / `want` send mints an open
node in the per-mailspace `backlog` graph (`source_id` = item handle).
`--depends-on` (task/need/want handles, validated before send) becomes a
prerequisite edge (`graph connect` adds one post-hoc). Lifecycle moves keep
node state in step: done moves complete nodes and unlock dependents; reopen
re-locks; `want promote` never changes node state, and wants never dispatch
in `vivi step` before explicit promotion. The `need bind` join completes
the need's node **and** settles its mailbox item (`via=join`); reopening a
bound unit restores both. Node completions write a `step_decision` graph
event in the same transaction
(`via=lifecycle|graph-complete|step-apply|repair|join`). `graph audit`
detects and repairs citizenship drift — including items sent by vivi
binaries too old to mint nodes (check the version in `mailspace status`;
stale PATH-shadowing installs have caused silent untracked items). No
schema beyond the graph tables.

**Judgment provider (shadow screens).** A mailspace-level `[judgment]` table
in `.vivi/mailspace.toml` (`provider`/`endpoint`/`model`/`timeout_ms`/
`key_cmd`) enables TypeSafe System One screens on `vivi step --apply` only:
one Noul per `done_when` clause plus a completion-honesty Noul, answers
appended to `.vivi/judgment-corpus.jsonl`. Shadow by design — provider
answers never gate mechanical completions. Auth is exclusively `key_cmd`
(`sh -c`, `password_cmd` semantics; no envvar, no inline key).
Absent/failing/timed-out providers degrade to `judgment=skipped(<class>)`.
No read path makes provider calls.

Invariant: Vivi decides which graph nodes are eligible (ready). The Mind
dispatches. Fleet (`prepare --node` → claim → activate) proves execution.
Do not encode Fleet roles inside Vivi core.

## Coding Standards

- Prefer existing modules and helper APIs before adding new abstractions.
- Keep production errors in `VivariumError` with `thiserror`; do not introduce `anyhow`.
- Avoid panics in production paths. Return `Result` where practical, and only use
  `unreachable!` or `unwrap` for truly invariant conditions.
- Use `clap` derive for CLI parsing.
- Use `tracing-subscriber` for logging.
- Use `tokio` for async work; the CLI entrypoint uses `#[tokio::main]`.
- Keep files and functions small. Hygiene tests enforce a 1000-line file ceiling
  and a 60-line function ceiling for checked `src/**/*.rs` and `crates/*/src/**/*.rs` files.
- Prefer `module.rs` files until a module genuinely needs submodules. When it
  does, use `module/leaf.rs` files with a parent `module.rs` rather than
  `module/mod.rs`.
- No inline unit tests in `src/`. The hygiene tests enforce a budget of zero
  inline `#[cfg(test)]` module bodies (`hygiene_no_inline_test_modules`) and
  zero `#[test]`/`#[tokio::test]`/`#[rstest]` attributes in production files
  (`hygiene_no_test_attrs_in_production`). Put unit tests in a companion
  `src/<module>_test.rs` file wired from the production module with
  `#[cfg(test)]` then `#[path = "<module>_test.rs"]` then `mod tests;`
  (companion files ending `_test.rs` or `_tests.rs`, and `tests.rs`, are
  excluded from the production scan), or in `tests/` for integration and CLI
  behavior.

## Entry Point

`src/main.rs` should keep the current shape:

- `main()` installs the broken-pipe guard, dispatches the `vivi-server` name
  to the server before any runtime exists, then parses `Cli`, initializes
  tracing, calls `run(...)`, prints errors to stderr, and exits nonzero on
  failure.
- `run(...)` returns `Result<(), VivariumError>`.
- Library behavior belongs in the `crates/*` members, most of it in
  `crates/vivi-core`. The root package holds the binary and its CLI dispatch
  glue (`src/local_*_command.rs`, `src/remote_mode.rs`).

## Configuration

- The mailspace config is `<project>/.vivi/mailspace.toml`.
- `--project` names the project root; otherwise the nearest ancestor holding
  `.vivi/mailspace.toml` wins.
- A base config (`base = true` in a `.vivi/mailspace.toml` above the project)
  is merged under the project's file; see `crates/vivi-core/src/mailspace/inherit.rs`.
- This repo holds no credentials. User-level state lives under `$VIVI_HOME`
  (default `~/.vivi`: the machine key, the server home) and the platform config
  dir (`vivi/proxy.toml`, the pump service registry).
- Config is `serde` + `toml`; edits that must keep comments use `toml_edit`.

## Validation

Before finishing code changes, run the narrowest useful check first, then widen
as risk increases:

- `cargo fmt --check`
- `cargo test --test hygiene` — mandatory before delivery for any change that
  touches `src/**/*.rs` or `crates/*/src/**/*.rs`; it fails the build on
  inline `#[cfg(test)]` modules
  and on `#[test]` attributes in production files
- `cargo test -p <crate>` for the crate you changed (most code is in
  `crates/vivi-core`), narrowed with a test-name filter while iterating
- `cargo test --workspace` — the full gate. A bare `cargo test` at the root
  runs only the root `vivi` package (970 tests on 2026-11-10) and skips every
  `crates/*` member, `vivi-core` included
- The separate workspaces and tools have their own checks (see Current
  Shape): `cd temporal && cargo test` (needs the `temporal` CLI),
  `cd sim && cargo run --release`, and `python3 lab/lander/test_lander.py`.
  Run them when you change what they exercise; `vivi-core` changes can break
  the `sim/` build, which depends on it

Measured on burgus (now pepper; M5 Max, 18 cores) on 2026-11-10, after the
crate split:

- `cargo test --workspace --quiet --no-fail-fast` (5,846 tests, 14 ignored,
  134 suites): 248 s from a fresh worktree, 229 s after a version bump that
  rebuilds every crate. Under the five-minute line, but close to it.
- `cargo test --quiet --no-fail-fast` (root package only, 970 tests): 156 s
  from a fresh worktree; not a full gate

Measured on 2026-10-20, before the crate split, with a faberlang pump
compiling alongside. The test counts no longer match the layout; treat the
times as upper bounds:

- `cargo test --lib --no-run` from a fresh worktree: 37 s
- `cargo test --lib` (2,907 tests): 46 s run, 68 s with a rebuild
- `cargo test --test hygiene`: 16 s with a rebuild
- `cargo test --test skills_cli`: 16 s with a rebuild
- `cargo test` (then the full suite, 3,189 tests): 351 s

For documentation-only edits, at least inspect links or run a small local
Markdown link scan when the touched file contains links.

## Measured development loop

Measured on burgus (now pepper) on 2026-10-19, before the crate split, with a warm
development target; the `cargo test` rows then covered the whole codebase and
now cover the root package only. These are
wall times, including incremental compilation where applicable. Measure again
when the command or source shape changes materially; do not raise a timeout
to hide a slowdown.

| Command | Wall time | Development timeout |
| --- | ---: | ---: |
| `cargo fmt --check` | 0.79s | 15s |
| `cargo check --quiet` | 17.03s | 60s |
| `cargo build --bin vivi --quiet` | 20.52s | 75s |
| `cargo test --lib --quiet` | 78.98s | 180s |
| `cargo test --quiet` | 190.95s | 270s |
| `cargo test --test hygiene --quiet` | 20.54s | 60s |
| `cargo test --test '*' --quiet --no-fail-fast` | 145.66s | 270s |

The full-gate measurement then passed 3,203 tests with ten ignored. Timings are
baselines, not acceptance receipts for later changes. A full delivery still
requires `cargo test --workspace` and the source hygiene check on the
delivered tip.
Keep one Cargo writer per shared target. Run the final gate from the integration
checkout and refresh generated embedded skills after changing checkout roots.

## Git Workflow

Standing authorization from the repo owner for agent development:

- Develop on a branch, not directly on `main`. Commit per unit of work.
- Once satisfied — the Validation checks above all pass — fast-forward
  `main` to the branch yourself. No PR and no human review is required for
  normal development.
- Fast-forward only: never force-push, never rewrite history on `main`. If
  `main` has moved, merge it into the branch, re-run the checks, then
  fast-forward.
- Report each merge to the user afterward; do not ask first.
- Ask first before: version bumps and `v*` tags (the only CI is the
  tag-triggered release build, so nothing else gates `main`), wire-contract
  changes shared with `vivi-server`, and anything destructive or hard to
  reverse.

## Dependency Policy

Use the dependencies already in `Cargo.toml` when possible. Important current
crates include:

- CLI and runtime: `clap`, `tokio`, `tracing-subscriber`
- Config and errors: `serde`, `serde_json`, `toml`, `thiserror`
- Local message composition and parsing: `mail-builder`, `mail-parser`
- Storage and identity: `rusqlite`, `sha2`, `hex`, `chrono`, `ed25519-dalek`
- Config edits: `toml_edit` (comment-preserving writes)
- HTTP: `axum` and `tower-http` (server, console, proxy), `reqwest`
  (proxy, update, doctor, mind, judgment provider)
- Terminal UI: `ratatui`, `crossterm` (`vivi-monitor`)
- Process probing: `sysinfo`
- Paths: `dirs`

Do not add a new dependency for small local logic that is already covered by
the standard library or existing crates.
