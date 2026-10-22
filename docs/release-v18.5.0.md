# Vivi 18.5.0

Minor release on 18.4.0. The model proxy gains a Grok route, documents an
OpenCode Go route, and stops sending Kimi's client identity to other
providers.

## `xai-grok` proxy adapter

A native adapter for xAI's Grok using the Grok CLI's login:

```toml
[providers.grok]
kind = "native"
adapter = "xai-grok"
credentials = "~/.grok/auth.json"
```

- It reads the login entry keyed `<issuer>::<client_id>` (the one matching
  `https://auth.x.ai`; `client_id` picks among several).
- It refreshes 30 s before expiry, or once after a 401, at
  `https://auth.x.ai/oauth2/token`, and writes the new key, any rotated
  refresh token and the new expiry back atomically at mode 0600 under the
  shared `auth.json.lock`. It re-reads the file first, so a refresh by the
  Grok CLI sharing the file wins.
- Requests pass through to `https://api.x.ai/v1` with only the bearer
  injected; streaming is preserved. Token text and token-endpoint bodies are
  never logged or returned.
- `vivi proxy providers` and `vivi proxy check grok` report its expiry and
  health.

## OpenCode Go on `openai-passthrough`

No code was needed; the README and the vivi skill document the block:

```toml
[providers.opencode-go]
kind = "native"
adapter = "openai-passthrough"
upstream = "https://opencode.ai/zen/go/v1"
key_cmd = "python3 -c '...read [\"opencode-go\"][\"key\"] from ~/.local/share/opencode/auth.json...'"
```

## Kimi identity headers go to Kimi only

`openai-passthrough` shares Kimi's handler, and until now every passthrough
upstream (DeepSeek and ZAI included) received the `KimiCLI/<version>`
user agent and the `x-msh-*` device headers, and the proxy wrote a
`device_id` file beside the credential. Only `kimi-coding` sends them now.

## Registry lock

A host-registry update (`vivi pump service add`, `vivi project create`) now
waits up to 2 s for the registry lock instead of failing at once. A child
process forked by another thread can hold a just-released lock for a
moment; a real concurrent writer still gets "another Vivi registry update is
running" after the wait.
