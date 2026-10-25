# Vivi 21.3.2

Patch release on 21.3.1. Pairs with vivi-server 2.3.2.

## Fix: a host base's flow roles are completed for a new project

The flow roles a new project ensures (admitter, slicer, researcher, head-cto,
repairer) were created when missing but never touched when present. A host
base that defines them with only a pool and model left them without the
`kind` and `read_only` that validation requires, so `vivi project create`
(and the server's `create_project`) failed with "role admitter has kind none".

A role that comes only from a base is now completed with the kind and
read_only its shipped definition needs, when the base set none. A role the
project's own file defines stays exactly as it is, and pool and model are
never changed.
