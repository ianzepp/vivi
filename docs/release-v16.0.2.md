# Vivi 16.0.2

Patch on 16.0.1. Found while running a real pump that was upgraded from 15.

## Finished pool jobs were collected and then dropped

The pump collects pool jobs from `vivi pool reconcile --porcelain`. When any
dispatch in the round failed, the command exited non-zero and the pump
ignored all of its output, including the `collected` lines for jobs that had
finished. A collected result is reported only once, so those jobs stayed
`dispatched` for ever: need-branch init, merges and audits never applied, and
units waiting on their need branch never dispatched. The pump now applies
every result a round reports and logs the failure.

## Dispatches left by 15 no longer fail every round

15 keyed pool slots on the mailspace name, 16 on its id. A dispatch that 15
left open on an old slot could never be settled: every reconcile reported
`not a slot of this pool` and exited non-zero. Such a dispatch is now
recorded as lost (`lost ... slot not in this pool`), so its unit is retried
normally and the round succeeds.

## Upgrading from 16.0.1

Restart the pump service after `vivi update`. On start the pump recovers
jobs whose results were dropped by reading their task receipts.

Also note for an upgrade from 15: an upgraded pool starts with no slots,
because 16 does not see 15's. Run `vivi pool create --pool <name>` for each
local pool the pump uses before `vivi pump start`; dispatch does not create
slots.
