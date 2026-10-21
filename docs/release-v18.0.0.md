# Vivi 18.0.0

Major release on 17.0.2. The history the pump and pool write into a project's
repository no longer carries Vivi's tracker handles. There is no switch: this
is how every project behaves from now on.

## Breaking: commits carry no handles

Before 18.0.0, a pool job's commit subject was the task subject, such as
`[3ea8dee9-1 a1] Retire the old surface`, with a `Pool-Run: slot=... model=...`
trailer. Merge agents committed with git's default messages, which name
branches (`Merge remote-tracking branch 'origin/vivi/unit/...'`). Need, unit,
attempt and job identifiers and branch names therefore ended up in history
that lives far longer than the mailspace they point into.

From 18.0.0:

- **Commit subject.** The runner commits with the task subject's leading
  `[...]` tags removed (`Retire the old surface`), or `Vivi pool work` when
  nothing is left. A failed attempt is `WIP (attempt failed): <subject>`.
  The task mail keeps its tagged subject; the pump still matches
  `[pump-job:<id>]` there.
- **No trailer.** The `Pool-Run:` trailer is gone. Which slot, model and base
  produced a commit stays in the pump's attempt records.
- **Plain messages from agents.** Every pool brief says to write plain commit
  messages with no identifiers or branch or ref names. Merge and landing briefs
  ask for `git merge --no-ff --no-edit -m "<plain summary>"`.
- **The runner refuses to publish handles.** `params.env` carries `FORBID`:
  the job's task handle, need id, job id, the words of the subject's tags, its
  own branch, each `--with` source ref and the unit ids in their names, a
  tracker-owned base ref, and `refs/vivi/with/`. Before any publication, and
  before reporting a local slot's commit, the runner reads the messages of the
  commits being published and refuses if one contains a token. The outcome is
  `handles_in_history`, with the token and commit in the run log; if the
  commits cannot be listed it refuses as well.
- **Pump job subjects** are reworded without need, unit or quarantine ids
  (`merge units into their need branch`, `land an accepted need on <target>`,
  `Repair a quarantined batch (<n> units)`, and so on).

Nothing in Vivi reads handles back from git: landing, merge and promotion
checks use commit ids and exact refs, and attempt numbers come from branch
names, which are unchanged.

## Upgrading

Work started under 17.x carries handles in its commits. Its merges are refused
with `handles_in_history` once 18.0.0 runs. Withdraw and replan such needs, or
let the refusal surface them. Existing history is not rewritten.
