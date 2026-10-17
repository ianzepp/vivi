# Migrating the pump from state.json to pump.db

Vivi 13.0.0 moves the pump's live state into `<pump home>/pump.db`. The
import is one-way; the old file is kept for rollback.

## Before upgrading

1. Stop the pump with `STOP` and wait for the process to exit. A tick may take
   a few minutes.
2. Back up the pump home. At minimum keep `state.json`, `plans/`,
   `planner-charter.md`, `priority.txt`, `skip.txt`, `reserved.txt` and
   `known-red.txt`.
3. Note the current `vivi pump status` output.

## First start

Start the 13.x pump. On the first tick it:

1. creates `pump.db` with schema version 2;
2. imports `state.json` in one transaction;
3. sets every imported need and unit without a revision to revision 1;
4. renames `state.json` to `state.json.imported`;
5. continues from the imported units and needs.

Check `vivi pump status` and `<home>/log.txt`. The import logs one line naming
the archived file.

## Rolling back to 12.x

The export is the rollback file. It is a snapshot, not a second live store.

1. Stop the 13.x pump with `STOP`.
2. Run `vivi pump export --json <path>`.
3. Start the 12.x pump on that file, or restore `state.json.imported` to
   `state.json` if the export cannot run.

The 12.x pump writes `state.json` again. Do not run both versions on the same
pump home.

## Schema and readers

`pump.db` carries `PRAGMA user_version = 2`. Readers (`need show`, `board
--pump`, `boot`, `need list --pump`) open it read-only and refuse an unknown
version with `pump state unreadable (schema N, expected 2)`. The pump is the
only writer, and it refuses a database whose version is newer than the binary
rather than rewriting the marker.
