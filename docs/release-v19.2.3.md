# Vivi 19.2.3

Patch release on 19.2.2: any role can answer a parked plan.

A parked verdict (`design`, `hold`, `operator`) was planned again only when
the thread gained a reply from the pump's `from` identity (the operator by
default), so a CTO or a head answering in its own name was ignored. Now any
reply newer than the plan from anyone but the pump itself re-plans the need on
the next board pass. The pump's own replies, and the event letters it holds as
the item's holder, come from `reply_from` and never count.

This is what a scoping loop needs: a CTO files a need, asks for a scoping pass
(the planner answers with a `design` verdict and the analysis), then approves
in its own name and the pump plans implementation units.
