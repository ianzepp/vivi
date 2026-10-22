# Vivi 19.2.2

Patch release on 19.2.1: a parked plan verdict keeps its whole reason.

A plan's reason was cut to 400 characters when the pump recorded it. That is
enough for a units plan's one-line note, but a parked verdict (`design`,
`hold`, `operator`) carries its whole answer in the reason and is posted on
the need thread for a ruling, so a scoping analysis reached the thread cut off
mid-word. Parked verdicts now keep up to 16,000 characters; units plans keep
400 as before.
