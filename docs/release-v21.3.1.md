# Vivi 21.3.1

Patch release on 21.3.0. Pairs with vivi-server 2.3.1.

## Fix: role writes on roles that come from a base

`vivi role set`, the MCP `role_set`, and the starter roles a new project gets
edited only roles defined in the project's own file. On a project whose roles
come from a host base, every such write failed with "unknown local role", and
so did `vivi project create` (and the server's `create_project`) as soon as
the base defined a starter role such as `merge`.

A write on a base-only role now appends an override entry to the project's
file holding only the changed keys; the merge applies it key by key over the
base role. Clearing an inherited key cannot work through an override, so it
is refused and names the `unset = ["identities.<role>.<key>"]` form.
