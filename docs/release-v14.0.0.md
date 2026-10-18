# Vivi 14.0.0

A major release for `vivi proxy`: one loopback front door that turns local
subscription logins into a normal provider API.

The mailspace and pump schemas are unchanged. The new surface is a
user-level daemon next to the pump service, configured at the platform
config directory under `vivi/proxy.toml`.

## `vivi proxy`

One listener (default `127.0.0.1:18180`) path-routes
`/<provider>/v1/{responses,chat/completions,messages,models}` and the same
paths without `/v1`. Clients such as Pi keep one `baseUrl` per provider.

```sh
vivi proxy run                 # foreground listener + child supervisor
vivi proxy install             # macOS LaunchAgent, or Linux systemd --user
vivi proxy uninstall
vivi proxy status
vivi proxy log [-f] [--provider <name>]
vivi proxy providers           # Pi base URLs, health, credential expiry
vivi proxy check [provider]    # tiny live GET to /<p>/v1/models
```

`install` writes `com.vivi.proxy` (launchd) or `vivi-proxy.service` (systemd
`--user`) so restarting the pump never cuts model access. A second
`vivi proxy run` refuses the instance lock. `--config` and
`VIVI_PROXY_CONFIG` override the default config path.

## Backends

| `kind` | Meaning |
| --- | --- |
| `native` | In-process adapter (`chatgpt-codex` or `kimi-coding` / `openai-passthrough`) |
| `child` | Vivi spawns the command, health-checks it, restarts with backoff |
| `upstream` | Reverse-proxy to a server that is already listening |

Native Codex reads the Codex CLI `auth.json` (then the Pi agent fallback) in
place, refreshes 30s before JWT expiry under an advisory lock, writes back
atomically at mode `0600`, filters SSE to an allowlist, rebuilds an empty
`response.completed.output`, and keeps the existing non-streaming, tool-less,
usage-0 chat shim. Native Kimi re-reads its credential file on every request,
injects `X-Msh-*` identity headers, passes bytes through, and retries once
after a 401 refresh. Cursor stays a `child` (Bun/protobuf engine). Local
llama-server is an `upstream`. Codex and Kimi can still be `child` processes
if the Python scripts are kept as a fallback.

## Security and logging

`proxy.toml` holds paths and `key_cmd` / `auth_token_cmd` only, never inline
secrets. A non-loopback bind requires `allow_remote = true` and
`auth_token_cmd`. Inbound POST needs `Content-Type: application/json`.
Requests with an `Origin` header, or a `Host` that is not loopback / the bind
address, are refused. Credential and token-endpoint failures become a fixed
message plus a request id; token-endpoint bodies are never echoed. The
rotated JSONL access log records provider, model, status, timings, and token
usage when the stream exposes them. Bodies are never logged. `GET /health`
aggregates each backend.

## Migration from `com.ianzepp.proxy.*`

Point each Pi provider `baseUrl` at the one listener and retire the old
per-port jobs:

| Provider | Old base | New base |
| --- | --- | --- |
| Codex | `http://127.0.0.1:18181` | `http://127.0.0.1:18180/codex/v1` |
| Kimi | `http://127.0.0.1:18182/v1` | `http://127.0.0.1:18180/kimi/v1` |
| llama | `http://127.0.0.1:18173/v1` | `http://127.0.0.1:18180/llama/v1` |
| Cursor | `http://127.0.0.1:18183/v1` | `http://127.0.0.1:18180/cursor/v1` |

A four-provider Mac example lives in the README under “The proxy”.

Do not tag or publish this release until the native adapters have been
checked against live providers.

## Compatibility

- Pump database schema: unchanged (5).
- Mailspace schema: unchanged (8).
- New user-level files: `vivi/proxy.toml`, `vivi/proxy.lock`, and the
  rotated access log under the configured `log_dir`.
