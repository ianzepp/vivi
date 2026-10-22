# Vivi 19.1.0

Minor release on 19.0.3: two new commands.

## vivi pump recover

A need branch held `unverified` (a runner published a tip the controller did
not accept) could never resume: the pump logged "need branch held for explicit
recovery" every tick and no command performed the recovery.

`vivi pump recover <need>... [--target <id>] [--reason TEXT] [--dry-run]`
posts an action on the need's open pump record, applied at the pump's next
tick. The pump re-reads the remote need ref; if it still equals the observed
tip, it adopts that tip as the branch's trusted tip, sets the branch active
and returns merging units to delivered, so the next need merge re-merges them
(a re-merge of work already in the tip is harmless). A ref that moved or
cannot be read is refused with a `need-recover-refused` exception and the hold
stays. The pump does not re-prove the adopted tip's ancestry: running the
command is the operator's statement that the tip is sound. `--dry-run` shows
what would happen and reads the remote ref live. The hold message now names the
command and is logged once per hold.

## vivi mcp tools and vivi mcp call

MCP clients cache a server's tool list when a session starts, so tools a server
upgrade adds stay invisible until the client restarts. Two one-shot commands
reach the project server directly, with the bridge's credential resolution:

- `vivi mcp tools [--json]` lists the tools the server offers now.
- `vivi mcp call <tool> [ARGS_JSON] [--args-file PATH|-] [--json]` calls one
  tool and prints its text, or the full result with `--json`. Exit 1 when the
  tool reports an error, 2 on transport, auth or JSON-RPC errors.

Bare `vivi mcp` is still the stdio bridge.
