# `vivi mind` quickstart (spike)

How to stand up the `spike/vivi-mind` mind on one Linux box from a fresh
clone: a proxy, pi, at least one pumped project, and the mind in the
background. This was written while rebuilding the e2e sandbox from scratch
on 2026-10-25; every step below was run there. The design and the open
questions are in [`design-vivi-mind.md`](design-vivi-mind.md).

Nothing here installs a service. Everything stays on loopback.

## 0. An isolated environment (optional, recommended for a test hub)

The mind and the pump find everything through the config and state
directories, so a throwaway hub needs only environment overrides:

```sh
export SANDBOX=/path/to/sandbox
export HOME=$SANDBOX/home                       # git identity, pi config for pump jobs
export XDG_CONFIG_HOME=$SANDBOX/xdg/config XDG_DATA_HOME=$SANDBOX/xdg/data
export XDG_STATE_HOME=$SANDBOX/xdg/state XDG_CACHE_HOME=$SANDBOX/xdg/cache
export VIVI_CONFIG_DIR=$SANDBOX/xdg/config      # pump-service.toml and mind.toml under vivi/
export VIVI_PROXY_CONFIG=$SANDBOX/xdg/config/vivi/proxy.toml
export VIVI_MIND_HOME=$SANDBOX/mind             # state.json, mind.sock, sessions/, mind.log
```

`VIVI_MIND_HOME` is also pi's `HOME` when the mind starts it (see step 3).

## 1. Build vivi and install pi

```sh
git clone https://github.com/ianzepp/vivi.git && cd vivi
git checkout spike/vivi-mind
cargo build                                     # target/debug/vivi has `vivi mind`
npm i -g --prefix "$SANDBOX/npm" @mariozechner/pi-coding-agent@0.73.1
export PATH="$SANDBOX/npm/bin:$PATH"            # pi; the mind passes PATH through to pi
```

pi 0.73.1 is the version whose RPC the mind speaks (`src/mind/pi.rs`).

## 2. The proxy

The mind and the pump's pi jobs reach models only through `vivi proxy` on
`127.0.0.1:18180`. For OpenRouter, the bearer passthrough with the key read
from the proxy's own environment (no secret in the file):

```toml
# $VIVI_PROXY_CONFIG
listen = "127.0.0.1:18180"
log_dir = "/path/to/sandbox/logs/proxy"

[providers.openrouter]
kind = "native"
adapter = "openai-passthrough"
upstream = "https://openrouter.ai/api/v1"
key_cmd = "printenv OPENROUTER_API_KEY"
```

Start it in the background with `OPENROUTER_API_KEY` in that shell's
environment (`vivi proxy run`; `vivi proxy install` writes a user service
instead), then `vivi proxy check openrouter`.

## 3. Point pi at the proxy

pi reads custom providers from `~/.pi/agent/models.json`. The mind starts pi
with a cleared environment and `HOME` set to the **mind home**, so the mind's
pi reads `$VIVI_MIND_HOME/.pi/agent/models.json`, not your own. Pump jobs run
pi as you, so they read `$HOME/.pi/agent/models.json`. Write the same file in
both places:

```json
{
  "providers": {
    "proxy": {
      "baseUrl": "http://127.0.0.1:18180/openrouter/v1",
      "api": "openai-completions",
      "apiKey": "vivi-proxy-holds-the-key",
      "models": [
        { "id": "~deepseek/deepseek-flash-latest", "contextWindow": 128000, "maxTokens": 16000 }
      ]
    }
  }
}
```

The `apiKey` is a placeholder pi requires; the proxy replaces it. Check it:
`pi -p --no-tools --model 'proxy/~deepseek/deepseek-flash-latest' "Reply PONG"`.

## 4. `mind.toml`

Copy [`src/mind/mind.toml.example`](../src/mind/mind.toml.example) to
`$VIVI_CONFIG_DIR/vivi/mind.toml` (beside `pump-service.toml`) and set at
least `model`, as pi's `<provider>/<id>`:

```toml
authority = "observe"
heartbeat = "10m"                 # short for a test; the default is 30m
pi_command = "/path/to/sandbox/npm/bin/pi"
model = "proxy/~deepseek/deepseek-flash-latest"
```

The mind adds `--mode rpc --no-tools --session-dir ...` itself, so
`pi_command` is the bare executable, with no wrapper.

## 5. Projects for it to watch

The mind watches every project in the host registry
(`$VIVI_CONFIG_DIR/vivi/pump-service.toml`) with `pump = true`.
`vivi project create` registers one:

```sh
vivi project create demo --projects-root $SANDBOX/projects --lanes-root $SANDBOX/lanes \
  --git-url-prefix file://$SANDBOX/lanes/ --initial-commit --no-serve
```

It prints what the pump still needs. For a local pool on pi, in this order:

1. Append the pool to `.vivi/mailspace.toml` by hand (`vivi pool set` cannot
   add a `[harness]` one key at a time: every write is validated and a
   harness without `run` is invalid):

   ```toml
   [pools.pi]
   provider = "local"
   slots = 2

   [pools.pi.local]
   root = "/path/to/sandbox/slots"
   worktree_of = "/path/to/sandbox/projects/demo/.vivi/pump/targets/demo/repo"

   [pools.pi.harness]
   name = "pi"
   run = "pi -p --model 'proxy/~deepseek/deepseek-flash-latest' --append-system-prompt {rules} @{brief}"
   models = ["deepseek-flash"]

   [pools.pi.committer]
   name = "Your Name"
   email = "you@example.com"
   ```

   `worktree_of` is the pump's integration clone. vivi 21.3.2 refuses a `~`
   in `harness.models` at dispatch, so the pool lists a plain label and the
   `run` line names the real slug.
2. Fill `[pump] committer_name` and `committer_email`, then add the roles:
   `vivi role add hand --kind hand`, `vivi role add auditor --kind auditor`,
   `vivi role add mind --kind mind`.
3. For every pump role (`merge pump-planner researcher admitter slicer
   head-cto repairer hand auditor`): `vivi role set <r> --model deepseek-flash`
   **before** `vivi role pool <r> pi`. The other order is rolled back ("has no
   model on pool pi").
4. Rungs: add `[pump.rungs]` with `pools = ["pi"]`, then
   `vivi pump rungs set task|need|main <command>`. The command runs in the
   target repo, so commit it to the lane's main first.
5. `vivi config validate`, then `vivi pump start` and `vivi pump run` in the
   background. The first tick clones the integration repo.
6. Then `vivi pool create --pool pi --porcelain`. A local pool has no slots
   until this runs, and it needs the clone from step 5. Without it the pump
   sits idle and health still says `ok`.

A deliberately stuck project for testing: the same setup with
`run = "echo this harness never writes an envelope"`. Its first need fails
its envelopes, goes to head-cto, and ends locked, with no model call.

## 6. Run the mind

```sh
vivi mind service list            # every registered project, watched=true
vivi mind probe                   # read-only: per-project signals and the wake decision
nohup vivi mind service run >> mind-run.log 2>&1 &
vivi mind status                  # running, authority, session id, last wake
vivi mind log -n 40               # wake turns and the gate's verdicts (dry-run actions)
```

The first tick wakes at once (heartbeat `never`). Later wakes come on a new
stuck signal, or when the heartbeat comes due: `role_schedule` calls a seat
due at 1.1 times its cadence, so a `10m` heartbeat wakes about 11 minutes
after the last wake. `vivi mind probe` shows `nothing due` until then.

## 7. Talk to it

```sh
vivi mind                         # attach; lines are prompts
# /status  /history  /pause  /resume  /compact  /quit
```

Attach is a new socket connection to the one pi session each time, so
`/quit` and attaching again (or restarting the mind) keeps the conversation:
`/history` shows the earlier wakes and chats. The session id is in
`state.json`; the session file is under `$VIVI_MIND_HOME/sessions/`.

`vivi mind stop` asks the mind to exit. Nothing restarts it unless a service
manager does (`vivi mind service unit` prints an example systemd unit).
