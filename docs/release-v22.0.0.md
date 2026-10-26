# Vivi 22.0.0

Major release on 21.4.1. **vivi-server is now part of vivi**: one release, one
binary, both sides.

## Breaking: the server is `vivi server run`

The vivi-server repository (its full history) moved into vivi. The server's
code is `src/server/`, its tests `tests/server_*`, its smoke scripts
`scripts/server/`, its docs and changelog `docs/server/`. There is no separate
vivi-server release from here on.

- `vivi server run` starts the server; `vivi server run --help` lists the
  environment it reads and `--version` prints the version. It is configured
  exactly as before: `BIND`, `PORT`, `DATA_DIR`, `STORE_ENCRYPTION_KEY`,
  `MODERATION_TOKEN`, `HOST_OPERATOR_TOKEN`, the `HOST_*` settings, and the
  three secrets from systemd credentials (`LoadCredential=`) first.
- A link named `vivi-server` to the `vivi` binary runs the server too, so a
  unit or script that started the old binary keeps working once the link is
  in place: `ln -s vivi /usr/local/bin/vivi-server`.
- The server still starts single-threaded: vivi's `main` dispatches it before
  vivi's own runtime exists, so it makes itself non-dumpable and takes its
  secrets out of the environment before any other thread runs.
- **The log target is `vivi::server`.** A unit that sets
  `RUST_LOG=vivi_server=info,...` must change it to
  `RUST_LOG=vivi::server=info,tower_http=info` (the new default), or the
  server's info lines disappear.
- The server builds against the vivi it ships with, not a pinned tag, so the
  two can no longer drift apart.

## Breaking: project retirement replaces `delete_project`

From the server's unreleased work (vivi-server after 2.4.0):

- `archive_project`, `restore_project` and `purge_project` replace the
  `delete_project` host tool, which is gone with no alias. Archive is the
  default: it unregisters the project and keeps every file. Restore undoes it.
  Purge destroys data, and only 24 hours or more after the archive. Older
  binaries ignore archived metadata and must not be used to downgrade across
  this change.
- Purge revokes a principal's keys before removing its last grant on the
  project, so an interrupted purge can no longer leave live keys behind.
- `create_project` and `lane_create` accept only a GitHub `source`
  (`https://github.com/<owner>/<repo>[.git]`); other addresses are refused
  before anything is written. The local `vivi project create --source` still
  accepts any Git address.
- A project made by `create_project` comes up with its pump running; the
  answer carries `pump_mode`.

## Fixes

- Pump replies: a role reply that opens with a sentence or two of prose and then
  its `+++` envelope is read from the first fence line, the prose kept at the
  head of the body. Models often introduce their receipt this way, and every
  such reply used to cost two correction jobs. A reply with no fence line is
  still refused, and letters elsewhere keep the strict rule.
- Pool jobs: a commit made by vivi's own local-pool code under a pool job now
  keeps its configured identity even when the job inherits `GIT_AUTHOR_*` and
  `GIT_COMMITTER_*` from the runner (the main rung was red on every pool run).
- `skills/vivi`: `vivi pump withdraw` on a need with no open record, and both
  remedies (`vivi need done` or `vivi pump send --need` then withdraw).
- The server's archive tests hold a process-wide lock for their whole life,
  so a sibling test can no longer import its project mid-test.

## Upgrading a host

1. Install vivi 22.0.0.
2. Point the server's unit at `vivi server run` (or add the `vivi-server`
   link), and update `RUST_LOG` as above.
3. Restart the server. Pump, proxy and mind restart with the vivi install as
   before.
