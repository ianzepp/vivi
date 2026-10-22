# Vivi 19.0.1

Patch release on 19.0.0.

## Pool jobs keep the SSH session's PATH order

The `worker` (macOS) and `ssh` pool drivers start a slot's boot under a login
shell (`bash -l`) for the variables its profile exports. On macOS that runs
`/etc/profile` and `path_helper`, which moves the system directories
(`/usr/bin` and the rest of `/etc/paths`) ahead of the session's own entries,
such as Homebrew and `~/.local/bin`. Jobs on a Mac worker therefore found
`/usr/bin/python3` (3.9) before a newer Python, and scripts that need Python
3.11 (`import tomllib`) failed in hand, merge and gate jobs.

The drivers now pass the session `PATH` in as `VIVI_SESSION_PATH` and put it
back in front inside the login shell, keeping the first occurrence of each
entry and dropping empty ones. The login profile's exported variables still
apply, the slot's stand-in tools stay last on `PATH`, and the publisher's
deliberately minimal `PATH` is unchanged. The snippet is bash 3.2 compatible.
Nothing changes on the worker itself: the driver script ships inside `vivi`.

## vivi skill

The embedded vivi skill gains an "Inherited config (base mailspaces)" section
and documents `vivi pump gate show|set`, both shipped in 18.3.0.
