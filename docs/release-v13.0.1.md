# Vivi 13.0.1

A patch release for the mailspace CLI. The mailspace schema stays at 8;
nothing to migrate.

## Large dumps no longer exceed SQLite's parameter limit

`want dump --status all` loads events and reply links for every listed
message with one `IN (?)` statement per helper. A large mailspace — about
67,000 ids in the worst case seen — put more bound parameters in one
statement than SQLite allows (32,766), and the dump failed at prepare with
`variable number must be between ?1 and ?32766`.

Both batch loaders now query the ids in chunks of 10,000 and merge the
results. Want, need, task, and mail dump, the board, work lists, lifecycle
folds, and rules all share those loaders, so none of them fail on a large
mailspace anymore. Chunks keep the one-query-for-many-ids shape, so small
dumps are as fast as before.

## Compatibility

- Mailspace schema: unchanged (8).
- No new configuration and no behavior change beyond the fix.
