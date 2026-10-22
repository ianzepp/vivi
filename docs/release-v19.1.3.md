# Vivi 19.1.3

Patch release on 19.1.2: hands spread across a pool's models.

## Least-loaded model choice

Since 19.0 a pool's models come from `[pools.<name>.caps]`, a table that
iterates alphabetically. The dispatcher took the first model under its cap, so
in a pool with several models every hand went to the alphabetically first one
and the others sat idle until it filled.

A hand now picks the model with the lowest load ratio (in-flight over cap)
among the models still under their cap. A model with more free slots wins a
tie of ratio, and remaining ties are broken at random. A retry still avoids the
previous attempt's model while another model has room, and falls back to it
only when it is the one model with room. The odd-attempt reversal of the model
order is gone; least-loaded choice with skip-last already gives retries a
different model.

A role that names a concrete model slug, and the final last-resort attempt,
behave as before. No configuration change is needed.
