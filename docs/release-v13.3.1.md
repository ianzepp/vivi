# Vivi 13.3.1

A patch release so quarantine repairs can start on local worktree pools.

## Quarantine repair dispatch

A quarantine repair unit now starts from `<remote>/pump/quarantine/<id>`,
the same shape as `dispatch.from`. A local worktree pool resolves a
`<remote>/<branch>` that its repository has not fetched yet by fetching that
one branch from the configured remote and resolving again. Plain names, shas
and unknown remotes never fetch; a failed fetch keeps the refusal and appends
the fetch error.

## Known limit

Fetch-mode pools (ssh, docker, fly) pass `--from` to the server as is, so a
`<remote>/<branch>` value, including `dispatch.from = "origin/next"`, does
not resolve there yet.

## Compatibility

- Pump database schema: unchanged (5).
- Mailspace schema: unchanged (8).
