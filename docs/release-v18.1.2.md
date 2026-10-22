# Vivi 18.1.2

Patch release on 18.1.1. It fixes a pump crash on stores created before
18.1.1.

## Fix: existing pump stores gain `needs.withdrawn`

18.1.1 added a `withdrawn` column to the pump store's `needs` table but
left the store schema version at 6. A store already at version 6 was
therefore never migrated, and the pump exited on every tick with
`no such column: withdrawn`. (Seen on hub after the 18.1.1 upgrade.)

- The pump store schema moves to version 7. Opening a version 6 store adds
  the column with default 0 (not withdrawn).
- A store where the column was already added by hand opens cleanly; the
  migration ignores the duplicate column.
