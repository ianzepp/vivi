# Phase 05: projects owned by signing keys

**Status: design, nothing built** (operator decisions 2026-10-25). This note is
the contract for the work. Until it is built, `contract.md` and the code
describe what exists; where they disagree with this note, they win.

## The idea

Today a hosted project is created by the host principal (`HOST_OPERATOR_TOKEN`
or a key minted `host: true`). `create_project` mints a fresh principal for the
project and returns its secret once, and every later call presents that secret
as a bearer key that never expires. Deleting a project removes its files and
its lanes at once.

Phase 05 changes who owns a project and how it ends:

- **Anyone who can reach the server creates a project** with an ed25519 key
  pair made on their own machine. They send the public key; it becomes the
  project's first operator. No host key is needed, and no secret ever leaves
  the client or comes back in a response.
- **The private key signs**: a short-lived session for everyday calls, and each
  request that changes whether a project exists or who can reach it.
- **Deleting is two stages.** Archive is the default and keeps everything on
  disk. Purge removes the files, and is allowed only 24 hours or more after
  the archive.

The model follows Minted Geek Swarm's tenant keys (`swarm-key`: public keys
registered at creation, challenge and verify, a two-step dissolve with the
intent sealed in), adapted to vivi-server's grants, where one principal can
hold grants on many projects.

## Settled decisions (operator, 2026-10-25)

1. **Who may create:** any client that reaches the server. On hub, the
   tailnet decides who that is (see Boundary). No host key.
2. **Keys:** ed25519 only, in OpenSSH format (`ssh-keygen -t ed25519`).
   Signatures are SSH signatures (`ssh-keygen -Y sign`, the SSHSIG format) in
   the namespace `vivi`, so a key made for vivi signs nothing else, and the
   private key may stay in ssh-agent.
3. **Source:** a new project or lane is either empty (a bare repository with
   one initial commit) or imported from `https://github.com/<owner>/<repo>`.
   Nothing else is fetched.
4. **Delete defaults to archive.** Purge is a separate request, refused until
   24 hours have passed since the archive. The delay binds every caller, the
   host key included. Nothing purges on its own.
5. **A project's operator may start and stop its own pump.**
6. **Resource limits are deferred.** Every project on hub is the operator's
   own; quotas come when that stops being true.

## Keys and principals

A principal gains public keys beside (and, after migration, instead of) its
bearer keys:

```json
{ "key_id": "pk_…", "algorithm": "ssh-ed25519", "public_key": "ssh-ed25519 AAAA… label",
  "fingerprint": "SHA256:…", "label": "…", "created_at": "…", "revoked_at": null }
```

- **A key is a principal.** The first time a public key proves possession, the
  server records a principal for it with no grants. Creating a project grants
  that principal `operator` on the new project. One key holder who creates
  five projects is one principal with five grants, not five principals with
  five secret files.
- A fingerprint belongs to one principal; registering it again is refused.
- Private key material (PEM or OpenSSH private blocks) is refused at the
  boundary, as swarm does.

## Signing

Every signed request signs a canonical text message that the server builds
and hands out with a fresh nonce:

```text
vivi-server v1
action: <session|create|archive|restore|purge|key-add|key-revoke>
project: <project id, or the requested name for create>
nonce: <server nonce>
expires: <rfc3339>
<action fields, one per line, sorted>
```

- `challenge {action, project?, fields?}` returns `{nonce, message, expires}`.
  It needs no authentication. A nonce is single-use, lives about two minutes,
  and is bound to the action and project it was issued for.
- The client signs `message` with `ssh-keygen -Y sign -n vivi` (or ssh-agent)
  and sends `{nonce, signature, public_key}` with the request.
- The server checks the signature against the stored public key (or, for
  `session` and `create`, against the presented key: that is the proof of
  possession), then that the nonce is live, unused and issued for this exact
  action and project.

Swarm's lesson that carries over: **the intent is sealed in the signature.**
An archive signature cannot be replayed as a purge, and a purge signature for
one project cannot remove another.

## Sessions

- `session` (signed) returns an opaque session token with a lifetime the
  client asks for, default 1 hour, capped at 24 hours. Only its digest is
  stored, in memory: a server restart ends every session, and clients sign in
  again.
- Every call checks the session against the store, as bearer keys are checked
  today: a revoked key or a lost grant fails the next call.
- A session token reaches the everyday tools only. It cannot create a
  project, archive, restore, purge, or add or revoke keys; those need their
  own signature (see Signed actions). A leaked session token can do a day's
  work, and cannot end a project or let anyone else in.

**Clients.** `vivi mcp` (the stdio bridge and `vivi mcp call`) signs in by
itself, from a key file or ssh-agent named by `VIVI_MCP_SIGNING_KEY`, and signs
in again on a 401. For an HTTP MCP client such as Claude Code, which reads its
auth header from a helper script (today `~/.vivi/faberlang-mcp-headers.sh`),
`vivi mcp token` prints a cached session token and renews it near expiry.
Check during the build whether Claude Code re-runs the helper after a 401; if
it does not, the helper asks for the 24-hour cap.

## Signed actions

These need a signature from a key holding `operator` on the project (create:
the presented key), on top of any session:

| Action | Effect |
| --- | --- |
| `create` | Create the project (below); grant the signer operator |
| `archive` | Archive the project (below) |
| `restore` | Undo an archive, at any time before a purge |
| `purge` | Remove an archived project for good, 24 hours or more after its archive |
| `key-add` | Add a public key to the project at `member` or `operator` (a new principal when the key is new) |
| `key-revoke` | Revoke one key's grant on the project |

The rule: **anything that changes whether a project exists, or who can reach
it, is signed.** Everything else (needs, config, roles, rungs, targets, pump
control, reads) takes a session.

A project must keep at least one operator key: `key-revoke` refuses to remove
the last one.

## Creating a project

`create_project {name, public_key, nonce, signature, source?, target?, branch?,
rungs?}`, with no bearer:

- Everything `create_project` does today: `<HOST_LANES_ROOT>/<name>/` with the
  first target's bare repository, the mailspace, the checkout, the
  `[pump.targets.<target>]` entry, the host registry entry.
- The signer's principal gets `operator` on the new project. Nothing secret is
  returned.
- **The pump starts running.** (Today a new project's pump starts stopped, and
  the hub runbook starts it by hand: finding F21.)
- `source`, when given, must match
  `^https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+(\.git)?$` with no user
  info, query or fragment. The fetch runs as the server's user with no
  credentials and `GIT_TERMINAL_PROMPT=0`, so only public repositories import;
  a private one fails fast. A private repository needs a read-only deploy key
  for that user, which is future work. Without `source`, the repository is
  empty with one initial commit.
- `lane_create` follows the same source rule, and becomes a session tool on
  the project's operator (its guarded config write still needs a paused pump).

The rule lives in vivi-server, not in vivi: `vivi project create --source`
run locally by an operator on their own machine still takes any Git address,
local paths included.

**Why the source rule matters.** Today `source` takes any Git address, local
paths and `file://` included (`vivi::project_create::lane::check_source`
refuses only empty, `-`-led and whitespace addresses). Behind the host key
that is fine. With open creation it would let a new project import another
project's private lane, such as `/mnt/vivi/lanes/faberlang/radix.git`. The
GitHub-only rule must ship before, or with, open creation.

## Archive, restore, purge

**Archive** (signed, the default way to end a project):

- Unregisters the project: the pump supervisor stops its pump and the server
  stops serving it. Every project tool answers that the project is archived.
- Keeps everything: the project root, its lanes, and the server data directory
  `DATA_DIR/projects/<id>/`. The name stays taken, because the root still
  exists.
- Records `archived: {at, by, root, lanes_dir}` in the project's server data
  (`project.json`), so the server can still find the project after the
  registry forgets it.
- Keeps every key and grant, limited to `restore` and `purge` on this project.
  (Revoking the keys would make purge, which they must sign, impossible.)
- Needs no wait for the pump's lock: it changes state only. This replaces
  today's "call delete twice" behavior.

**Restore** (signed): re-registers the project as it was (pump and serve as
before the archive), clears `archived`, and resets the purge clock.

**Purge** (signed, `now >= archived.at + 24h`, refused otherwise with the time
it becomes allowed):

- Requires the project to be archived and its pump lock to be free.
- Removes the project root, `<HOST_LANES_ROOT>/<name>/` and
  `DATA_DIR/projects/<id>/`, then every grant on the project.
- Leaves principals in place. A key holder with no grants left is still a
  principal: it can sign in and create another project.

**Host key.** It stays as break-glass: list every project, archive, restore
and purge any of them, and `key-add` an operator key to a project whose only
key was lost. It obeys the 24-hour rule like everyone else. Everyday work
never needs it.

## Boundary

On hub the tailnet is now the gate for creating projects. Today the operator's
devices reach vivi-server (`100.96.126.80:18190`, ufw on `tailscale0`), and
workers (`tag:worker`) reach hub only on ports 22 and 18180. Opening 18190 to
workers, or to anyone else, would let them create projects: that is a
decision to make on purpose, recorded in the `hub` skill.

A project key reaches that project's MCP tools and nothing else. It gives no
SSH, no lane push and no access to hub itself; content still enters a project
only through `source` or a need the pump lands.

## Not changing

- Phase 02 and 03 identity tokens for `vivi sync` and rooms.
- Member and operator grant levels, and the per-call operator rules the routes
  enforce today.
- The lane layout, the host base mailspace, and the pump.

## Migration (a clean break)

1. faberlang: register the operator's public key on its operator grant, point
   the Claude Code `faberlang` server's header helper at `vivi mcp token`,
   then revoke `~/.vivi/faberlang-mcp.key`.
2. Retire bearer keys for project access: `key_mint` and `grant_add` for
   projects give way to `key-add`; the legacy per-project key store
   (`src/keys.rs`, `ApiKeyRegistry`, still mirrored beside
   `DATA_DIR/keys.json`) goes with them.
3. `HOST_OPERATOR_TOKEN` stays a static bearer for now. Moving the host
   principal to a public key too is a later choice.
4. Update the `hub` skill's Project runbook and `scripts/hub-project-new.sh`.

## Build order

Each step ships on its own and is useful alone:

1. **Source rule and pump start** on today's auth: GitHub-only `source` for
   `create_project` and `lane_create`; a new project's pump starts running.
2. **Archive, restore, purge** on today's auth: archive by default, the
   24-hour purge rule, `archived` in `project.json`, one-call archive. The
   safety win does not wait for keys.
3. **Signing keys:** public keys on principals, `challenge`, `session`,
   session-checked tools, and signed `create`.
4. **Signed actions:** archive, restore, purge, `key-add` and `key-revoke`
   need signatures; last-operator guard.
5. **Clients and migration:** `vivi mcp` signing and `vivi mcp token`
   (in vivi), faberlang moved over, bearer project keys and the legacy store
   removed, docs and the hub runbook updated.

## Acceptance

- A client with only a fresh ed25519 key creates a project, works it over
  MCP, archives it, fails to purge it before 24 hours have passed, restores
  it, archives it again, and purges it after 24 hours (the delay is
  configurable in tests only).
- An archive signature presented as a purge, a signature for another project,
  a reused nonce, and an expired nonce are all refused, with nothing changed.
- A session token cannot create, archive, restore, purge, or add or revoke a
  key.
- `source` refuses a local path, `file://`, `ssh://`, a non-GitHub host, and a
  URL with user info; a private GitHub repository fails without hanging.
- The host key cannot purge before 24 hours.
- No response body or log line ever carries a private key or a session token
  after its first issue.
- `scripts/hosted-smoke.sh` runs the whole life cycle against a local server.
