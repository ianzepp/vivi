# Vivi 19.1.1

Patch release on 19.1.0.

## Merge receipts that lost their prefix are still read

A need merge worker ended its receipt with a correct result object but
without the `PUMP_MERGE_RESULT=` prefix. The pump rejected the receipt while
the runner had already published the candidate, so the need branch went
unverified and needed `vivi pump recover`.

When no prefixed line parses, the merge and need-landing receipt parsers now
take the last line that is exactly one JSON object (optionally in a ``` fence
or backticks) and deserializes as the expected result, and log once that the
worker drifted from the format. A prefixed line is still preferred, and the
last prefixed line that parses wins. A JSON fragment inside prose is never
read. Validation of sources, runner proofs and the remote ref is unchanged.
