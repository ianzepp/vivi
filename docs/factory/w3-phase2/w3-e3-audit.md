# W3-E3 audit — split-owner lanes (wave 3 phase 2)

- **Auditor**: independent, read-only; did not implement any of W3-E3.
- **Need branch**: `need/w3-e3` at `.claude/worktrees/need-w3-e3`, tip `ce53b9dc`, base `470225f4` (v21.0.0).
- **Commits audited**: `fe72143a` (project create split), `203d3695` (lane add), `18b32fdc` (sibling ACL copy), plus their no-ff merges `21534359`, `32e880bc`, `ce53b9dc`.
- **Authority**: `docs/factory/wave3-completion-goal.md` §3.6 (target + done_when), §6.2 W3-E3 probe row, and the audit scale in §5 step 6 (lines 846-849). The file has no `### 5.6` heading; that step is what this campaign's audits call §5.6. P1/P2 block the need; P3s go to the ledger; CLEAN and RESIDUAL both require no open P1/P2.
- **Scope**: diff `470225f4..ce53b9dc` only. Targeted tests run in the need worktree. No repo file edited. Full suite not re-run (parent: rc 0, 231.7s on `ce53b9dc`).

## 1. What landed

14 files, +3000/−138. No `Cargo.toml` / `Cargo.lock` change.

| Piece | Where |
| --- | --- |
| Split `project create` | `create_split` / `create_split_at` probe both roots and return `SplitCreated.remaining` (`src/project_create.rs:305-360`). CLI calls `create_split` (`src/local_project_command.rs:27`). `create` / `create_at` still do both halves with no probe (`src/project_create.rs:242-255`). |
| Public halves | `create_lane` (`src/project_create/lane.rs:79`), `create_project_half` (`src/project_create.rs:266`). |
| `vivi project lane add` | `lane_add::add_split` (`src/project_create/lane_add.rs:122`). `--lanes-root` required; `--git-url-prefix` optional; `--seed` optional. No `--repo-only` / `--lanes-only` / `--projects-only` flag anywhere in `src/`. |
| Guarded target write | `run_target_steps` calls `pump::config::targets::add` (`src/project_create/lane_add.rs:281`), which is `config_edit::guarded` (`src/pump/config/targets.rs:77-89`) and refuses while the pump is running (`src/mailspace/config_edit/guard.rs:153-161`). |
| ACL copy | `src/project_create/lane/acl.rs`, invoked after `git init` and before the seed commit (`src/project_create/lane.rs:134-136`). |
| Skills | `skills/vivi/SKILL.md` project section. README does not list `project create`; nothing to update there. |
| `valid_target_id` | Visibility-only promotion (`src/pump/config/validate.rs:284-292`). The predicate body is unchanged. |

Recorded deviations, matched in the code and the skill, not re-opened: `--lanes-root` is required (commit `203d3695`, ledger); `create` / `create_at` stay both-halves so existing `vivi-server` callers are unchanged (commit `fe72143a`).

## 2. Correctness

### 2.1 The three ownership cases hold

Writability is a real create/remove probe. Only `ErrorKind::PermissionDenied` is "not writable"; every other `create_dir` error aborts (`src/project_create.rs:701-713`). A missing root never reaches the probe: `Roots::of` / `existing_dir` canonicalize first and return "not an existing directory" (`src/project_create.rs:310-311`, `462-466`, `776-783`; lane add does the same at `lane_add.rs:131-132`). `bad_inputs_are_refused_before_anything_is_made` still refuses a missing projects root through that helper (`src/project_create_test.rs:243-247`). There is no test that injects a non-permission probe error (for example `AlreadyExists`); the match arm is the whole policy and it does not map those to `Ok(false)`.

chmod `0555` is the stand-in for the other user. It is not a second uid, and it is unix-only. On this host the tests pass, so the probe treats that mode as unwritable and the other half runs.

**Lane add** (the §3.6 done_when cases):

| Case | Test | What it actually asserts |
| --- | --- | --- |
| Both writable | `both_halves_make_the_repo_the_checkout_and_the_target` (`lane_add_test.rs:130`) and CLI `lane_add_attaches_the_lane_and_prints_it` (`tests/project_cli.rs:263`) | Bare repo, `core.sharedRepository` group, checkout, `[pump.targets.<id>]`, no `remaining`. CLI stdout has no `remaining`. |
| Lanes only | `a_read_only_project_root_runs_only_the_lanes_half` (`lane_add_test.rs:208`) | Repo and seed exist; checkout and target do not; `remaining.half == Projects`; `remaining.command` is the exact `vivi project lane add … --lanes-root … --git-url-prefix … --seed` string. |
| Projects only | `a_read_only_lanes_root_runs_only_the_projects_half` (`lane_add_test.rs:246`) | Pre-made repo is cloned; target id is written; `remaining.half == Lanes` and the command string is exact. |
| Neither | `no_writable_root_is_an_error_that_names_both_roots` (`lane_add_test.rs:284`) | Error names both roots; repo, checkout, and target are absent. |
| Printed command is runnable | CLI `the_printed_command_performs_the_other_half` (`tests/project_cli.rs:315`) | Lanes-only run, then the printed line is tokenized and re-executed with the lanes root made unwritable. Second run clones, writes the target, and still prints the lanes-half line. |

**Project create** has the same three shapes, exact command strings included: `split_runs_both_halves_when_both_roots_are_writable`, `an_unwritable_lanes_root_runs_only_the_projects_half`, `an_unwritable_projects_root_runs_only_the_lanes_half` (`src/project_create_test.rs:446`, `:467`, `:500`). The lanes-only case also pins the seed committer resolved from the base above a projects root this run cannot write (`:500-528`). CLI covers the projects-only create case and checks the printed command contains the canonical roots (`tests/project_cli.rs:131-176`). It does not re-exec that command; the library test's `assert_eq!` on `remaining.command` is the strict pin.

### 2.2 Rollback

Only paths this run created are removed.

- Project create, lanes-only failure at `Repo` and `Commit`: repo gone, project absent, registry untouched (`a_lanes_half_failure_rolls_back_only_the_repository`, `project_create_test.rs:570`).
- Project create, projects-only failure after another user already made the repo: project and registry gone, that repo kept (`a_projects_half_failure_leaves_a_repo_it_did_not_create_alone`, `:586`; real pool-miss path `a_values_failure_in_split_mode_leaves_the_lanes_owners_repo`, `:607`).
- Lane add, both writable, injected failure at `Lane`, `Clone`, and `Target`: repo, checkout, and target all absent (`a_failure_at_each_step_leaves_nothing_behind`, `lane_add_test.rs:306`).
- Lane add, projects-only, failure at `Clone` and `Target`: the other user's repo stays; checkout and target do not (`a_projects_half_failure_leaves_the_lanes_owners_repo_alone`, `:321`).
- Running pump: error contains `the pump is running`; repo, checkout, and target are absent (`a_running_pump_refuses_the_target_and_rolls_everything_back`, `:342`). The refusal is `guarded` → `refuse_while_running`, before the config write (`guard.rs:71`, `:153-161`). The clone is pushed onto the rollback list before `git clone` (`lane_add.rs:277-281`) and removed with the repo when `both()` fails (`lane_add.rs:184-191`).

`Step::Clone` inject fires before the clone, so a real `git clone` failure is not what that loop proves. The `Target` inject fires after a successful clone and does prove the checkout is removed. `Made::roll_back` ignores `NotFound` and appends any other removal error (`lane_add.rs:383-399`).

### 2.3 Second add, per artifact

`a_second_add_for_the_same_target_is_refused` (`lane_add_test.rs:369`) pins the recorded rule: an existing repo is refused by name; with the repo deleted and the target entry left, the error is `pump target `sandbox` already exists`, the fresh repo this attempt created is rolled back, and the old checkout is untouched; with the entry removed and the checkout left, the error names `target checkout` and nothing new remains. Refusals happen before the clone (`lane_add.rs:268-276`). Bad ids and `http(s)` prefixes are refused with nothing created (`lane_add_test.rs:411`). A projects-only run whose repo does not exist yet errors with `the lanes half must run first` and creates nothing (`lane_add_test.rs:439`, `lane_add.rs:271-276`).

That rule has a sharp edge, recorded rather than accidental: a caller who can write **both** roots cannot finish a half the other owner already created. `both()` always starts at `create_lane`, which `refuse_existing`s the repo (`lane_add.rs:179-183`, `lane.rs:84`). The split-owner path works only because the second user fails the writability probe and takes `projects_only` / `lanes_only`. The CLI re-exec test sets that up with chmod. Not a done_when miss.

### 2.4 No half-only flags

`ProjectCreateCommand` and `ProjectLaneAddCommand` (`src/cli/project_command.rs:32-91`) have no `--repo-only` or equivalent. The only `repo-only` hit in the tree is the goal doc forbidding it, plus an unrelated pool-config fixture.

## 3. done_when and the tests diff

§3.6 bullets against the code:

- Three ownership cases for `lane add`, including the printed command — met, §2.1. `project create` has the same split — met.
- A failure leaves nothing this run created — met, §2.2, including the guarded running-pump refusal.
- Full suite green — parent record rc 0, 231.7s on `ce53b9dc`. Not re-run here. Targeted suites below are green.
- Audit CLEAN/RESIDUAL with no open P1/P2 — **not met**. One open P2 (§7).

§6.2 row: the three ownership cases are in temp-dir tests and passed here. The Linux ACL copy test did not run (below).

`git diff 470225f4..ce53b9dc -- tests/` changes one pre-existing assertion. `create_prints_the_result_and_registers_the_project` no longer requires empty stderr; it requires stderr to contain `lane ACL copy skipped` (`tests/project_cli.rs:51-54`). That is the off-Linux skip warning from `18b32fdc`, not a loosened success check: stdout JSON, registration, and the refusal path are unchanged. `create_text_output_names_the_missing_items` never asserted empty stderr. `src/project_create_test.rs` loses only a `use` line; every other old assertion is intact. New tests are additions.

## 4. Tests run in the need worktree (tip `ce53b9dc`)

Warm tree. Host rustc 1.99.1 (Homebrew). No second cargo writer.

| Command | Result | Wall |
| --- | --- | --- |
| `cargo test --lib project_create` | ok — 52 passed, 0 failed, 0 ignored | 1.20s |
| `cargo test --test project_cli` | ok — 7 passed, 0 failed | 0.30s |
| `cargo test --test skills_cli` | ok — 6 passed, 0 failed | 0.17s |
| `cargo test --test hygiene` | ok — 13 passed (inline-test, test-attr, 1000-line file, 60-line function) | 0.21s |
| `cargo check --all-targets --quiet` | rc 0 | 15.28s |

The 52 lib names include every ownership, rollback, second-add, pump-refusal, and ACL decision test cited above. They do **not** include `linux_copies_group_and_acl_from_the_lexicographic_sibling`. It is not ignored; it is not in the binary. `cargo check --all-targets` on this Mac compiles the `not(target_os = "linux")` arms only. I did not reproduce the ledger's `x86_64-unknown-linux-gnu` check: this machine has Homebrew rustc and no rustup target, and I did not install one.

New compiler warning from this diff, on both test and check: unused parameter `repo` at `src/project_create/lane_add.rs:200`. Pre-existing warnings (`fetch_update`, dead pump-amend code, `unused_must_use` on `unclaim`) are the ledger's follow-ups, not this need.

## 5. API for W3-D4

Library functions take plain structs and return values. None of them print. None of them take a clap struct.

| D4 need | Call | Prints? | Trip |
| --- | --- | --- | --- |
| `lane_create` = projects half of a lane (open question in §3.5) | `lane_add::add_split(&LaneAdd) -> LaneAdded` | No. Surface `remaining.command` yourself. | There is no public "projects half only" function. `add_split` probes. If this process can create a directory in the lanes root, it also creates the bare repo. If it cannot, it clones and adds the target **only when `<lanes>/<id>.git` already exists**; otherwise it errors `the lanes half must run first` and does not return the command (`lane_add.rs:271-276`). `create_project_half` is the wrong function: that is the mailspace/registry half of `project create`, and it never clones or writes `[pump.targets]`. |
| `project_create` tool, today documented as `project_create::create` | `create` still requires both roots and does not split (`project_create.rs:242-255`) | No | On a server user who cannot write the lanes root, `create` fails the lanes half instead of returning the command. The split entry is `create_split` / `create_split_at`. The projects-only entry, with an explicit registry path and no probe, is `create_project_half` (`project_create.rs:266`). The skill still says the library entry point is `create` (`skills/vivi/SKILL.md:197`) and only describes the split as CLI behaviour (`:199-202`). |
| Lanes half alone | `create_lane(&LaneCreate)` | No | ACL skip/failure is `tracing::warn!` only (`acl.rs:163-166`). `LaneCreated` has no warning field. A server process whose subscriber does not show `vivi` warnings loses it. |
| Audit line for the target write | `targets::add` returns `GuardedWrite` (backup path + still-invalid notes) | — | `run_target_steps` discards it (`lane_add.rs:281`). `LaneAdded` cannot tell D4 where the backup went. |

`LaneAdd`, `ProjectCreate`, `LaneCreated`, `ProjectHalfCreated`, `SplitCreated`, `RemainingHalf` are all public and `Serialize` where the CLI JSON path needs them. `writable_root` is private, which is right: a server tool should not reimplement the probe if it calls `add_split` / `create_split_at`.

## 6. ACL slice

Decision logic is tested on this host (all of these passed):

- Lexicographic first readable sibling, including an unsorted listing (`lane_test.rs:23`, `:38`).
- Off Linux and missing tools do not consult the readable callback; the callback panics if they do (`:45`, `:61`). `this_host_reports_whether_it_can_copy_an_acl` expects `HostKind::NotLinux` here (`:179`).
- Empty listing, unreadable sibling, and a failed listing become one flat warning line (`:77`, `:94`, `:106`).
- A failed copy is `AclCopy::Skipped` with `the lane was still created`, not an error (`:128`). `settle(Ok)` is `Copied`.
- `clean_acl` drops comments, effective-rights notes, and blank lines (`:150`).
- `list_siblings` keeps only other `*.git` directories (`:159`).
- A failed copy injected after `git init` still returns the lane and still makes the seed commit (`an_acl_copy_failure_does_not_fail_lane_creation`, `:188`).
- A real `create_lane` on this host skips with `NOT_LINUX` and still creates the repo (`off_linux_creation_skips_acl_copy_and_still_creates_the_repo`, `:223`). `apply` is what the production path calls (`lane.rs:114`).

Cfg gating of the runtime test is real in one direction: `linux_copies_group_and_acl_from_the_lexicographic_sibling` and its helpers are `#[cfg(target_os = "linux")]` (`lane_test.rs:242-350`). This Mac neither runs nor ignores it. The Linux-only copy (`getfacl` / `setfacl` / `chgrp`, `acl.rs:268-426`) is not compiled by the host check I ran.

It is **not** a real gate against a silent pass on Linux. The test returns success, after an `eprintln!` cargo hides on a passing test, when the three programs cannot be spawned or when `setfacl` is rejected by the filesystem (`lane_test.rs:245-262`). §6.2's "on Linux the ACL copy test" can be green with no ACL copied. That is P2 below.

Best-effort failure is pinned, separate from that test: a copy error becomes one warning line and the lane, including its seed commit, still exists (`acl.rs:123-133`, `lane.rs:134-142`, tests cited above). Off Linux, `plan` returns `NOT_LINUX` without listing siblings (`acl.rs:86-90`, `:239-241`).

**Known limitation (execution):** this lane did not execute the Linux copy. No Linux target is installed here. The ledger's cross check was not reproduced. Fixing the P2 does not by itself produce a Linux run.

Directory scope, when the test does assert: `chgrp --reference` and `setfacl --set-file` run on the repo directory only (`acl.rs:279-294`). The comment says named ACLs are not default ACLs, so they do not apply to files `git init` already created. The Linux assertions compare `getfacl` and gid of that directory, not of `HEAD` or objects (`lane_test.rs:270-272`). Hub's lanes root has named ACLs and no defaults (ledger). A directory-level copy does not put those named entries on the files inside the new repo. P3, not a second P2: the goal text and the test both describe the repo directory, and the code does that.

## 7. Hygiene

- No `#[test]` / `#[cfg(test)]` module body in production files beyond the allowed `#[cfg(test)] #[path] mod tests` wiring. `hygiene_no_inline_test_modules` and `hygiene_no_test_attrs_in_production` passed.
- File sizes: `project_create.rs` 843, `lane_add.rs` 405, `acl.rs` 427, `lane.rs` 203, `local_project_command.rs` 147, `project_command.rs` 91. All under 1000. `hygiene_no_file_over_line_limit` passed. Longest new functions are under the 60-line ceiling (`hygiene_no_function_over_line_limit` passed). The 52-line `findings_for` in `validate.rs` is pre-existing and untouched.
- No `unwrap` / `expect` / `panic!` / `todo!` / `unreachable!` in the new production paths. `unwrap_or` / `unwrap_or_default` only. Tree-wide unwrap/expect/panic budgets still pass.
- No new dependency.

## 8. Findings

**P1** — none. The three ownership cases, the printed commands, rollback of only this run's paths, per-artifact second-add refusal, and the guarded `targets::add` path are implemented and pinned by tests that passed here.

**P2**

1. **The Linux ACL runtime test can pass without copying an ACL.** `linux_copies_group_and_acl_from_the_lexicographic_sibling` (`src/project_create/lane_test.rs:242-263`) `return`s when `getfacl` / `setfacl` / `chgrp` fail to spawn, and again when `setfacl` is rejected by the filesystem. Cargo captures `eprintln!` on success, so the skip line is not in the passing summary. §6.2 then sees a green test and no copy. The `#[cfg(target_os = "linux")]` attribute is real — the test is absent on this Mac (52 passed, 0 ignored, name not listed) — but that only stops a macOS vacuous pass. It does not stop a Linux vacuous pass. Fail the test (or `#[ignore]` it so the harness counts ignored, not passed) when the environment cannot perform the copy. Best-effort semantics and the sibling-choice decision are already pinned on every host (§6); this P2 is the runtime proof's teeth, not a shown wrong copy.

**P3** (ledger)

1. **ACL copy is the repo directory only** (`src/project_create/lane/acl.rs:279-294`). Files created by `git init` before `chgrp` / `setfacl` keep the creator's group and gain no named ACL. Hub has no default ACL, so those named entries are not inherited by later objects either. The Linux test, when it asserts, compares the directory (`lane_test.rs:270-272`).
2. **Lane-add's printed command is not canonical.** `remainder_command` shells `request.project_root` and `request.lanes_root` as given (`lane_add.rs:353-366`). Project create shells the canonical roots (`project_create.rs:663-668`). A relative `--lanes-root` is not a command the other user can run from another cwd. The ownership tests pass already-canonical paths, so they do not catch it. The "no write permission" line uses the canonical root, so it can disagree with the command under it.
3. **ACL outcome is not in the return value.** `acl::report` is `tracing::warn!` (`acl.rs:163-166`). `create_lane` / `create` drop it. Off Linux every lane creation warns. The CLI shows it because the subscriber is `vivi=info` on stderr (`src/main.rs:54-62`); the updated CLI test requires the substring (`tests/project_cli.rs:51-54`). A server caller does not get a structured warning (§5).
4. **New unused parameter.** `lanes_only`'s `repo` (`lane_add.rs:196-201`) is unused. `cargo test` and `cargo check --all-targets` both warn. Existence is still checked inside `create_lane`; this is a dead argument, not a missed refusal.
5. **D4 name trap, recorded in part.** `project_create::create` does not split (§5, commit `fe72143a`). The skill still names `create` as the library entry (`skills/vivi/SKILL.md:197`). `add_split` drops `GuardedWrite` (`lane_add.rs:281`). A both-writable retry cannot complete an already-created half (§2.3); that matches the recorded per-artifact refusal and is pinned.

## 9. Verdict rationale

The split itself is what §3.6 asked for. Temp-dir tests prove all three ownership cases, exact remainder commands for absolute paths, rollback that leaves another user's repo alone, per-artifact second-add refusal, and a running pump that refuses through `targets::add`'s guarded path and leaves nothing this run created. The one pre-existing `tests/` expectation edit is the off-Linux ACL warning, justified by `18b32fdc`. Hygiene ceilings hold, and there is no new dependency and no half-only flag.

The need is not CLEAN or RESIDUAL: §6.2's Linux ACL probe is a test that reports success when it did not copy, which is an open P2. The un-executed state of that test on this Mac is real and stays even after the P2 is fixed; it is not, by itself, the block.

VERDICT: BLOCK

---

## 10. Pass 2 — W3E3-4 (`7fe9a071`, need tip `d6b44dcb`)

Pass 1's P2 is closed. Diff `ce53b9dc..d6b44dcb` touches only `src/project_create/lane_test.rs` and `src/project_create/lane_add.rs`.

**What changed.** `linux_copies_group_and_acl_from_the_lexicographic_sibling` (`lane_test.rs:242-293`) no longer returns. `linux_require_acl_tools` (`:296-306`) `assert!`s if `getfacl`, `setfacl`, or `chgrp` fails to spawn. `linux_must_setfacl` (`:309-321`) panics if `setfacl` cannot start and `assert!`s on a non-zero status. After the plant, `linux_require_acl_entry` (`:324-329`) fails if `getfacl --omit-header --numeric --no-effective` does not contain the exact line `user:<uid>:rwx` on `a.git` and `user:<uid>:r-x` on `m.git`. The two fixture bodies must differ (`:262`). `create_lane` then has to come back equal to `a.git`, unequal to `m.git`, and still carrying `user:<uid>:rwx` (`:274-280`). Owning group is the `# group:` line from `getfacl --numeric` (`linux_owning_group`, `:367-386`), and the new repo's group must equal `a.git`'s (`:281-285`). There is no `return`, no `eprintln` skip, and no `#[ignore]`.

On a Linux host that has the three programs, a missing tool, a rejected `setfacl`, a no-op `setfacl` (success with the named entry absent), or a copy that is not `a.git`'s ACL is a test failure. The vacuous-green path from pass 1 is gone. `#[cfg(target_os = "linux")]` is unchanged, so this Mac still does not compile the test.

**Group residual, accepted.** `linux_chgrp_other` (`:353-364`) returns false when `id -G` has no gid other than the primary, and also when `chgrp` itself fails. Only then is the inequality against `m.git` skipped (`:286-292`). The equality against `a.git` still runs, and it does not prove a copy if `a.git`'s group was never changed. The ACL body assertions are not gated on this flag, so the named-ACL proof still runs. Slightly wider than "single-group user": a `chgrp` refusal takes the same path.

**P3-4 closed.** `lanes_only` no longer takes `repo` (`lane_add.rs:196-200`). The caller dropped it (`:149`). `cargo test` and `cargo check --all-targets` no longer warn on that parameter.

**Still not executed.** The test name is absent from the 52 lib tests (0 ignored). This host's `cargo check --all-targets` does not compile the Linux cfg. The private-rustc typecheck named in the dispatch note was not reproduced here.

**Checks on `d6b44dcb`** (need worktree, warm enough to recompile the lib):

| Command | Result | Wall |
| --- | --- | --- |
| `cargo test --lib project_create` | ok — 52 passed, 0 failed, 0 ignored | 44.71s |
| `cargo check --all-targets --quiet` | rc 0 | 16.65s |

Pass-1 P3s 1, 2, 3, and 5 are unchanged (directory-only copy, non-canonical lane-add command, tracing-only ACL warning, `create` does not split). P3-1's old line cite `lane_test.rs:270-272` is stale after this edit; the directory ACL compare is now `lane_test.rs:274-280`. Production `acl.rs:279-294` did not move.

**P1** — none.

**P2** — none. Pass 1's P2 is fixed in `7fe9a071`.

**P3** — pass 1 items 1, 2, 3, and 5, plus: the Linux runtime test is still un-executed in this lane; a host that cannot `chgrp` to a second group only equality-checks the owning group (`lane_test.rs:263-292`, `:353-364`).

Pass 2 verdict is RESIDUAL, not CLEAN, because §6.2's Linux ACL probe has still never run. The test will no longer lie if it does run. That is not an open P2.

VERDICT: RESIDUAL
