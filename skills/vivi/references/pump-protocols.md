# Pump protocols

This reference describes the job protocol between the pump and the pools: job identity, briefs, restart recovery and runner publication. The pump creates and persists each role job before it selects a pool. The pool runs the task and returns a receipt; the controller validates and applies it.

The shape of every letter is in [envelope](envelope.md): the pump record, the operator action, the pump notes, the `[job]` and `[step]` tables, and the reply each role sends. See [pump contract](pump-contract.md) for control, role routing, the flows and restart behavior.

## Job dispatch and identity

Each durable job has a stable ID, kind, target where applicable, recipient role, candidate pools, brief, and collector payload. The pump saves the job before it picks a pool. Repeated queue attempts with the same ID and identical inputs are idempotent; reusing an ID for different inputs is an error.

The pump dispatches pending jobs using the current role routing and pool fallback chains. A job with no eligible free slot remains pending. The pool receipt is collected on later ticks and applied once.

## Job briefs

A job reaches its worker as mail, in one of two forms. Both carry the job's `[job]` table (`id`, `kind`, optional `builtin_role`; see [the `[job]` table](envelope.md#the-job-table)).

- A job with no task of its own is sent as a task. The task front matter holds the `[job]` table, the subject starts with **[pump-job:&lt;id&gt;]**, and the body is the brief.
- A job for a flow step that already has a task is briefed with a **brief** note: a `mail` reply on that task from the pump identity, with `[pump] note = "brief"` and the `[job]` table in its front matter. A task that already carries a brief note for the job id is not briefed again, so a retried dispatch posts nothing new.

A brief that would pass the letter cap (64 KiB) is cut before it is sent. The head and the tail stay, the middle is replaced by a marker, the full text is saved as **job-briefs/&lt;id&gt;.full.txt** in the pump home, and a **job-brief-truncated** exception records the cut.

## Restart recovery

The pump can stop after it sends a task and before it saves the task handle. On restart it recovers each unfinished job:

- A job with a saved handle: the pump reads the task's latest receipt and records the dispatch, or the terminal pool result, instead of sending again.
- A pending job with no handle: the pump lists the addressed role's tasks and looks for the job. A task matches when its subject carries **[pump-job:&lt;id&gt;]** or its `[job]` table names the id. The pump reattaches that handle and reads the existing result instead of sending duplicate work. A task whose subject matches but whose body does not name the job is an error.

## Runner publication

Only the pool runner publishes. The pool dispatcher passes a publish ref to the runner; the runner-only **PUBLISH_REF**, **PUBLISH_EXPECT** and **PUBLISH_REMOTE** settings are not exposed to the agent. The agent never pushes.

After a clean agent exit the runner reads the candidate commit, checks that the ref is a branch and that the commit equals the optional expected SHA, performs a plain fast-forward push, fetches the ref back, and verifies the remote commit and ancestry. A ref under `vivi/need/` published with an expected SHA is create-only: an existing ref at another tip is refused, and the same tip counts as already published. The collected pool summary records **runner publish: &lt;state&gt; &lt;ref&gt; &lt;candidate-sha&gt; &lt;remote-sha&gt;**.

The controller requires that receipt, the exact ref and candidate, and its own read of the remote ref. A worker's claim that it moved a branch is not proof.

Which role may publish which ref:

| Role kind | Allowed refs |
| --- | --- |
| merge | the target's configured main branch; `refs/heads/vivi/need/<x>`; `refs/heads/vivi/need-prior/<x>`; `refs/heads/pump/merge-main/<x>` |
| hand | `refs/heads/vivi/unit/<x>` |
| repairer | `refs/heads/vivi/repair/<x>` |
| release | any branch, with an exact expected commit SHA |

`<x>` is one path segment. The runner can also delete a branch it owns (`vivi/need/`, `vivi/need-prior/`, `vivi/unit/`, `vivi/repair/`, `pump/merge-main/`), and only with a compare-and-delete guard on the expected tip. The pump does not `git push` these refs itself.

## Cadence, digest and research jobs

- **Cadence** and **digest** jobs are addressed to the role whose schedule is due. They always run read-only; their output is mail (wants and needs).
- **Research** needs are filed by head-ceo with **vivi need send --mark research --explores &lt;want&gt; --done-when "..."**, then **vivi pump send --need &lt;need&gt;** (mark first; the open pump record starts the run). The research flow runs the researcher and posts the analysis on the want.
