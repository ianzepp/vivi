# The reply envelope

Every role reply in the pump is Markdown that opens with TOML front matter
between two `+++` fence lines. The front matter carries the answer word and the
role's typed fields. The prose report goes below it. The pump reads the front
matter and nothing else to decide what happens next; the prose is for the next
reader.

This file is the general rule and the full table. A role's own charter states
that role's fields and one example, so the charter is enough for a worker. Read
this file when you need the whole table, or when a reply was refused.

## Shape

```
+++
answer = "built"
change = ["src/a.rs"]
+++
Prose report.
```

## Rules

- The envelope opens the final message. Leading blank lines are allowed; nothing
  else may come before the first fence.
- There are exactly two `+++` fence lines: one opens the front matter and one
  closes it. Do not use `+++` anywhere else in the reply. A third fence line
  (three plus signs) is refused.
- Every `[[table]]` goes above the closing fence. A `[[follow_up]]` or
  `[[check]]` table written below the closing fence is prose, not data, and the
  reply is refused.
- The front matter is TOML with typed fields. Unknown keys are refused, so a
  misspelled or invented key is an error, not a note. Use only the keys listed
  for your role.
- A list means a list of strings. A list field defaults to empty, so leave it
  out when it has nothing in it.
- Write free text as a TOML literal string, `'...'`. A literal string has no
  escapes, so quotes and backslashes need no care. For several lines use
  `'''...'''`. Do not use `"..."` for free text.
- A literal string cannot contain a single quote. Rephrase the text, or use the
  multi-line `'''...'''` form, which can.
- `answer` is always the first key and is always one of the words listed for your
  role. The words are lower-case except the researcher's, which are
  `READY`, `NOT READY` and `NEEDS FURTHER REVIEW`.
- The word `failed` belongs to the pump. Only a hand may write `answer = "failed"`
  itself. Every other role either gives one of its own words or ends without an
  envelope, and the pump records the failure.
- Do not repeat the front matter in the prose, and do not wrap the envelope in a
  code fence in your real reply. The examples below are fenced only so you can
  read them here.

## Fields by role

An envelope carries judgment and reports only. No field is a SHA, a published
ref, a merged flag, an exit code or a commit that the pump acts on. Those come
from the runner and from the repository history.

The `[[check]]` table is `{cmd, result, seconds}` with `result` one of `pass`,
`fail` or `skipped`, and `seconds` an integer.

| Role | `answer` | Other fields | Prose body |
| --- | --- | --- | --- |
| researcher | `READY`, `NOT READY`, `NEEDS FURTHER REVIEW` | `open_questions` (list) | the analysis |
| planner | `planned`, `delivered`, `operator`, `hold`, `design` | `reason` (string; required unless `planned`) | the plan document |
| admitter | `admitted`, `revise`, `escalate` | `required_changes` (list; non-empty for `revise`), `findings` (list), `reason` (string; required for `escalate`) | the review |
| slicer | `sliced`, `escalate` | `mode` = `slice`, `rework` or `split`; `[[slice]]` tables {`id`, `title`, `brief`, `target`, `write_scope` (list), `deps` (list)}, at least one for `sliced` and none for `escalate`; split ids are `<old id>.<n>`, two to four of them; `reason` (string; required for `escalate`) | notes |
| auditor (verify) | `clean_pass`, `residual`, `block_range`, `block_ship` | `raise_rung` (`need`, `main` or `release`; optional); `[[requirement]]` tables {`id`, `text`, `status` = `SATISFIED`, `PARTIAL`, `MISSING`, `UNVERIFIED` or `NOT_APPLICABLE`, `evidence` (list)}; `[[finding]]` tables {`id`, `content`, `paths` (list), `recheck_trigger`} | the report |
| judge (auditor, short mode) | `block`, `residual` | `reason` (string; required) | the reasons |
| head-cto | `research`, `plan`, `slice`, `rework`, `lock` | `reason` (string; required); `new_fact` (string; required for any answer but `lock` from the third run on) | the analysis |
| merge (a flow merge step: a slice onto the need branch, and the sync step) | `merged`, `conflict` (the worker's claim; the runner's publish receipt and git decide) | `conflicts` (list of paths); `design_change` (bool; the conflict needs a design change); `[[check]]` tables (reported only). No `tip`: the SHA comes from the publish receipt | the conflict resolutions |
| hand | `built`, `blocked-scope`, `failed` | `change` (list of paths; reported only: no-defect is decided by the runner from the push result `no_changes`, never by an empty list); `[[check]]` tables (reported only; the merge job's task rung exit code decides); `deviations` (list); `[[follow_up]]` tables {`paths` (list), `need` (string), `optional` (bool, default false)}; a required follow-up goes to the slicer, an optional one is only posted; `remaining` (list); `scope_paths` (list; required for `blocked-scope`); `scope_reason` (string; required for `blocked-scope`); `scope_design` (bool, default false) | the report |
| repairer (the repair flow) | `built`, `failed` | `change` (list of paths; reported only); `[[check]]` tables (reported only; the main rung decides); `deviations` (list). No diff cap; no sha, ref, commit or exit keys. `overtime` is the pump's word for a job past its time cap, never written by the repairer | notes |
| head-ceo (cadence prompt; the pump does not parse it) | `reviewed`, `no-op` | `promoted`, `research`, `dropped` (lists of handles) | notes |

## Examples, one per role

### Researcher

```
+++
answer = "NEEDS FURTHER REVIEW"
open_questions = [
  'Should the retry limit be per step or per run?',
  'Is the old queue table still read by any command?',
]
+++
The analysis follows: what the code does today, what the need changes, and
the two points above that the plan cannot settle from the code alone.
```

### Planner

```
+++
answer = "planned"
+++
# Plan: one title for the whole need

The plan document follows.
```

A planner that does not plan says why:

```
+++
answer = "delivered"
reason = 'Commit 3f9a2c1 already adds the command and its test; nothing is left.'
+++
Evidence for the claim above.
```

### Admitter

```
+++
answer = "revise"
required_changes = [
  'Name the file that owns the retry counter; the plan says "the store".',
  'State what the command prints when the run is closed.',
]
findings = ['The second phase repeats work the first phase already delivers.']
+++
The review follows.
```

### Slicer

```
+++
answer = "sliced"
mode = "slice"

[[slice]]
id = "need-1"
title = 'Add the retry counter column'
brief = '''
In src/store.rs, add the column and its migration. Add a test in
src/store_test.rs that asserts a new row starts at zero.
'''
target = "app"
write_scope = ["src/store.rs", "src/store_test.rs"]
deps = []

[[slice]]
id = "need-2"
title = 'Read the counter in the tick'
brief = 'In src/tick.rs, stop a step after two attempts. Test it in src/tick_test.rs.'
target = "app"
write_scope = ["src/tick.rs", "src/tick_test.rs"]
deps = ["need-1"]
+++
Left out as already delivered: the CLI flag. Assumed: one attempt means one job.
```

An escalation carries no slices:

```
+++
answer = "escalate"
mode = "slice"
reason = 'The plan needs a branch handed from one target to another, which the pump cannot do.'
+++
Notes.
```

### Auditor (verify)

```
+++
answer = "residual"
raise_rung = "main"

[[requirement]]
id = "R1"
text = 'A closed run stops accepting steps.'
status = "SATISFIED"
evidence = ["src/run.rs:210", "src/run_test.rs:44"]

[[requirement]]
id = "R2"
text = 'The board shows the run count.'
status = "PARTIAL"
evidence = ["src/board.rs:88"]

[[finding]]
id = "F1"
content = 'The board counts closed runs twice when a run is reopened.'
paths = ["src/board.rs"]
recheck_trigger = 'Any change to the run count query.'
+++
The report follows.
```

### Judge (auditor, short mode)

```
+++
answer = "block"
reason = 'The merge dropped the guard on line 120; a closed run now accepts steps.'
+++
The reasons follow.
```

### Head-cto

```
+++
answer = "rework"
reason = 'The build passes its rung but the audit found a missing guard.'
new_fact = 'The auditor traced the missing guard to the slice that merged last.'
+++
The analysis follows.
```

A `lock` answer needs no `new_fact`:

```
+++
answer = "lock"
reason = 'Three runs failed on the same missing decision; the operator must choose.'
+++
Notes.
```

### Merge

```
+++
answer = "merged"

[[check]]
cmd = "cargo check --quiet"
result = "pass"
seconds = 41
+++
No conflicts. The merge was clean.
```

A conflict names its files:

```
+++
answer = "conflict"
conflicts = ["src/tick.rs"]
design_change = true
+++
Both branches changed the same function; the two edits cannot be combined.
```

### Hand

```
+++
answer = "built"
change = ["src/store.rs", "src/store_test.rs"]
deviations = ['insert_row now takes the run id as its second argument.']
remaining = []

[[check]]
cmd = "cargo test --lib store --quiet"
result = "pass"
seconds = 38

[[follow_up]]
paths = ["src/other.rs"]
need = 'The old caller of insert_row still passes the old arguments.'
+++
Notes for the auditor.
```

A hand that needs a file outside its write scope changes nothing and answers:

```
+++
answer = "blocked-scope"
scope_paths = ["src/mailspace.rs"]
scope_reason = 'A one-line re-export is needed so the flow code can call parse_flowchart.'
scope_design = false
+++
Nothing was changed. The function is not reachable from my file.
```

A hand that finds no defect still answers `built` and lists the files it looked at in `change` only if it changed them. An empty `change` does not say "no defect": the runner decides that from the push result `no_changes`. An optional follow-up is only posted, not sliced:

```
+++
answer = "built"

[[follow_up]]
paths = ["src/other.rs"]
need = 'The old caller could use a clearer name.'
optional = true
+++
The reported defect does not reproduce: the guard is already in place.
```

### Repairer

```
+++
answer = "built"
change = ["src/store.rs", "src/store_test.rs"]
deviations = ['The failing test also covered the empty case; the fix handles it too.']

[[check]]
cmd = "cargo test --lib store --quiet"
result = "pass"
seconds = 38
+++
The row id was read before the insert; it is read after it now.
```

A repairer that cannot find a fix changes nothing and answers `failed`:

```
+++
answer = "failed"
deviations = ['The failing test passes on the branch; the red came from a stale build cache.']
+++
No change made.
```

### Head-ceo

```
+++
answer = "reviewed"
promoted = ["need-12"]
research = []
dropped = ["want-3"]
+++
Notes.
```

## When the envelope is refused

The pump checks the envelope before it acts on the reply. Refusals are:

- no opening fence, or text before it;
- a missing closing fence, or a third fence;
- a `[[follow_up]]` or `[[check]]` table written below the closing fence;
- front matter that is not valid TOML;
- an unknown key;
- an `answer` word that is not one of the role's words;
- a missing or mistyped field for that answer (a `slice` table with no `brief`,
  `blocked-scope` with no `scope_paths`, and so on).

The pump then sends one correction that quotes the problem and asks for the
same reply again, corrected. A correction does not use an attempt. If the next
reply is refused too, the pump sends a second correction under the same rule.
If the reply after that is still refused, the step answers `failed`, and the
pump handles it like any other failed step.

When you receive a correction, change only the envelope. Do not redo the work,
and do not change the prose unless the correction says to.
