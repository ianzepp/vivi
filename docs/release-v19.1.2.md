# Vivi 19.1.2

Patch release on 19.1.1: two proxy fixes.

## A dead Kimi credential no longer takes down the passthrough routes

`openai-passthrough` providers (zai, deepseek, opencode-go on hub) share the
Kimi adapter's code, and its credential lookup fell back to Kimi's own
credential files for every provider. One refused Kimi refresh (the
subscription is cancelled) marked that file dead, and every passthrough
request then returned 502 `credential_error` until the proxy restarted.
Dead-marking, the Kimi credential fallbacks and OAuth refresh now apply only to
the `kimi-coding` adapter. Passthrough `/health` names `openai-passthrough`.
A passthrough provider with a `credentials` file reads it as a bearer only; a
401 goes back to the client unchanged.

## OpenCode Go: session header

OpenCode Go refuses chat requests without `x-opencode-session`
(`MissingSessionID`). A passthrough provider may now set

```toml
session_header = "x-opencode-session"
```

The proxy forwards a client's own header unchanged, or adds one: `ses_` plus a
hash of the model and the first non-system message (stable for a
conversation, never derived from a key), or a random value when the body has
neither. Older binaries reject the new field, so upgrade before adding it.
