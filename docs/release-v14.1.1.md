# Vivi 14.1.1

Patch on 14.1.0. Ian tags and releases; this note is the change list.

## Sticky dead credentials (`vivi proxy`)

A refused token refresh (HTTP 400/401, especially `invalid_grant`, or a
missing refresh token when a JWT must be refreshed) marks that provider's
credential file dead in memory, keyed on path + mtime + size. Later
requests return 502 `credential_error` immediately with no upstream or
token-endpoint calls. One WARN when it goes dead. Recovers when the file
changes or the process restarts. Transient token-endpoint failures
(network, 429, 5xx) do not mark dead. There is still no background
refresh.

`/health` and `/<provider>/health` report `status: credential_dead` with
HTTP 503. `vivi proxy check` now GETs `/<provider>/health` and prints
that label. `vivi proxy providers` does too.

## Service units

systemd `--user` `vivi-proxy.service` uses `Restart=on-failure` and
`StartLimitIntervalSec=60` / `StartLimitBurst=5`. launchd keeps
`KeepAlive` and sets `ThrottleInterval` 10.

`vivi proxy install` writes `ExecStart` to the stable `vivi` symlink when
the running binary is a versioned `vivi-<semver>` next to that link, so
`vivi update` is picked up on the next process start. `vivi update`
prints a reminder to restart an already-installed unit (`vivi proxy
install`); it does not restart the unit itself.

## Update / install.sh

- Warn when a different `vivi` is earlier on PATH than the managed
  symlink.
- The update header `from` version is the active symlink target.
- `vivi update` prints `checksum: OK` after a verified download.
