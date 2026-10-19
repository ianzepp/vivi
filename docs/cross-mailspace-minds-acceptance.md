# Cross-mailspace Minds acceptance evidence

This records current evidence for the six October 13 work letters: assignment
and authority (`677218b4`), assigned-need bind and join (`30f742fc`), reopen
and lapse (`9e2de880`), close provenance (`3b1cc8d2`), structured delivery
(`4de5c96b`), and the investigation request (`49fc4a34`). The separate
`39e77392` pool-dispatch question was marked **NOT RULED** and is not part of
these six acceptance items.

The cross-mailspace observations below use the in-repository `FakeServer` with
two isolated temporary mailspaces. They exercise sync and the mailspace API;
they are not a live two-machine server run, and they do not capture spawned
`vivi` CLI stdout or a real Git remote push. Results called **test-backed** are
assertions in the named tests. Results called **source-only** describe code
paths and existing CLI behavior without claiming a live observation. The new
Boot, folder, graph, reopen, authority, and delivery assertions in
`src/sync/delegation_test.rs` are awaiting the parent’s focused test lease.

| Step | Mailspace A (authoring) | Mailspace B (scoped Mind / merger) | Evidence and code path |
| --- | --- | --- | --- |
| A sends a need to its own Mind; B pulls it before assignment | A keeps the authored need and its backlog node. | B has a synced copy held for A’s address; `boot` omits it until assignment. | Boot omission is asserted in `assigned_foreign_need_binds_local_units_and_join_close_travels_home`. Holding classification is in `src/mailspace/holding.rs::holding_of`; the boot inventory is `src/boot.rs::collect_open`. The fake-server test does not capture `need show` or `sync status` text. |
| A assigns the need to B’s Mind; A pushes and B pulls | A’s assignment is a reply letter to the immutable need. | B applies the event, gets a local backlog node, and `boot` lists the active assigned need. | Test-backed in `assigned_foreign_need_binds_local_units_and_join_close_travels_home`; event parsing/folding/projection are `src/mailspace/delegation.rs::parse_assignment_event`, `delegation_state`, and `apply_assignment_projection`; open work lookup is `assigned_work_for_role`; boot includes it through `collect_open`. |
| An unassigned B role attempts to close it | A remains the item author; A’s unrelated `other` role is also refused. | B’s `other` role is refused; only the current delegate may close while assignment is active. | B-side refusal existed in `assigned_foreign_need_binds_local_units_and_join_close_travels_home`; the A-side refusal assertion is newly added and pending focused execution. Write authority is `src/mailspace/item_events.rs::may_close`. |
| B binds two local tasks and accepts both | On A’s next pull, the join close is applied without an acceptance hand step. The item state is `done`, its row is in `done`, and its backlog node is complete. | B writes the bind and local task lifecycle events; the last accepted unit closes the bound need. | State transfer and join are already asserted by that fake-server test; the added A-side folder and graph assertions are pending focused execution. Bind/join paths are `src/mailspace/backlog.rs::backlog_bind_units` and `backlog_complete_if_join_complete`; event application is `src/mailspace/item_events.rs` and `src/mailspace/item_events/emit.rs`. |
| A or an active delegate reopens during assignment | A’s next pull shows the delegate’s reopen as `reopened`. A later operator reopen also returns the need to open work. | B’s folder and graph reopen; `boot` lists the operator-reopened need again. | The author/operator reopen-to-delegate assertion existed in the fake-server test; the delegate-reopen, folder, graph, and Boot assertions are newly added and pending focused execution. Reopen authorization and ordering are `item_events.rs::may_close`, `event_may_speak`, and the event projection. |
| A withdraws or assigns a newer delegate; a delegate closes | A controls assignment history; a later assignment replaces the prior one. A close by the active delegate lapses that assignment. | B’s former role cannot close after withdrawal or replacement; after a lapsed close, the old delegate cannot act until reassigned. | Test-backed in `withdraw_and_newer_assignment_remove_old_delegate_authority` and the join test. The fold is `src/mailspace/delegation.rs::delegation_state`; withdraw/reassign entry points are `withdraw_need` and `assign_need`. |
| A sends a structured delivery need to B’s merge role; B closes with main provenance | After B pushes and A pulls, A’s need thread shows `close_branch = main` and the supplied merge commit. | B’s `boot` lists the open delivery before its merge role closes it. | The two-mailspace Boot and provenance assertions are newly added in `assigned_delivery_need_appears_in_merge_boot_and_close_syncs_home` and await focused execution. The test constructs a delivery-shaped need directly, so it does not claim to execute `vivi delivery send` or push a branch. `tests/delivery_cli.rs` exercises that CLI against a test-local repository/remote. Delivery encoding is in `src/mailspace/delivery.rs`; close metadata is in `src/mailspace/item_events/emit.rs`. |
| An unattended push stops | `vivi sync status` can report queued letters, but this lane did not capture a running CLI status screen. | On the Mind machine, use periodic `vivi sync --drain`; on the authoring machine, run it when the laptop wakes. A 401/network stop is non-zero and leaves letters queued. | Command sequencing is `src/local_sync_command.rs::run_passes`; stop formatting and unsent counts are in `src/sync/push.rs::PushReport::lines` and `unsent`. Focused 401/drain tests exist in `src/sync/push_test.rs`; this is not a live server probe. |

## Acceptance disposition

- **49fc4a34, investigation:** useful two-checkout behavior is covered by the
  fake-server tests, with newly added assertions for folder state, graph state,
  operator/delegate reopen, Boot visibility, and delivery close provenance.
  A real two-machine CLI transcript and captured `sync status` remain
  unverified; the table marks those gaps instead of presenting source review as
  a live probe.
- **677218b4, assignment:** current code and fake-server tests cover newest
  assignment wins, withdrawal, delegate close authority, refusal of an
  unassigned delegate role, and backlog visibility. New pending assertions
  cover unrelated author-side roles and local hand-kind rejection in
  `assign_need`.
- **30f742fc, bind and join:** current code and the fake-server test cover
  binding an assigned foreign need to B-local tasks and syncing the automatic
  join close to A. The work-graph flow and Mind sync recipe are documented in
  `skills/vivi/references/work-graph-flow.md` and `skills/vivi/SKILL.md`.
- **9e2de880, review reopen:** the operator’s selected lapse rule is
  implemented: a delegate close lapses authority; an author/operator reopen
  after that requires a fresh assignment. Reopen by the author, an
  operator-kind role in the authoring mailspace, or the active delegate is
  documented. Newly added two-mailspace reopen assertions still need the
  focused run.
- **3b1cc8d2, provenance:** pair validation, syncable close metadata,
  clearing on reopen/location-free close, and joined-unit provenance are
  covered by `tests/item_events_cli.rs`, `tests/delivery_cli.rs`,
  `src/sync/events_test.rs`, and `src/mailspace/backlog_test.rs`. A joined need
  keeps a pair only when every unit has a pair and all branches match; it uses
  the latest unit close’s commit.
- **4de5c96b, delivery:** the delivery is a structured need assigned to the
  merger, and the command records push outcome, branch/tip/base, gate, closed,
  blocked, and newly filed work. The local delivery CLI test covers the real
  command; the new two-mailspace test covers Boot visibility and the synced
  merge close. Its focused run is still pending.

This evidence distinguishes tested fake-server behavior, inspected code paths,
and the remaining live CLI/server probe. No external mail server or shared
mailspace was mutated for this acceptance pass.
