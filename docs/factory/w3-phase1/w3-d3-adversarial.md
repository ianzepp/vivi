adversarial pass: 2 P1 / 1 P2 / 3 P3

Target: `POST /v1/mcp` grant wall on `need/w3-d3` at `6604df0`, vivi pin 21. Local server `127.0.0.1:18141` (`DATA_DIR=/tmp/w3-phase1/data`, `SEND_RATE_PER_MINUTE=0`) and a second server on `18142` with limit 2. No repo writes. Raw `vivi_` secrets are omitted below; key ids are the evidence handles.

## P1 — Revoked and ungranted server keys keep working through the per-project mirror

Story. Self-service mint writes the same secret's SHA-256 into `DATA_DIR/projects/<id>/api_keys.json` so rooms auth and the letter wire accept it. Host MCP `key_revoke` and `grant_remove` update only `ServerKeyStore`. The mirror row stays live (`revoked_at` null). A caller who still holds the secret is dead on `POST /v1/mcp` and alive on `POST /v1/rooms/<id>/mcp`, `GET /v1/projects/<id>/keys`, and letter append. Those side paths have no grant check and no audit line. The same hole is open on a hosted project: rooms MCP is not limited to chats (`require_live_chat` lets durable and hosted ids through). It is same-project, not cross-tenant. HTTP `DELETE /v1/projects/:id/keys/:key_id` already revokes both stores.

Evidence.
- Code: `ApiKeyRegistry::adopt` (`src/keys.rs` 207–239) is called only from self-service `mint_handler` (491–508). `key_admin` `key_revoke` calls `store.revoke_key` and returns (`src/mcp/server.rs` 653–656). `grant_remove` only calls `remove_grant` (669–680). `revoke_handler` revokes the mirror, then the server key (`src/keys.rs` 624–635). Rooms auth is `resolve_project_caller` on the per-project registry (`src/mcp.rs` 49–74). `require_live_chat` (`src/projects_api.rs` 211–212) passes any project that is not a closed chat.
- Live, project `5rw85-j25cv-487wz-60znq-9t6cw`, self-service key `key_2p8GA74V9YA`. Host `key_revoke` returned `{"revoked":"key_2p8GA74V9YA"}`. Then `POST /v1/mcp` ping and overview returned `401 invalid bearer token`. `POST /v1/rooms/<A>/mcp` overview returned `200` with that `project_id`. `GET /v1/projects/<A>/keys` with the same bearer returned `200` and still listed `key_2p8GA74V9YA`. `PUT /v1/projects/<A>/items/msg_bc3d8e35…` returned `201`. The same bearer on `POST /v1/rooms/<B>/mcp` returned `401`. Mirror file after the revoke: `key_2p8GA74V9YA` `revoked_at` null. `msg_bc3d8e35` does not appear in `audit.jsonl`.
- `grant_remove` on principal `creator@<A>`: `/v1/mcp` overview returned `this key has no grant on <A>`; rooms overview with that self-service key returned `200`. Mirror `key_mDfNuWLSNg0` `revoked_at` null.
- Control: a host `key_mint` (not adopted) got rooms `401` and `/v1/mcp` overview `200`.
- Contrast: `DELETE /v1/projects/<A>/keys/key_uoemsGAuwtQ` returned `204`. Both `/v1/mcp` and rooms then returned `401`, and the mirror row has `revoked_at` set.
- Hosted project `bw6y6-py395-7d747-t10zn-weq0y`, self-service key `key_Esv5bdP1qVk` (member `hand1`). Host `key_revoke` made `/v1/mcp` return `401`. `POST /v1/rooms/<hosted>/mcp` overview returned `200` with that `project_id` and `mail_visible: 1`. Mirror `revoked_at` null.

Severity. P1. A credential the host has revoked or ungranted still authorizes reads and writes on the project.

Fix. `key_revoke` and `grant_remove` must revoke or drop that `key_id` in every `api_keys.json` `adopt` wrote.

## P1 — `{member}.{role}` is a principal another member can occupy, and the first grant wins

Story. Self-service mint puts an acting identity on principal `{member}.{role}@{project}` and a no-identity mint on `{member}@{project}` (`src/keys.rs` 474–480). Member names allow `.` (`src/identities.rs` 141, `src/grants.rs` 512–518), so a member literally named `creator.ops` is the same principal as member `creator` acting as `ops`. `ServerKeyStore::mint` appends grants onto the existing principal (`src/grants.rs` 256–259). `resolve_grant` returns the first grant whose `project_id` matches (`src/mcp/server.rs` 411–412) and never consults the later one. The caller therefore acts as the earlier member. On a hosted project that earlier grant was the host member's operator grant, and `pump_control` paused the pump. Any admitted member can introduce the colliding name: member `hand1` vouched `w3d3app.ops` and that name registered and minted. The pump pause itself was vouched with the host member's token, on the same vouch route, after that member had minted `identity=operator`.

Evidence.
- Durable project `<A>`. `creator` minted `identity=ops` first (`key_vaTmuevu_lA`, grant member `creator`, identity `ops`, level `member`). A vouched member `creator.ops` then minted `key_lLbHvwynKpI`. The mint body listed both grants, creator's first. `overview` with that key returned `you.name=creator`, `you.address=creator@11111111.local`. `list` kind `mail` showed the send `msg_d1ca30bdd72b03d61bd33cde` subject `who-am-i` with `from=creator`. Audit line: principal `creator.ops@<A>`, tool `send`, outcome `ok`. The log names the colliding principal; the letter is from `creator`.
- Hosted project `bw6y6-py395-7d747-t10zn-weq0y`, host member `w3d3app`. That member's self-mint `identity=operator` stored level `operator`. Vouch of the name `w3d3app.operator`, then that member's no-identity mint, returned grants in order: `{member:w3d3app, identity:operator, level:operator}` then `{member:w3d3app.operator, identity:null, level:member}`. `pump_control` action `pause` with the colliding key returned `{"action":"pause","by":"operator","mode":"paused"}`. Audit: principal `w3d3app.operator@<hosted>`, tool `pump_control`, outcome `ok`. The operator key then `resume`d the pump back to `running`.
- `hand1` (admitted non-host member) `PUT` a vouch for `w3d3app.ops` and got `201`. Register of that name succeeded and the self-service mint returned a single grant `{member:w3d3app.ops, level:member}` (no prior `identity=ops` grant existed to inherit).

Severity. P1. One member's key acts as another member, and the inherited operator grant runs host pump control.

Fix. Stop using `{member}.{role}` as a principal string members can equal; key the grant by member plus identity, and resolve that pair instead of the first grant on the project.

## P2 — A member-level hosted key acts as the operator role for mail

Story. `self_service_level` stores `member` unless the caller is the host's own member acting as a role of kind operator (`src/mcp/server.rs` 932–950). The comment says tools re-derive that rule per call. Mail tools do not. `dispatch_tool` fills a missing key identity with `DEFAULT_HOSTED_IDENTITY` (`"operator"`, `src/lib.rs` 68, `src/mcp.rs` 234–238) and `live::dispatch` acts as that role. `pump_control` does re-check, and refuses a key minted by anyone but the host member (`src/mcp/live/pump.rs` 152–163). An admitted non-host member therefore reads and sends as operator while the pump stays shut.

Evidence. Hosted project above, member `hand1`.
- Mint with no identity: grant `{member:hand1, identity:null, level:member}`. `overview` returned `you.name=operator`, `you.address=operator@5f0de35b.local`. `send` returned `from=operator` and `delivered[].identity=operator` (`msg_9e873d4c3e9131301c54a78b`).
- `pump_control` pause returned `pump_control needs an API key minted by the host's own member; this key was minted by another member`.
- Mint with `identity=operator`: stored level stayed `member`, `overview` was still `you.name=operator`, and `pump_control` returned the same host-member refusal.
- Control: the create_project operator key (`key_-gK0uud4u-U`, host member, identity `operator`) paused and resumed the pump (`by=operator`).

Severity. P2. Bounded to a member already admitted on that hosted project, and to the mail tools. The pump tools held.

Fix. Have mail tools apply the same operator rule `pump_control` applies, and do not substitute `operator` for a missing identity when that rule fails.

## P3 — Retired project keys come back if the file is renamed, and the marker never sweeps again

Story. First boot renames `api_keys.json` to `api_keys.json.retired` and writes `DATA_DIR/api_keys.retirement`. Later boots return immediately when the marker exists (`src/grants.rs` 579–593). Nothing records the retired digests. Putting the bytes back under the live name re-enables rooms auth, and a restart leaves that file in place. The check-then-rename has no lock.

Evidence.
- On the running server, renaming `<A>/api_keys.json` aside made rooms overview `401`. Copying the bytes back made rooms overview `200` with project `<A>`, without a restart.
- Fresh `DATA_DIR=/tmp/w3-phase1/data-marker2` with `projects/not-a-real-id/api_keys.json`. First boot logged `retired 1 legacy api key(s)`, removed the live file, and wrote the marker. The file was moved back. Second boot on the same directory left `api_keys.json` in place (`retired` absent, marker still present).

Severity. P3. Requires a write to `DATA_DIR`. A local writer can already read the store. The footgun is that retirement looks permanent and is not.

Fix. Remember retired digests and refuse them, and hold a lock across the marker check and the sweep.

## P3 — Audit mode 0600 is applied only when the file is created

Story. `append_private` opens `DATA_DIR/audit.jsonl` with mode `0600` (`src/audit.rs` 93–101). On Unix that mode applies to creation, not to an existing inode. A file that already exists at `0644` stays `0644` while audited writes append to it.

Evidence. After `chmod 0644` of the live `audit.jsonl`, a host `key_mint` returned `200` and `stat` still showed `0o644`. Mode was set back to `0600` on the temp file afterwards. A freshly created log in the first probe was `0o600`.

Severity. P3. Requires a local writer who can create or chmod the file before the server appends.

Fix. After open, fchmod the descriptor to `0600`, or refuse to append when the mode is looser.

## P3 — Secret compare bails out on length, and key verify stops at the first live digest

Story. `constant_time_eq` is `subtle` `ct_eq` on the byte slices (`src/auth.rs` 112–114`). `subtle` 2.6.1 documents that `[T]::ct_eq` returns immediately when the lengths differ (`subtle-2.6.1/src/lib.rs` 314–319). The host operator token is compared raw, before any hash (`src/mcp/server.rs` 95–97), so a length mismatch takes a shorter path than an equal-length miss. `ServerKeyStore::verify` hashes the presented secret first (presented length is not the compare length), then walks the index with `!revoked && constant_time_eq` and stops at the first match (`src/grants.rs` 450–456). Revoked rows are skipped before the compare.

Evidence. Code paths above. No timing delta was measured; the digests are high entropy, so this is a length and match-position leak, not a practical key recovery. Empty `HOST_OPERATOR_TOKEN` is dropped at boot (`src/main.rs` 58–60). A bearer of `vivi_` plus the host token returned `401 invalid bearer token`.

Severity. P3.

Fix. Compare the host token with a length-hiding compare, and scan every key digest instead of returning at the first live match.

## CHECKED-NOT-FOUND

Cross-tenant project argument. Key minted on project `5rw85-j25cv-487wz-60znq-9t6cw` (A). `overview` and `send` with B's id `02aad-435m0-6es31-aehe6-q6ndb` returned `this key has no grant on <B>`. The same denial for B's uppercase id, padded and internal spaces, a newline joining A and B, `../<B>`, `/tmp/<B>`, a leading NUL, empty string, unknown `no-such-project`, A's own uppercase and padded forms, and Crockford homoglyphs (`o`/`i`/`l`) of A and B. `GET /v1/projects/<B>/items` with B's own token then showed `records: []`. A JSON-RPC batch of one send to B and one send to A returned the no-grant error and one accepted send on A (`msg_70a2c158…`, `from=creator`). A's key `overview` with hosted directory name `w3d3app`, hosted id `bw6y6-py395-7d747-t10zn-weq0y`, that id uppercased, and the string `creator` all returned no-grant. Host operator token `overview` of A returned no-grant; `projects` returned `{"host":true}`. A minted host principal (`host: true`, no grants) did the same. Dispatch overview of A returned A's canonical `project_id`. `resolve_grant` uses the grant's id after an exact match, and only then a hosted name among that key's own grants (`src/mcp/server.rs` 406–433). `load_meta` canonicalizes through `normalize_room_id` before any directory join (`src/projects.rs` 1134–1141), so `../` never reaches the filesystem.

Rooms as a cross-tenant door. A's mirror key on rooms URL of B, of B's homoglyph (`6es3i`), and of B's uppercase id: all `401`. A's homoglyph URL (`6oznq`) with A's own key: `401` (rooms auth joins the raw URL segment, `src/keys.rs` 107, so the alias does not see A's key file). A's exact rooms URL: `200` and `project_id` A. Challenge on B's homoglyph returned `200` (existence check canonicalizes), and register of a new key there returned `409 admission_required`. Closed chat `xtkz9-fmp2h-s79j1-mrq34-d37ft`: overview while live `200`, after close `this key has no grant`. Homoglyph and uppercase rooms URLs of that closed chat returned `410 room_closed` before auth.

Key auth edges. No header, empty authorization, `bearer` in lowercase, `Basic`, and `Bearer` plus a second space: `401 bearer token required` or the identity-token refusal. `Bearer Bearer <key>` and a raw identity token: `401 identity tokens are not accepted on /v1/mcp`. Bare `vivi_` and `vivi_` plus the host token: `401 invalid bearer token`. Exact host token: ping `200`. A trailing space after the host token also pinged `200` on a raw socket; HTTP field parsing strips trailing OWS, and the two-space case shows the server does not trim the token itself. `key_mint` with `host: true` and a grant: `a host principal carries no project grants`. Non-host calls to `grant_add`, `key_mint`, `key_list`, `key_revoke`, `create_project`: `this key is not a host principal`. `keys.json` and `api_keys.json` store digests. Audit scan for the raw secrets and the host token found none (`audit.jsonl` mode `0600` at create, 12 lines in the first pass). Server log hits for `vivi_` are the crate name `vivi_server`, not secrets. `summarize_ok` for `key_mint` records key id and principal (`src/mcp/server.rs` 875–882).

Rate limit and audit of `/v1/mcp` writes. Limit 2 on `:18142`. One JSON-RPC batch of 4 sends: 2 accepted (`sequence` 1 and 2), then two `over 2 writes/min for this key`. Two host `key_mint` calls still succeeded (bucket `host`, `src/mcp/server.rs` 575), and the third host mint returned the same cap. A second project key's send still succeeded (`sequence` 3). Seat for a missing `key_id` is the principal `host` (`allow_write`, 780–784), which is a different bucket string from a project id. Refused cross-tenant sends are in `audit.jsonl` (`outcome=refused`, no `project_id`). The accepted send is `outcome=ok` with `project_id`. `key_list` is not a write. A batch `Err` returns immediately (`src/mcp.rs` 119–126) after earlier calls have already run; that path was not triggered. Tool errors inside a batch do not abort it (the mixed batch above).

Body and batch abuse. A 3MB body returned `400 request body could not be read`. A 2000-deep nested array returned JSON-RPC parse error `-32700`. A batch of 1000 pings returned `200` with 1000 results in about 0.01s. Ping afterwards `200`.

Grant snapshot. `verify` copies grants at authentication (`src/grants.rs` 462–466). A grant change in a later tool call of the same request does not rewrite the principal already built. Not raced in-process beyond that.

## pass 2

adversarial pass 2: 0 P1 / 1 P2 / 0 P3

Target: the same three findings, on `need/w3-d3` at `bd8b849` (merge of `d8fb261`, W3D3-5). Local server `127.0.0.1:18151`, `DATA_DIR=/tmp/w3-phase1/pass2`, `SEND_RATE_PER_MINUTE=0`. Durable projects `9grb0-7tdz0-5pttx-ce6k3-hewyr` (A) and `ypscz-nvfrd-t8f0n-hcny2-dht1r` (B). Hosted project `fvmrm-x6kc9-0bx62-m2nna-p9b97` (member `w3d3p2`, mailspace `7ee98a74-d362-40be-9854-156aab256938`). The debug binary already contained the fix strings; no rebuild. Pass 1's three P3s were left as filed.

### Revocation gap — closed

`key_revoke` and `grant_remove` revoke the per-project mirror first and the server key second (`src/mcp/server.rs` 660–674 and 687–709). A storage failure returns the tool error `storage failure` and leaves both sides live.

Observed.

- Self-service `key_XEHNlsXcgpU` on A. Before revoke: rooms overview `200`, `GET /v1/projects/<A>/keys` `200`, letter append `201` (`msg_babc358a2988e60bb4bd8be5`). Host `key_revoke`: `/v1/mcp` overview `401`, ping `401`, rooms `401`, key list `401`, letter `401`. Mirror row `revoked_at` `2026-10-24T06:22:35Z`.
- Fresh mint, then `grant_remove` of principal `creator@<A>`: `/v1/mcp` returned `this key has no grant on <A>`. Rooms, key list, and letter all `401`. Mirror `key_1wIJFMXmMSg` `revoked_at` set.
- Two projects. Mint `key_YP4IWsQ-DBU` on A, then host `grant_add` of member `creator` on B. `/v1/mcp` overview of B returned `200`, `you.name=creator`. Rooms on B returned `401` the whole time: `grant_add` does not adopt, and B's `api_keys.json` was absent. `grant_remove` of A only: `/v1/mcp` on A no grant, rooms on A `401`, `/v1/mcp` on B still `200` with `you.name=creator`. Mirror A has `key_YP4IWsQ-DBU` revoked; mirror B still missing. A later `key_revoke` of that key made `/v1/mcp` on B `401`. The surviving B grant is the intended case.
- Storage order. `chmod 0555` on A's project directory, then `key_revoke` of `key_NMl7roTRqZ0`, returned `storage failure`. With the directory restored to `0755` before any further revoke: `/v1/mcp` overview `200`, rooms `200`, mirror `revoked_at` null. The following `key_revoke` then made rooms and `/v1/mcp` ping `401`.
- Hosted. `hand1` key `key_mNH5sJ1KNzc` revoked: rooms `401`, `/v1/mcp` ping `401`, that mirror row revoked. Sibling `key_mjwiIBGKiw8` (own member, `identity=operator`) stayed `revoked_at` null.

No successful `key_revoke` or `grant_remove` left that project's mirror accepting the secret.

### Principal collision — closed for self-service mint

`mint_derived` refuses when the derived principal already exists under a different member/identity pair (`src/grants.rs` 337–387). Self-service mint maps that to HTTP `409` `principal_conflict` (`src/keys.rs` 594–595). The principal string is still `{member}.{role}@{project}` or `{member}@{project}` (`src/grants.rs` 622–627). `resolve_grant` still returns the first grant whose project id matches (`src/mcp/server.rs` 413).

Observed, both orders.

- A, identity first. `creator` minted `identity=ops` (`key__XRiMKZKm8I`, one grant: member `creator`, identity `ops`, level `member`). Vouch and register of member `creator.ops`, then that member's no-identity mint: HTTP `409` `principal_conflict`. Body: principal `creator.ops@<A>` is already derived from member `creator` acting as `ops`; this mint derives it from member `creator.ops` with no acting identity.
- B, dotted member first. Member `creator.ops` minted first (grant member `creator.ops`, identity null, level `member`). Overview `you.name=creator.ops`, address `creator.ops@44444444.local`. Then `creator` with `identity=ops`: HTTP `409`. Body: principal `creator.ops@<B>` is already derived from member `creator.ops` with no acting identity; this mint derives it from member `creator` acting as `ops`.
- `identity=Ops` is a different principal and minted `201` (`key_0iBGG4UAbOQ`). On the durable project the tool ignores the acting identity: overview `you.name=creator`.
- Hosted `create_project` key `key_KYjJK89vqGY` uses `mint` on principal `w3d3p2@<H>` with identity `operator` (`src/mcp/host.rs` 224–242), not `mint_derived`. The own member's later no-identity self-mint of that same principal: HTTP `409` (already derived from member `w3d3p2` acting as `operator`). After that member also self-minted `identity=operator` (principal `w3d3p2.operator@<H>`, level `operator`), a vouched member literally named `w3d3p2.operator` minted and got HTTP `409` for that second principal. The durable reverse order above is the same `mint_derived` check; it was not repeated on a second hosted project.

Checked, host token only. Host `key_mint` and `grant_add` still call `mint` / `add_grant`, which append a grant onto an existing principal (`src/grants.rs` 274–277 and 505). Host `key_mint` onto principal `creator.ops@<B>` with a grant for member `creator` returned both grants, `creator.ops` first. Overview with the merged key and with the original dotted key stayed `you.name=creator.ops`. A following `grant_add` of `{member:creator, identity:sneak, level:operator}` on that principal was accepted. The first grant is the one `resolve_grant` returns, so the added member does not become the actor. That is host-token power plus first-grant-wins. It does not let a non-host key act as another member.

### Acting-as — silent operator default closed; named operator identity still open

A key with no named identity now acts as the grant's member, unless that member is the mailspace's own member, in which case it acts as the default identity (`src/mcp.rs` 261–273, `src/mcp/live.rs` 112–124). `pump_control` still requires the host's own member (`src/mcp/live/pump.rs` 152–172).

Observed, the three checks that hold.

- No named identity, non-host member `hand1`, before any role of that name existed. Mint stored `{member:hand1, identity:null, level:member}`. Overview and send returned `this key acts as identity 'hand1', which is not a role of the project mailspace`. `pump_control` returned `pump_control needs an API key minted by the host's own member; this key was minted by another member`. The call did not act as `operator`.
- Same key shape after `vivi role add hand1 --kind hand` on the temp mailspace, remint `key_jXbU_CnsdSY` (level `member`, identity null). Overview `you.name=hand1`, address `hand1@7ee98a74.local`. Send `kind=mail` returned `from=hand1`, `delivered[].identity=hand1`, `msg_8aeb5bf7621bec74ddff3358`. `pump_control` returned the same host-member refusal.
- Own-member fallback. Host `key_mint` principal `fallback@<H>`, grant `{member:w3d3p2, level:member}` with no identity. Overview `you.name=operator`, address `operator@7ee98a74.local`. Send `from=operator`, `delivered[].identity=operator`, `msg_e01f7aaa62e5c6a20018664d`. `pump_control` pause returned `by=operator`, `mode=paused`. The create_project key resumed the pump. The own member's self-service no-identity mint is the `409` above, because `create_project` already occupies `w3d3p2@<H>` as acting `operator`; the fallback was reached through the host-minted grant.

## P2 — A non-host key named `identity=operator` still sends mail as operator

Story. `self_service_level` stores `member` when the caller is not the host's own member (`src/mcp/server.rs` 971–988). The comment says the tools re-derive that rule per call. The mail path does that only when the key names no identity. A named identity is copied through as the actor (`src/mcp.rs` 269–272) and is not passed back through `operator_rule_holds`. `pump_control` does re-check, and refuses. An admitted non-host member who mints `identity=operator` therefore reads and sends as the operator role while the pump stays shut. The missing-identity half of the pass 1 finding is closed; this half is the same bug with the identity string supplied by the minter.

Evidence. Hosted project above, member `hand1`. Mint `identity=operator`: HTTP `201`, grant `{member:hand1, identity:operator, level:member}`. Overview `you.name=operator`, address `operator@7ee98a74.local`. Send `from=operator`, `delivered[].identity=operator`, `msg_2c4d72ce135e19d6ef56a3d6`. `pump_control` pause returned `pump_control needs an API key minted by the host's own member; this key was minted by another member`.

Severity. P2. Bounded to a member already admitted on that hosted project, and to the mail tools. The pump tools held.

Fix. Before mail tools honor a named identity, apply the same own-member operator rule `pump_control` applies. A non-host member's `identity=operator` must not act as that role.

## delivering-session ruling on pass-2 P2 (accepted pre-existing, not fixed in-need)

The finding (a non-host key minted with identity=operator sends mail as operator while
pump_control refuses) is v20 behavior the wave-3 goal explicitly preserves: §3.4 "Self-service
stays" and "No stored level: operator is derived per call" — the stored level is metadata,
operator tools re-check the host's-own-member + operator-kind rule per call (which is why
pump_control refuses), and mail tools have never carried an operator gate: a per-project key
minted with identity=X acted as X in v20 exactly the same way. Changing it would alter ruled
behavior inside a need. Filed as an operator question for after the release: should a member
be able to self-mint a key that acts as the operator identity for sends?
