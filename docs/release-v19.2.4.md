# Vivi 19.2.4

Patch release on 19.2.3: a red gate on a non-default target can go green.

## Gate audits on every target

When a gate build failed on a non-default target (gradus, inferentia), the
auditor ran and returned its fix units, but the pump never applied the
result: the audit collector read only the workspace target's audit run. The
gate-fix need sat at "terminal result awaits collection" indefinitely.

- Audit results are collected for every configured target.
- Fix units minted from a target's audit carry that target, so they run
  against the right repository.
- Audits that already finished and were never applied are collected on the
  first tick after upgrading; no manual repair is needed.

## Gate-fix units rank first

Fix work was recognised by its `pumpfix` id prefix, but since 19.1.5 gate-fix
needs are ordinary needs with handle ids, so their units queued behind
ordinary work. Units of pump-created gate-fix needs now rank first in unit
dispatch, job dispatch, merge selection and the need-merge cap, bypass merge
holds while the gate is red, and claim the gate pool's reserved slot. The
legacy prefix still works.
