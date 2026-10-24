# Letters and the reply envelope

One letter format serves every stored need, want, task, pump record, mail and
chat letter and every role reply: TOML front matter between two `+++` fence
lines, then Markdown prose. The same parser reads all of them
(`src/letter.rs`). In a stored letter the front matter carries the keys vivi
acts on (`done_when`, `depends_on`, and so on) and the prose is for the reader.
In a role reply the front matter carries the answer word and the role's typed
fields. The pump reads the front matter
and nothing else to decide what happens next; the prose is for the next
reader.

This file is the general rule and the full table. A role's own charter states
that role's fields and one example, so the charter is enough for a worker. Read
this file when you need the whole table, when you write a stored letter by
hand, or when a reply was refused.

## Stored letters

Need, want, task, pump record, mail and chat letters are front matter plus
prose, and the send path stamps the header `X-Vivi-Format: 1` on every one of
them. Memo and rule letters are not written in this format. The keys of the
pump letters (record, action, note, `[job]`, `[step]`) are in
[Pump letters](#pump-letters); a chat letter has no keys.

Keys per kind (unknown keys are refused, blank list entries are refused):

| Kind | Keys |
| --- | --- |
| need | `done_when` (list, **required**, non-empty), `target`, `depends_on` (list), `write_scope` (list), `priority` (integer), `serves` (a want), `explores` (a want). Never both `serves` and `explores` |
| want | `question` (**required**), `depends_on` (list) |
| task | `done_when` (list), `depends_on`, `parent`, `target`, `write_scope`, `from`, `branch`, `with` (list), `band`, and the pump's `[step]` and `[job]` tables ([Pump letters](#pump-letters)) |

A need letter:

```
+++
done_when = ["cargo test --test foo passes", "README names the flag"]
target = "vivi"
depends_on = ["ab12cd34"]
serves = "ef56ab78"
+++

Prose body in Markdown.
```

`vivi need send`, `vivi want send` and `vivi task send` build the front matter
from flags, so you rarely write it by hand.

A need, from flags (repeat `--done-when` for more clauses; optional
`--target`, `--write-scope`, `--depends-on`, `--serves` or `--explores`, and
`--mark`):

```sh
vivi need send --from mind --to hand-1 --subject "Add a greeting" \
  --body "Print hello." \
  --done-when "running hello.sh prints hello"
```

A task, from flags:

```sh
vivi task send --from mind --to hand-1 --subject "Write hello.sh" \
  --body "Print hello." \
  --done-when "running hello.sh prints hello" \
  --write-scope hello.sh
```

A want, from flags:

```sh
vivi want send --from head-cto --to mind --subject "want: extract a shared retry helper" \
  --body "Non-urgent cleanup after the auth fix lands." \
  --question "Should the three retry loops share one helper?"
```

`task send` has no flag for `from`, `branch`, `with` or `band`. Put those in a
whole-letter body.

### The whole-letter body

A body whose first non-blank line is `+++` is the whole letter and is taken
unchanged. Passing `--done-when`, `--target`, `--write-scope`, `--serves`,
`--explores`, `--question` or `--depends-on` beside it is refused. A body
`unit.md`:

```
+++
done_when = ["cargo test -p parser --test validate passes", "malformed input is rejected with the expected error"]
write_scope = ["crates/parser/src/validate.rs", "crates/parser/tests/validate.rs"]
band = "P1"
+++

goal: docs/goal.md
unit: U-17
edit: reject malformed input at validate.rs::parse_input
```

```sh
vivi task send --from mind --to hand --subject 'unit U-17: parser input validation' --body-file unit.md
```

Lines like `goal:` below the closing fence are free text: vivi never reads them.

### Refused at send

Nothing is stored when a send is refused:

- a need with no `done_when`: `need has no done_when: pass --done-when or add done_when = [...] to the front matter`
- a want with no `question`: `want has no question: pass --question or add question = "..." to the front matter`
- a whole-letter body plus a flag: `the body is a whole letter (it opens with +++): put done_when in its front matter instead of passing --done-when`
- an unknown front matter key, a blank list entry, or a need with both `serves` and `explores`;
- a mail whose front matter is not a pump action, a pump note or a role reply envelope, for example
  `unknown field `foo`, expected `pump` or `job` (and the front matter is not a role reply envelope)`;
- a pump record that fails the checks under [Pump letters](#pump-letters).

`--serves` and `--explores` check that the handle is a want and store its
canonical id.

### Letters that cannot run

Dependencies live in the front matter `depends_on` only; body lines such as
`done_when: ...` or `write_scope: ...` are prose and are not read. A letter
without the format header or without valid front matter is shown by `show` and
`list`, reported by `vivi step` as the exception `letter_invalid`, and refused
by the pump. Send a new letter of that kind instead.

## Pump letters

Three kinds of letter belong to the pump: the record that starts a run, the
action that steers a run, and the notes the pump writes while it works. A
plain-prose mail or chat message needs no front matter.

### Pump record

`vivi pump send --need <handle>` stores one letter of kind `pump`, addressed to
the pump. One open record per need starts one flow run. Keys (unknown keys are
refused):

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `need` | string | yes | handle of the need the run serves |
| `kind` | `build`, `research`, `release` or `repair` | no | the flow to run; absent means the need's mark (`research`, `release`, `repair`), else `build` |
| `target` | string | no | the target the run works in; a `repair` record must name a configured target |
| `tested` | string | no | the main tip a red main was seen at; only with `kind = "repair"` |

```
+++
need = "e4ef1712"
kind = "repair"
target = "default"
+++

Repair run.
```

A prose body gets `need = "<handle>"` as its front matter. A prose line that
starts with `kind:`, `target:` or `tested:` is refused: write those facts in
the front matter. A send is also refused for a need that already has an open
record, a need that is done, a handle that is not a need, front matter that
does not parse, and a `repair` record with no configured `target`.

### Pump action

`vivi pump requeue`, `withdraw`, `replan`, `answer` and `unlock` each post one
`mail` reply on the need's open record. The reply opens with a top-level
`action` key; the prose is the reason. The pump applies each letter once.
Keys (unknown keys are refused; a key that the chosen action does not read is
refused):

| Key | Type | Required | Read by | Meaning |
| --- | --- | --- | --- | --- |
| `action` | `requeue`, `withdraw`, `replan`, `answer` or `unlock` | yes | all | what to do |
| `need` | string | yes | all | handle of the need the record covers |
| `unit` | string | `requeue` | `requeue` | handle of the step task whose newest job runs again |
| `step` | string | `answer` | `answer` | handle of the step task being answered |
| `word` | string | `answer` | `answer`, `unlock` | `answer`: the answer word; `unlock`: the flow entry to restart at (absent means the flow's first entry) |

```
+++
action = "unlock"
need = "9e71360d"
word = "plan"
+++

released after review
```

`withdraw` closes the record `withdrawn`. `replan` restarts the live run at
`plan`. An action posted on a closed record is never
applied, and a letter that does not parse as an action is never applied (the
pump raises an alarm for it).

### Pump note

The pump records its own progress as `mail` replies whose front matter has a
`[pump]` table. Nothing else in the letter is read as data.

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `pump.note` | note word | yes | what the note says |
| `pump.t` | integer | yes | order stamp in unix microseconds, greater than zero; same-second notes sort by it |
| `pump.ref` | string | no | the thing the note is about, such as an attempt label or a job id |

The note words are `attempt` (one pool attempt of a job), `brief` (the job
brief, see below), `deviations` (a hand's deviations), `follow-up` (a hand's
follow-up items), `slice-env` (an environment failure of a slice, `ref` is the
hand job), `repair-attempt` and `repair-rung` (the repair flow), `main-red` (a
red main reported on its repair need), `abandoned` (job ids a restarted run
gave up) and `slice-merge` (why a slice merge failed, `ref` is the merge
job). The prose below the front matter is the note text.

```
+++
[pump]
note = "deviations"
t = 1761230000123456
+++

insert_row now takes the run id as its second argument.
```

### The `[job]` table

A pump job brief carries a `[job]` table. It is read in two places: on a task
(the brief is the task letter) and on a `brief` note (the brief is a reply on a
step task that already exists). A `[job]` table is refused on any other mail,
and a `brief` note without one is refused.

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | string | yes | the pool job id |
| `kind` | string | yes | the job kind |
| `builtin_role` | string | no | the built-in role the job runs |

```
+++
[pump]
note = "brief"
t = 1761230000123456

[job]
id = "j-4f2a91"
kind = "hand"
+++

The brief text.
```

### The `[step]` table

A task that belongs to a flow run carries a `[step]` table.

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `need` | string | yes | handle of the need the run serves |
| `run` | integer | yes | the run number |
| `node` | string | unless `sentinel` | the flow node the task is for |
| `sentinel` | bool | no | `true` for the run's sentinel task, which has no node; then `node` must be absent |

## Role replies

Every role reply in the pump is Markdown that opens with the same front matter.
The `answer` key carries the answer word. Seats send it with
`vivi mail reply`; the mail gate accepts it as a role reply envelope and
stores it unchanged.

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
- The first non-blank line must be the fence `+++`. The next `+++` line closes
  the front matter, and everything after it is prose. A later `+++` line is not
  refused, but do not write one.
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
- The word `failed` belongs to the pump. A hand and the repairer may write
  `answer = "failed"` themselves. Every other role either gives one of its own
  words or ends without an envelope, and the pump records the failure.
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
| head-ceo (cadence prompt; the pump does not parse it) | `reviewed`, `no-op` | `served`, `research`, `dropped` (lists of handles; `served` names the wants this run sent serving needs for) | notes |

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
served = ["want-12"]
research = []
dropped = ["want-3"]
+++
Notes.
```

## When the envelope is refused

The pump checks the envelope before it acts on the reply. Refusals are:

- no opening fence, or text before it;
- a missing closing fence;
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
