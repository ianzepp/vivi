# Vivi 21.2.0

Minor release on 21.1.0. Projects on a host get one layout and one owner,
and can be deleted. Pairs with vivi-server 2.2.0. The letter and pump
formats are unchanged.

## One layout: lanes per project

- A project's lanes live under `<lanes_root>/<project>/<target>.git` and are
  reached at `<prefix><project>/<target>.git`. Before, every lane sat flat in
  the lanes root, so two projects with the same target id collided.
- `vivi project create <name>` makes the project's first target (`--target`,
  default the project name) with its checkout `<project>/<target>` and its
  `[pump.targets.<target>]` entry (`remote_url` and `main` set). Before, it
  made a bare repo named after the project that no target used, which also
  collided with a target of the same name.
- `vivi project lane add` makes `<lanes_root>/<project>/<id>.git`, creating
  the project's lanes directory when an older project has none.
- Checkouts are cloned from the lane's local path and `origin` is then set to
  the lane URL, so creating a project or a lane never needs the network.

## One owner: split-owner mode is gone

One user owns the projects root and the lanes root. The halves mode of
`project create` and `lane add` (probing each root, doing the writable half,
printing a command for "the other owner") and the sibling ACL copy are
removed. Group and ACL come from the lanes directory itself: a setgid lanes
root with default ACLs hands both to everything made inside it.

Library: `create_split`, `create_project_half`, `create_lane`, `LaneCreate`,
`LaneCreated`, `SplitCreated`, `RemainingHalf`, `Half` and
`lane_add::add_split` are removed; `lane_add::add` replaces `add_split`.
`ProjectCreated` gains `lanes_dir`, `target` and `checkout_path`.
`LaneAdded` is flat: `{target_id, repo_path, checkout_path, git_url,
initial_commit}`. New: `project_create::readiness_of`, `lane_url`.

## `vivi project delete`

`vivi project delete <name> --projects-root P --lanes-root L --confirm <name>`
unregisters the project, then removes `P/<name>` and `L/<name>/`. While a
pump still holds the project's lock it stops after unregistering (the pump
service stops that pump within a round) and asks to be run again; a repeated
call finishes what an earlier one left.

## Fix: targets on projects whose `[pump]` is inherited

`vivi pump target add` (and project creation) refused with "no [pump] table"
on a project whose `[pump]` came from a base mailspace. It now writes an
implicit `[pump]` that holds only the targets and merges over the base.
