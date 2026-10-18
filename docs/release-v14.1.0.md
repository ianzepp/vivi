# Vivi 14.1.0

A minor release: leftover `vivi proxy` live-test fixes, plus self-update.

Do not tag or publish until Ian has walked `install.sh` and `vivi update`
on a Linux desktop.

## Proxy leftovers

- Default Codex `/v1/models` list is `gpt-6-luna` and `gpt-6.1-sol`.
- Access-log usage harvest now keeps the full response body, so non-streaming
  Kimi/llama JSON and llama streaming usage chunks are recorded when the
  upstream reports them (including float/string token counts and llama
  `timings`).
- The Codex chat-completions shim sets `x-vivi-request-id` to the same id
  as the access log.

## Self-update

`vivi update` resolves the newest public `v*` GitHub release, downloads
`vivi-<triple>.tar.gz`, verifies `<archive>.sha256`, installs
`<prefix>/bin/vivi-<version>`, and atomically repoints a relative `vivi`
symlink. `--check` compares `CARGO_PKG_VERSION` to that release (exit 1
when an update exists, exit 2 on fetch failure). `--version` pins or rolls
back. `vivi versions` lists installs; `vivi uninstall` removes them.

`install.sh` uses the same layout under `~/.local` so binaries live in
`~/.local/bin` (usually on `PATH`; never `~/.vivi`, which is a
mailspace). The release workflow publishes a `.sha256` next to each
tarball, attaches `install.sh`, and asserts that the installer's default
version matches the tag.

A cargo-managed `~/.cargo/bin/vivi` or any other plain-file `vivi` is
refused unless `--prefix` or `--adopt` / `--force` is given. `--adopt`
installs under `~/.local/bin` and moves a plain-file `vivi` aside to
`vivi.unmanaged` (never deletes it). `vivi uninstall` only removes
`vivi-<semver>` files and the `vivi` symlink in that shared bin
directory, and refuses a bin dir that is or sits inside a mailspace.

## Compatibility

- Pump database schema: unchanged (6).
- Mailspace schema: unchanged (8).
