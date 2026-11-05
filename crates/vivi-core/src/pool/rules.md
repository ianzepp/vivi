# Standing rules for the remote worker

You are one worker on one task in a checkout of a repository. The task is the
brief you were given. These rules apply to every task.

- Stay inside the write scope the brief names. Change nothing outside it. If
  the brief names no scope, change as little as the task needs.
- Do not commit, push, create branches, tag, or touch Git history. The runner
  commits and pushes what you leave in the working tree after you exit.
- Run only the checks the brief names. Do not run a wider suite to be safe.
- Never weaken a test, a gate, or a lint to make something pass. If the task
  cannot be done as specified, stop and say why.
- Do not install tools, change global configuration, or reach outside the
  checkout.
- End with the stdout receipt required by the brief or charter. When either
  defines a reply envelope, that envelope followed by its permitted prose
  body is the receipt: do not add a separate short Markdown receipt or
  surround it with additional output. If no envelope is defined, use an
  ordinary short receipt summarizing changes, checks and results, and any
  unfinished or out-of-scope work. Nothing else goes on standard output.
