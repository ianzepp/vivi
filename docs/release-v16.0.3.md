# Vivi 16.0.3

Patch on 16.0.2.

## Needs without `done_when:` lines can land

A need lands on the integration branch only after an Auditor accepts it, and
16.0.0 built the audit's requirement list only from `done_when:` lines in the
need's own messages. A need with none was refused before any audit ran
("need audit has no original requirements"), so it could never land; most
needs written before 16 have none. Such a need is now audited against its
own messages: each source message's text (up to 4000 characters) is one
requirement. `done_when:` lines, when present, are used as before.

## Known issue

Pump-internal repair needs (`pumpfix-*`: gate fixes and quarantine repairs)
have no mail thread, so their audit cannot be built ("message not found").
Their units merge into their need branch but the need does not land yet, and
the `need-audit-dispatch` exception repeats each tick.
