# Vivi 21.0.0

Major release on 20.0.0. Every letter is a **v1 letter**: a need, want, task,
mail, chat or pump record opens with a TOML front-matter block between `+++`
fences, carries its facts there, and ships with the header
`X-Vivi-Format: 1`. The work-letter and pump content headers are gone from
every writer; headers carry transport and letters carry facts. Events, memos
and rules keep their headers for now (verdict, tag, rule scope, supersedes)
— they move into front matter in the next step of the format work. Old
letters stay readable as history and are refused for execution. This is a
clean break.

## Upgrade note

- **Drain or reset a pump host before upgrading.** A need with a live run
  keeps it: in-flight runs on legacy needs are not re-checked against the new
  format. `vivi pump reset` archives the old pump home beside itself when the
  run history is not worth migrating.
- **vivi-server must move to 2.0.0 with this release.** `vivi mcp` and the
  pool MCP bridge post to `<server>/v1/mcp`, which older servers do not serve,
  and vivi-server 2.0.0 keeps every letter it composes on this format. With
  2.0.0 the old per-project MCP routes are gone: mint new server keys and
  re-point connectors (see the vivi-server 2.0.0 changelog).
- `vivi config validate` refuses every key vivi 21 never reads — the v19
  pump keys: `[pump] planner, plan, reserved_file, fix_prefix`;
  `[pump.board] planners_max, plan_timeout`; `[pump.dispatch] kinds,
  default_kind, max_attempts, per_tick, overdue_grace, last_resort`.
  `vivi config migrate --write` removes them; the pump never stops on them.
- A hand role needs a pool.

## The v1 letter format

- A need requires a non-empty `done_when` list in its front matter and is
  refused at send otherwise; nothing is stored. A want requires a `question`.
  A task requires neither; `vivi step` reports `no_done_when` for one.
- Send flags render into the front matter: `--done-when`, `--target`,
  `--write-scope`, `--serves`, `--explores`, `--question`, `--depends-on`.
  Dependencies ride `depends_on = [...]` in the front matter;
  `X-Vivi-Depends-On` is gone.
- Pump records, actions and notes are letters too. A legacy letter raises
  `flow-letter-invalid` once and is never acted on. `vivi step`, verify
  inputs, the planner brief, slice targets and pool refs all read front
  matter.
- `vivi::letter::gate` is public: library consumers compose through the same
  gate, and every letter vivi-server 2.0.0 composes is a v1 letter.

## Wants follow their serving needs

- `vivi want promote` is removed. A want's state is computed from its serving
  needs: open while any is open, done once all are closed (a withdrawn need
  counts as closed, an explores need does not), and a new serving need
  reopens the want. `need send --serves` / `--explores` binds a need to the
  want; want show/list, the board, boot, cadence and the work-graph node all
  follow the computed state.

## The pump

- `vivi pump reset` archives the pump home beside itself
  (`pump-archive-<stamp>`, never overwriting an earlier archive) and starts
  fresh; it is refused while the pump runs, and a paused pump still holds the
  lock and is refused too. The next tick re-ingests every open pump record.
- Config writes go through the guard: backup first, validate after, rollback
  on a new error, refused while the pump runs. A paused pump is writable.
- The v19 surfaces are gone: `vivi pump status`, `closeout`, `adopt`,
  `export` and `log --lane`, the boot pump view, board and need pump blocks,
  gate routing, mind oversight, the v19 doctor and health signals, and the
  pool clean recycle.

## vivi mcp

- `vivi mcp tools` and `vivi mcp call` drive a vivi-server over Streamable
  HTTP at `<server>/v1/mcp` with an API key (`VIVI_MCP_API_KEY`;
  `--server` or `VIVI_MCP_SERVER` or the `[sync]` table names the server).
  The pool stdio bridge uses the same endpoint. Member credential mint and
  renewal are gone: one server key is the whole credential.

## Removed

- Commands: `vivi pump status`, `vivi pump closeout`, `vivi pump adopt`,
  `vivi pump export`, `vivi pump log --lane`, `vivi want promote`, and the
  member credential verbs of `vivi mcp`.
- Headers: the work-letter and pump content-header writers
  (`X-Vivi-Depends-On`, `X-Vivi-Parent`, `X-Vivi-Need`, `X-Vivi-Pump-*`).
  Event, memo and rule headers (`X-Vivi-Verdict` and kin, `X-Vivi-Tag`,
  `X-Vivi-Rule-Scope`, `X-Vivi-Supersedes`) remain in this release.
