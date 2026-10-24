//! In-process tests of the project HTTP API: identity proof-of-possession,
//! token renewal, the directory, and the letter wire (raw `.eml` bytes in,
//! the same bytes out, content-addressed duplicates, scoped visibility).

use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::util::ServiceExt;
use vivi_server::api_router;

const STORE_KEY_SEED: u8 = 0x42;

fn store_key() -> String {
    STANDARD.encode([STORE_KEY_SEED; 32])
}

/// A fresh, empty data dir per test so file-walking assertions are exact.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-project-api-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn spki_pem(key: &SigningKey) -> String {
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(key.verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Traveling id shape: `msg_` plus 24 lowercase hex.
fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

/// A deterministic canonical mailspace id for `name`: lowercase hyphenated
/// 8-4-4-4-12 hex derived from the name, so every test identity gets its own
/// stable prefix.
fn mailspace_for(name: &str) -> String {
    let hex = sha256_hex(name.as_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The prefix of `mailspace_for(name)`.
fn prefix_for(name: &str) -> String {
    mailspace_for(name)[..8].to_owned()
}

/// `name@<prefix>.local`: the address of `name`'s mailspace role.
fn address(name: &str) -> String {
    format!("{name}@{}.local", prefix_for(name))
}

/// A letter with LF line ends whose bytes are unique per `(id, body)`.
fn eml(id: &str, body: &str) -> Vec<u8> {
    format!("From: a@p.local\nTo: b@p.local\nMessage-ID: <{id}@p.local>\n\n{body}").into_bytes()
}

/// Raw letter bytes: `headers` (no trailing blank line) then `body`.
fn raw_letter(headers: &str, body: &str) -> Vec<u8> {
    format!("{headers}\r\n\r\n{body}").into_bytes()
}

const FORMAT_ONE_NEED: &str = "X-Vivi-Format: 1\r\nX-Vivi-Kind: need";

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut headers = Vec::new();
    if body.is_some() {
        headers.push(("content-type".to_owned(), "application/json".to_owned()));
    }
    let bytes = body.map(|v| v.to_string().into_bytes()).unwrap_or_default();
    raw_request(app, method, uri, bytes, token, headers).await
}

/// One request with an arbitrary byte body and explicit headers; the
/// response body is JSON (or empty, as `Value::Null`).
async fn raw_request(
    app: &Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    token: Option<&str>,
    headers: Vec<(String, String)>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    // Framework-level rejections (405) carry no body at all.
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    request(app, method, uri, body, token).await
}

/// `PUT` raw letter bytes to `/items/{path_id}` with the given extra headers
/// (envelope fields, a stray Message-ID, ...). `content-type` is always
/// `message/rfc822`, as clients send it.
async fn put_with(
    app: &Router,
    project: &str,
    token: Option<&str>,
    path_id: &str,
    raw: &[u8],
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut all = vec![("content-type".to_owned(), "message/rfc822".to_owned())];
    all.extend(headers.iter().map(|(n, v)| (n.to_string(), v.to_string())));
    raw_request(
        app,
        "PUT",
        &format!("/v1/projects/{project}/items/{path_id}"),
        raw.to_vec(),
        token,
        all,
    )
    .await
}

/// Append raw letter bytes with optional envelope headers.
async fn append(
    app: &Router,
    project: &str,
    token: &str,
    id: &str,
    raw: &[u8],
    to: Option<&str>,
    kind: Option<&str>,
) -> (StatusCode, Value) {
    let mut headers = Vec::new();
    if let Some(to) = to {
        headers.push(("x-vivi-to", to));
    }
    if let Some(kind) = kind {
        headers.push(("x-vivi-kind", kind));
    }
    put_with(app, project, Some(token), id, raw, &headers).await
}

async fn read(app: &Router, project: &str, token: &str, since: u64) -> Value {
    let (status, items) = send(
        app,
        "GET",
        &format!("/v1/projects/{project}/items?since={since}"),
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{items}");
    items
}

fn ids(items: &Value) -> Vec<String> {
    items["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["message_id"].as_str().unwrap().to_owned())
        .collect()
}

async fn challenge(app: &Router, project: &str, signing_key: &SigningKey) -> (String, Vec<u8>) {
    let (status, challenge) = send(
        app,
        "POST",
        &format!("/v1/projects/{project}/identities/challenge"),
        Some(json!({"public_key_pem": spki_pem(signing_key)})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let bytes = STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    (
        challenge["challenge_id"].as_str().unwrap().to_owned(),
        bytes,
    )
}

/// Register `name` with `mailspace_for(name)`.
async fn register(
    app: &Router,
    project: &str,
    signing_key: &SigningKey,
    name: &str,
) -> (StatusCode, Value) {
    register_with(app, project, signing_key, name, json!(mailspace_for(name))).await
}

/// Register with an explicit `mailspace_id` JSON value; `Value::Null` omits
/// the field entirely.
async fn register_with(
    app: &Router,
    project: &str,
    signing_key: &SigningKey,
    name: &str,
    mailspace_id: Value,
) -> (StatusCode, Value) {
    let (challenge_id, bytes) = challenge(app, project, signing_key).await;
    let signature = STANDARD.encode(signing_key.sign(&bytes).to_bytes());
    let mut body = json!({
        "name": name,
        "public_key_pem": spki_pem(signing_key),
        "challenge_id": challenge_id,
        "signature_base64": signature,
    });
    if !mailspace_id.is_null() {
        body["mailspace_id"] = mailspace_id;
    }
    send(
        app,
        "POST",
        &format!("/v1/projects/{project}/identities"),
        Some(body),
        None,
    )
    .await
}

/// Renew a token: fetch a challenge for `challenge_key`, sign it with
/// `signing_key`, and present it for `name`.
async fn renew_with(
    app: &Router,
    project: &str,
    name: &str,
    challenge_key: &SigningKey,
    signing_key: &SigningKey,
) -> (StatusCode, Value) {
    let (challenge_id, bytes) = challenge(app, project, challenge_key).await;
    let signature = STANDARD.encode(signing_key.sign(&bytes).to_bytes());
    send(
        app,
        "POST",
        &format!("/v1/projects/{project}/auth/token"),
        Some(json!({
            "name": name,
            "challenge_id": challenge_id,
            "signature_base64": signature,
        })),
        None,
    )
    .await
}

/// A project with its creator registered.
struct Fixture {
    app: Router,
    dir: PathBuf,
    project: String,
    token: String,
}

async fn fixture(name: &str, creator_seed: u8) -> Fixture {
    let dir = fresh_dir(name);
    let app = api_router(dir.clone(), Some(store_key()), None);
    let creator_key = key(creator_seed);
    let (status, project) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "creator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project = project["project_id"].as_str().unwrap().to_owned();
    let (status, creator) = register(&app, &project, &creator_key, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{creator}");
    let token = creator["token"].as_str().unwrap().to_owned();
    Fixture {
        app,
        dir,
        project,
        token,
    }
}

/// A governance letter: headers, blank line, vouch JSON.
fn vouch_letter(id: &str, name: &str, signing_key: &SigningKey) -> Vec<u8> {
    let body = json!({"vouch": {"name": name, "public_key_pem": spki_pem(signing_key)}});
    eml(id, &body.to_string())
}

impl Fixture {
    async fn append(
        &self,
        id: &str,
        raw: &[u8],
        to: Option<&str>,
        kind: Option<&str>,
    ) -> (StatusCode, Value) {
        append(&self.app, &self.project, &self.token, id, raw, to, kind).await
    }

    /// The creator vouches `name` for `signing_key`, then it registers;
    /// returns its token.
    async fn add_member(&self, vouch_id: &str, name: &str, signing_key: &SigningKey) -> String {
        let raw = vouch_letter(vouch_id, name, signing_key);
        let (status, vouched) = self.append(vouch_id, &raw, Some("postmaster"), None).await;
        assert_eq!(status, StatusCode::CREATED, "{vouched}");
        let (status, identity) = register(&self.app, &self.project, signing_key, name).await;
        assert_eq!(status, StatusCode::CREATED, "{identity}");
        identity["token"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn project_identity_auth_and_items_wire() {
    let fx = fixture("auth-wire", 1).await;
    let (status, unadmitted) = register(&fx.app, &fx.project, &key(2), "builder").await;
    assert_eq!(status, StatusCode::CONFLICT, "{unadmitted}");
    assert_eq!(unadmitted["error"]["code"], "admission_required");

    let token = fx.add_member(&traveling(99), "builder", &key(2)).await;
    let (status, wrong_key) = register(&fx.app, &fx.project, &key(3), "builder").await;
    assert_eq!(status, StatusCode::CONFLICT, "{wrong_key}");
    assert_eq!(wrong_key["error"]["code"], "admission_required");
    let (status, guest) = register(&fx.app, &fx.project, &key(4), "guest").await;
    assert_eq!(status, StatusCode::CONFLICT, "{guest}");

    for n in 1..=2u64 {
        let id = traveling(n);
        let (status, created) = append(
            &fx.app,
            &fx.project,
            &token,
            &id,
            &eml(&id, &format!("body {n}")),
            None,
            Some("need"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
    }
    let (status, unauthorized) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/items?since=0", fx.project),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{unauthorized}");

    let items = read(&fx.app, &fx.project, &token, 0).await;
    assert_eq!(items["next_cursor"], 3);
    let records = items["records"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["sequence"], 1);
    assert_eq!(records[0]["from"], "creator");
    assert_eq!(records[0]["to"], "postmaster");
    assert_eq!(records[0]["kind"], Value::Null);
    let first = STANDARD
        .decode(records[0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        first,
        vouch_letter(&traveling(99), "builder", &key(2)),
        "the vouch letter is served byte for byte"
    );
    assert_eq!(records[2]["from"], "builder");
    assert_eq!(records[2]["kind"], "need");
    assert_eq!(records[2]["to"], Value::Null);
}

#[tokio::test]
async fn the_list_advertises_item_events_on_every_page() {
    let fx = fixture("item-events", 1).await;
    let token = fx.token.clone();
    // The advertisement is on the object itself, so a probe of an empty
    // page past the end of the log reads it too.
    let first = read(&fx.app, &fx.project, &token, 0).await;
    assert_eq!(first["item_events"], 1, "{first}");
    let past = read(&fx.app, &fx.project, &token, 999).await;
    assert!(past["records"].as_array().unwrap().is_empty(), "{past}");
    assert_eq!(past["item_events"], 1, "{past}");
    // A filtered, limited list carries it as well.
    let (status, filtered) = send(
        &fx.app,
        "GET",
        &format!(
            "/v1/projects/{}/items?since=0&limit=1&kind=need",
            fx.project
        ),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{filtered}");
    assert_eq!(filtered["item_events"], 1, "{filtered}");
}

#[tokio::test]
async fn project_directory_lists_authenticated_members_sorted_by_name() {
    let fx = fixture("directory", 21).await;
    let zulu = fx.add_member(&traveling(98), "zulu", &key(20)).await;
    assert!(!zulu.is_empty());
    let uri = format!("/v1/projects/{}/directory", fx.project);

    let (status, missing_auth) = send(&fx.app, "GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{missing_auth}");
    let (status, invalid_auth) = send(&fx.app, "GET", &uri, None, Some("not-a-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{invalid_auth}");

    let (status, directory) = send(&fx.app, "GET", &uri, None, Some(&fx.token)).await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    let names: Vec<&str> = directory["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["creator", "zulu"]);
    assert_eq!(
        directory["members"][0]["fingerprint"]
            .as_str()
            .unwrap()
            .len(),
        64
    );

    let unknown_project_id = format!(
        "{}{}",
        if fx.project.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &fx.project[1..]
    );
    let (status, unknown) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{unknown_project_id}/directory"),
        None,
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
}

#[tokio::test]
async fn letter_round_trip_is_byte_exact_and_hashes_match() {
    let fx = fixture("round-trip", 31).await;
    // Non-UTF-8 bytes, CRLF line ends, NUL, and no trailing newline.
    let mut raw = b"From: a@p.local\r\nSubject: bytes\r\n\r\nbody \xff\xfe\x00\x80 end".to_vec();
    raw.extend_from_slice(b"\r\nlast line without newline");
    let expected = sha256_hex(&raw);

    let id = traveling(1);
    let to = address("builder");
    let (status, created) = fx.append(&id, &raw, Some(&to), Some("task")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["message_id"], id);
    assert_eq!(created["sequence"], 1);
    assert_eq!(created["content_sha256"], expected);
    assert!(created.get("duplicate").is_none());

    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let record = &items["records"][0];
    assert_eq!(record["content_sha256"], expected);
    assert_eq!(record["to"], to);
    assert_eq!(record["kind"], "task");
    let served = STANDARD
        .decode(record["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, raw);
    assert_eq!(sha256_hex(&served), expected);
}

#[tokio::test]
async fn idempotency_matrix() {
    let fx = fixture("idempotency", 41).await;
    let first_id = traveling(1);
    let raw = eml(&first_id, "original");
    let hash = sha256_hex(&raw);

    // Fresh: 201.
    let (status, fresh) = fx.append(&first_id, &raw, None, Some("want")).await;
    assert_eq!(status, StatusCode::CREATED, "{fresh}");
    assert_eq!(fresh["sequence"], 1);

    // Same id, same bytes: 200 duplicate, original sequence.
    let (status, same) = fx.append(&first_id, &raw, None, Some("want")).await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["duplicate"], true);
    assert_eq!(same["message_id"], first_id);
    assert_eq!(same["sequence"], 1);
    assert_eq!(same["content_sha256"], hash);

    // Same id, different bytes: 409 and nothing written.
    let (status, conflict) = fx
        .append(&first_id, &eml(&first_id, "tampered"), None, Some("want"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], "conflict");

    // Same bytes under a new id: 200 duplicate naming the ORIGINAL id and
    // sequence; the new id is never stored.
    let alias = traveling(2);
    let (status, aliased) = fx.append(&alias, &raw, None, Some("want")).await;
    assert_eq!(status, StatusCode::OK, "{aliased}");
    assert_eq!(aliased["duplicate"], true);
    assert_eq!(aliased["message_id"], first_id);
    assert_eq!(aliased["sequence"], 1);

    // The log holds exactly the first letter, and the next fresh letter
    // takes the next sequence (nothing above consumed one).
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![first_id.clone()]);
    let next = traveling(3);
    let (status, next_created) = fx.append(&next, &eml(&next, "next"), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{next_created}");
    assert_eq!(next_created["sequence"], 2);
}

/// Push `raw` and require `422 letter_format` whose message is the parser's.
async fn assert_letter_format(fx: &Fixture, id: &str, raw: &[u8]) -> Value {
    let expected = vivi::letter::of_message(raw, "")
        .expect_err("a refused letter is not one the parser accepts");
    assert!(
        !matches!(expected, vivi::letter::LetterError::Legacy { .. }),
        "legacy must not be refused: {expected}"
    );
    let (status, value) = fx.append(id, raw, None, Some("need")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{value}");
    assert_eq!(value["error"]["code"], "letter_format");
    assert_eq!(value["error"]["message"], expected.to_string());
    value
}

#[tokio::test]
async fn a_broken_or_unknown_format_claim_is_refused_and_stores_nothing() {
    let fx = fixture("letter-format", 61).await;
    assert!(ids(&read(&fx.app, &fx.project, &fx.token, 0).await).is_empty());

    // Format 1 with an unknown front-matter key.
    let unknown_key = raw_letter(
        FORMAT_ONE_NEED,
        "+++\ndone_when = [\"it works\"]\nbogus = 1\n+++\n\nBuild the thing.\n",
    );
    let refused = assert_letter_format(&fx, &traveling(1), &unknown_key).await;
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bogus"),
        "{refused}"
    );

    // Format 1 need front with no done_when.
    let no_done_when = raw_letter(
        FORMAT_ONE_NEED,
        "+++\ntarget = \"vivi\"\n+++\n\nBuild the thing.\n",
    );
    let refused = assert_letter_format(&fx, &traveling(2), &no_done_when).await;
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("done_when"),
        "{refused}"
    );

    // A format this vivi does not read.
    let unknown_format = raw_letter(
        "X-Vivi-Format: 2\r\nX-Vivi-Kind: need",
        "+++\ndone_when = [\"it works\"]\n+++\n\nBuild the thing.\n",
    );
    let refused = assert_letter_format(&fx, &traveling(3), &unknown_format).await;
    assert!(
        refused["error"]["message"].as_str().unwrap().contains('2'),
        "{refused}"
    );

    // The same broken bytes again are still a refusal, not a duplicate:
    // nothing was stored for replay to find.
    assert_letter_format(&fx, &traveling(1), &unknown_key).await;
    assert!(ids(&read(&fx.app, &fx.project, &fx.token, 0).await).is_empty());
}

#[tokio::test]
async fn a_format_one_letter_that_parses_is_accepted() {
    let fx = fixture("letter-format-ok", 62).await;
    let id = traveling(1);
    let raw = raw_letter(
        FORMAT_ONE_NEED,
        "+++\ndone_when = [\"it works\"]\n+++\n\nBuild the thing.\n",
    );
    assert!(vivi::letter::of_message(&raw, "").is_ok());
    let (status, created) = fx.append(&id, &raw, None, Some("need")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![id]);
    let served = STANDARD
        .decode(items["records"][0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, raw);
}

/// A letter with no `X-Vivi-Format` header is legacy and is still filed.
/// W3-B4 tightens this; that change has to move this test on purpose.
#[tokio::test]
async fn a_legacy_letter_without_a_format_header_is_still_accepted() {
    let fx = fixture("letter-legacy", 63).await;
    let id = traveling(1);
    let raw = raw_letter("X-Vivi-Kind: need", "plain body, no front matter\n");
    assert!(
        !raw.windows(b"X-Vivi-Format".len())
            .any(|w| w == b"X-Vivi-Format")
    );
    assert!(matches!(
        vivi::letter::of_message(&raw, "").unwrap_err(),
        vivi::letter::LetterError::Legacy { .. }
    ));

    let (status, created) = fx.append(&id, &raw, None, Some("need")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![id.clone()]);
    let served = STANDARD
        .decode(items["records"][0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, raw);

    // Replay of an already-accepted letter keeps the duplicate outcome.
    let (status, again) = fx.append(&id, &raw, None, Some("need")).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["message_id"], id);
    assert_eq!(
        ids(&read(&fx.app, &fx.project, &fx.token, 0).await),
        vec![id.clone()]
    );

    // Different bytes under the same id conflict, even when those bytes
    // would themselves be a format refusal. Replay runs first.
    let broken = raw_letter(
        FORMAT_ONE_NEED,
        "+++\ndone_when = [\"it works\"]\nbogus = 1\n+++\n\nnope\n",
    );
    let (status, conflict) = fx.append(&id, &broken, None, Some("need")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], "conflict");
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![id]);
    let served = STANDARD
        .decode(items["records"][0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, raw);
}

#[tokio::test]
async fn size_cap_and_input_validation() {
    let fx = fixture("validation", 51).await;
    let cap = 64 * 1024;

    // Exactly 64 KiB is accepted; one byte more is 413.
    let id = traveling(1);
    let mut at_cap = eml(&id, "");
    at_cap.resize(cap, b'x');
    let (status, ok) = fx.append(&id, &at_cap, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{ok}");
    let id = traveling(2);
    let mut over = eml(&id, "");
    over.resize(cap + 1, b'x');
    let (status, too_big) = fx.append(&id, &over, None, None).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{too_big}");
    assert_eq!(too_big["error"]["code"], "body_too_large");

    let letter = eml(&traveling(3), "x");
    let bad = |status: StatusCode, value: &Value| {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "invalid_request");
    };

    // Empty body.
    let (status, value) = fx.append(&traveling(3), b"", None, None).await;
    bad(status, &value);

    // Bad kind (header) and bad `to` (header; the full grammar is covered by
    // `recipient_grammar_is_enforced_at_append`).
    let (status, value) = fx.append(&traveling(3), &letter, None, Some("bogus")).await;
    bad(status, &value);
    for to in ["two words", "bob", &"a".repeat(65)] {
        let (status, value) = fx.append(&traveling(3), &letter, Some(to), None).await;
        bad(status, &value);
    }
    // A header value that is not ASCII.
    let non_ascii = Request::builder()
        .method("PUT")
        .uri(format!(
            "/v1/projects/{}/items/{}",
            fx.project,
            traveling(4)
        ))
        .header("authorization", format!("Bearer {}", fx.token))
        .header(
            "x-vivi-to",
            axum::http::HeaderValue::from_bytes(b"caf\xe9").unwrap(),
        )
        .body(Body::from(letter.clone()))
        .unwrap();
    let response = fx.app.clone().oneshot(non_ascii).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    bad(status, &serde_json::from_slice(&bytes).unwrap());

    // Bad path id.
    for id in ["plain-1", "msg_ABCDEF000000000000000000", "msg_0000"] {
        let (status, value) = fx.append(id, &letter, None, None).await;
        bad(status, &value);
    }

    // No token: 401.
    let (status, no_token) =
        put_with(&fx.app, &fx.project, None, &traveling(5), &letter, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{no_token}");

    // None of the rejects consumed a sequence or stored anything: the cap
    // letter is the only one stored.
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(items["records"].as_array().unwrap().len(), 1);
    let (status, fine) = fx
        .append(
            &traveling(6),
            &eml(&traveling(6), "y"),
            Some("Bob.s-1_x@Elsewhere.example"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{fine}");
    assert_eq!(fine["sequence"], 2);
}

#[tokio::test]
async fn kind_is_recorded_lowercase_and_mail_is_null() {
    let fx = fixture("kinds", 61).await;
    let cases = [
        (1, Some("TASK"), json!("task")),
        (2, Some("Need"), json!("need")),
        (3, Some("want"), json!("want")),
        (4, Some("mail"), Value::Null),
        (5, Some(""), Value::Null),
        (6, None, Value::Null),
    ];
    for (n, kind, _) in &cases {
        let id = traveling(*n);
        let (status, value) = fx.append(&id, &eml(&id, "k"), None, *kind).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    for (i, (_, _, expected)) in cases.iter().enumerate() {
        assert_eq!(&items["records"][i]["kind"], expected, "case {i}");
    }
}

#[tokio::test]
async fn visibility_scopes_mail_and_filtered_items_do_not_advance_the_cursor() {
    let fx = fixture("visibility", 71).await;
    let bob = fx.add_member(&traveling(90), "bob", &key(72)).await;
    let carol = fx.add_member(&traveling(91), "carol", &key(73)).await;
    // Sequences 1 and 2 are the two vouches (governance mail, member-visible).

    // 3: private mail creator -> bob. 4: work item. 5: postmaster mail
    // (member-visible). 6: private mail creator -> bob again.
    let letters: [(u64, Option<String>, Option<&str>); 4] = [
        (10, Some(address("bob")), None),
        (11, None, Some("need")),
        (12, Some("POSTMASTER".to_owned()), None),
        (13, Some(address("bob")), Some("mail")),
    ];
    for (n, to, kind) in letters {
        let id = traveling(n);
        let to = to.as_deref();
        let raw = if to == Some("POSTMASTER") {
            // A postmaster letter must carry a vouch; re-vouch an
            // already-admitted member (idempotent) to keep it valid.
            vouch_letter(&id, "bob", &key(72))
        } else {
            eml(&id, "private")
        };
        let (status, value) = fx.append(&id, &raw, to, kind).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }

    let creator_view = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&creator_view).len(), 6);

    let bob_view = read(&fx.app, &fx.project, &bob, 0).await;
    assert_eq!(ids(&bob_view).len(), 6, "bob is the recipient of both");
    assert_eq!(bob_view["next_cursor"], 6);

    // The bystander sees both vouches, the work item, and the postmaster
    // letter, but neither private mail; the cursor stops at 5.
    let carol_view = read(&fx.app, &fx.project, &carol, 0).await;
    assert_eq!(
        ids(&carol_view),
        vec![traveling(90), traveling(91), traveling(11), traveling(12)]
    );
    assert_eq!(carol_view["next_cursor"], 5);
    let sequences: Vec<u64> = carol_view["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(sequences, vec![1, 2, 4, 5]);

    // Resuming after the last returned record yields nothing and keeps the
    // cursor where it was.
    let tail = read(&fx.app, &fx.project, &carol, 5).await;
    assert!(tail["records"].as_array().unwrap().is_empty());
    assert_eq!(tail["next_cursor"], 5);
}

#[tokio::test]
async fn vouch_wire_admits_refuses_malformed_and_conflicts_without_storing() {
    let fx = fixture("vouch", 81).await;

    // Malformed: not JSON, missing key, no blank line, non-UTF-8 body.
    let mut non_utf8 = b"Subject: v\n\n".to_vec();
    non_utf8.extend_from_slice(&[0xff, 0xfe]);
    let malformed: Vec<Vec<u8>> = vec![
        eml(&traveling(1), "not a vouch"),
        eml(&traveling(1), r#"{"vouch":{"name":"builder"}}"#),
        b"Subject: v\n{\"vouch\":{}}".to_vec(),
        non_utf8,
    ];
    for raw in malformed {
        let (status, value) = fx
            .append(&traveling(1), &raw, Some("postmaster"), None)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    }

    // A valid vouch (CRLF letter) still lands at sequence 1: nothing above
    // was stored.
    let body = json!({"vouch": {"name": "builder", "public_key_pem": spki_pem(&key(82))}});
    let crlf = format!("Subject: vouch\r\n\r\n{body}\r\n").into_bytes();
    let (status, vouched) = fx
        .append(&traveling(2), &crlf, Some("postmaster"), Some("mail"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{vouched}");
    assert_eq!(vouched["sequence"], 1);
    let (status, identity) = register(&fx.app, &fx.project, &key(82), "builder").await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");

    // The same name for a different key conflicts; nothing is stored, so the
    // next letter is sequence 2.
    let other = vouch_letter(&traveling(3), "builder", &key(83));
    let (status, conflict) = fx
        .append(&traveling(3), &other, Some("postmaster"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    let (status, next) = fx
        .append(&traveling(4), &eml(&traveling(4), "next"), None, None)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{next}");
    assert_eq!(next["sequence"], 2);

    // The vouch letter is served exactly as sent.
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let served = STANDARD
        .decode(items["records"][0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, crlf);
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Where the project mailspace keeps a letter's bytes.
fn blob_path(fx: &Fixture, content_sha256: &str) -> std::path::PathBuf {
    fx.dir
        .join("projects")
        .join(&fx.project)
        .join(".vivi")
        .join("blobs")
        .join(&content_sha256[..2])
        .join(&content_sha256[2..4])
        .join(format!("{content_sha256}.eml"))
}

#[tokio::test]
async fn letters_are_held_verbatim_in_the_project_mailspace() {
    let fx = fixture("verbatim", 91).await;
    let id = traveling(1);
    let raw = eml(&id, "a body held in the clear");
    let (status, created) = fx
        .append(&id, &raw, Some(&address("builder")), Some("need"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    // Decided item 16: plain text on an encrypted volume, in a real mailspace.
    let blob = blob_path(&fx, created["content_sha256"].as_str().unwrap());
    assert_eq!(std::fs::read(&blob).unwrap(), raw, "byte for byte");
    let project_dir = fx.dir.join("projects").join(&fx.project);
    let mut files = Vec::new();
    walk_files(&project_dir, &mut files);
    assert!(
        files.iter().all(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            !name.ends_with(".sealed") && name != "items.json"
        }),
        "no sealed-blob store remains"
    );
}

/// Projects hold plain letters on the volume (decided item 16), so they do
/// not depend on the room storage key: a server booted without one still
/// serves them.
#[tokio::test]
async fn projects_work_without_a_store_encryption_key() {
    let dir = fresh_dir("no-key");
    let app = api_router(dir, None, None);
    let creator_key = key(93);
    let (status, project) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "creator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let project = project["project_id"].as_str().unwrap().to_owned();
    let (status, creator) = register(&app, &project, &creator_key, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{creator}");
    let token = creator["token"].as_str().unwrap().to_owned();
    let id = traveling(1);
    let raw = eml(&id, "no key needed");
    let (status, created) = append(&app, &project, &token, &id, &raw, None, Some("need")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let items = read(&app, &project, &token, 0).await;
    assert_eq!(items["records"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn restart_converges_over_the_same_data_dir() {
    let fx = fixture("restart", 101).await;
    let ids_and_bytes: Vec<(String, Vec<u8>)> = (1..=3u64)
        .map(|n| {
            let id = traveling(n);
            let mut raw = eml(&id, &format!("letter {n}"));
            raw.extend_from_slice(&[0xff, b'\r', b'\n', 0x00]);
            (id, raw)
        })
        .collect();
    for (id, raw) in &ids_and_bytes {
        let (status, value) = fx.append(id, raw, None, Some("need")).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }
    let before = read(&fx.app, &fx.project, &fx.token, 0).await;

    // A second router over the same data dir (a process restart): the
    // persisted bearer still opens, sequences and bytes are identical.
    let restarted = api_router(fx.dir.clone(), Some(store_key()), None);
    let after = read(&restarted, &fx.project, &fx.token, 0).await;
    assert_eq!(before, after);

    // Replays are duplicates with the original sequences; a fresh letter
    // continues the counter.
    for (n, (id, raw)) in ids_and_bytes.iter().enumerate() {
        let (status, value) = append(
            &restarted,
            &fx.project,
            &fx.token,
            id,
            raw,
            None,
            Some("need"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["duplicate"], true);
        assert_eq!(value["sequence"], n as u64 + 1);
    }
    let id = traveling(4);
    let (status, fresh) = append(
        &restarted,
        &fx.project,
        &fx.token,
        &id,
        &eml(&id, "after restart"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{fresh}");
    assert_eq!(fresh["sequence"], 4);
}

#[tokio::test]
async fn unreadable_blob_is_an_internal_error_not_a_skip() {
    let fx = fixture("missing-blob", 111).await;
    let id = traveling(1);
    let (status, created) = fx.append(&id, &eml(&id, "one"), None, Some("need")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let blob = blob_path(&fx, created["content_sha256"].as_str().unwrap());
    std::fs::remove_file(blob).unwrap();
    let (status, error) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/items?since=0", fx.project),
        None,
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{error}");
    assert_eq!(error["error"]["code"], "internal");
}

#[tokio::test]
async fn token_renewal_issues_a_new_token_and_keeps_the_old_one_valid() {
    let fx = fixture("renew", 121).await;
    let creator_key = key(121);
    let (status, renewed) =
        renew_with(&fx.app, &fx.project, "creator", &creator_key, &creator_key).await;
    assert_eq!(status, StatusCode::OK, "{renewed}");
    let new_token = renewed["token"].as_str().unwrap();
    assert_ne!(new_token, fx.token);
    assert!(renewed["token_expires_at"].as_str().unwrap().ends_with('Z'));

    // Both tokens authenticate an items read; the new one is the creator.
    for token in [new_token, fx.token.as_str()] {
        let items = read(&fx.app, &fx.project, token, 0).await;
        assert!(items["records"].as_array().unwrap().is_empty());
    }
    let id = traveling(1);
    let (status, created) = append(
        &fx.app,
        &fx.project,
        new_token,
        &id,
        &eml(&id, "via renewed token"),
        None,
        Some("need"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let items = read(&fx.app, &fx.project, new_token, 0).await;
    assert_eq!(items["records"][0]["from"], "creator");
}

#[tokio::test]
async fn token_renewal_fails_closed_with_one_generic_401() {
    let fx = fixture("renew-fail", 131).await;
    let creator_key = key(131);
    let builder_key = key(132);
    fx.add_member(&traveling(90), "builder", &builder_key).await;
    let generic = |status: StatusCode, value: &Value| {
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        assert_eq!(value["error"]["code"], "unauthorized");
        value["error"]["message"].clone()
    };

    // Wrong signing key for the challenge it fetched.
    let (s, wrong_sig) =
        renew_with(&fx.app, &fx.project, "creator", &creator_key, &builder_key).await;
    let m1 = generic(s, &wrong_sig);
    // Challenge issued for another identity's key, presented for `creator`.
    let (s, other_key) =
        renew_with(&fx.app, &fx.project, "creator", &builder_key, &builder_key).await;
    let m2 = generic(s, &other_key);
    // Unknown name.
    let (s, unknown_name) =
        renew_with(&fx.app, &fx.project, "nobody", &creator_key, &creator_key).await;
    let m3 = generic(s, &unknown_name);
    // Unknown challenge id.
    let (s, unknown_challenge) = send(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/auth/token", fx.project),
        Some(json!({"name": "creator", "challenge_id": "nope", "signature_base64": "AAAA"})),
        None,
    )
    .await;
    let m4 = generic(s, &unknown_challenge);
    assert!([&m2, &m3, &m4].iter().all(|m| **m == m1), "one message");

    // Replay: a challenge is spent on first use, even a successful one.
    let (challenge_id, bytes) = challenge(&fx.app, &fx.project, &creator_key).await;
    let signature = STANDARD.encode(creator_key.sign(&bytes).to_bytes());
    let body =
        json!({"name": "creator", "challenge_id": challenge_id, "signature_base64": signature});
    let uri = format!("/v1/projects/{}/auth/token", fx.project);
    let (status, first) = send(&fx.app, "POST", &uri, Some(body.clone()), None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (s, replay) = send(&fx.app, "POST", &uri, Some(body), None).await;
    generic(s, &replay);

    // A failed attempt also spends its challenge.
    let (challenge_id, bytes) = challenge(&fx.app, &fx.project, &creator_key).await;
    let bad = json!({"name": "builder", "challenge_id": challenge_id,
        "signature_base64": STANDARD.encode(creator_key.sign(&bytes).to_bytes())});
    let (s, value) = send(&fx.app, "POST", &uri, Some(bad), None).await;
    generic(s, &value);
    let good = json!({"name": "creator", "challenge_id": challenge_id,
        "signature_base64": STANDARD.encode(creator_key.sign(&bytes).to_bytes())});
    let (s, value) = send(&fx.app, "POST", &uri, Some(good), None).await;
    generic(s, &value);
}

#[tokio::test]
async fn token_renewal_is_keyed_to_the_project() {
    let fx = fixture("renew-project", 141).await;
    let creator_key = key(141);

    // A second project where the same key is also the creator.
    let (status, other) = send(
        &fx.app,
        "POST",
        "/v1/projects",
        Some(json!({"public_key_pem": spki_pem(&creator_key), "creator_name": "creator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other = other["project_id"].as_str().unwrap();
    let (status, value) = register(&fx.app, other, &creator_key, "creator").await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    // A challenge issued for project B is refused at project A.
    let (challenge_id, bytes) = challenge(&fx.app, other, &creator_key).await;
    let signature = STANDARD.encode(creator_key.sign(&bytes).to_bytes());
    let (status, value) = send(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/auth/token", fx.project),
        Some(
            json!({"name": "creator", "challenge_id": challenge_id, "signature_base64": signature}),
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");

    // Unknown project: 404.
    let unknown = format!(
        "{}{}",
        if fx.project.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &fx.project[1..]
    );
    let (status, value) = send(
        &fx.app,
        "POST",
        &format!("/v1/projects/{unknown}/auth/token"),
        Some(json!({"name": "creator", "challenge_id": "x", "signature_base64": "AAAA"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
    assert_eq!(value["error"]["code"], "project_not_found");
}

#[tokio::test]
async fn raw_body_round_trips_byte_identically_through_put_then_get() {
    let fx = fixture("raw-body", 151).await;
    // Non-UTF-8, NUL, CRLF, a lone CR, and no trailing newline; also a
    // body that looks like JSON and a base64-ish run, which must stay raw.
    let mut raw = b"From: a@p.local\r\nSubject: raw\r\n\r\n".to_vec();
    raw.extend_from_slice(&[0xff, 0xfe, 0x00, 0x80, b'\r', b'{', b'"', b'=', b'=']);
    raw.extend_from_slice(b"\r\nlast line without newline");
    let id = traveling(1);
    let (status, created) = fx.append(&id, &raw, None, Some("task")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["content_sha256"], sha256_hex(&raw));

    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let served = STANDARD
        .decode(items["records"][0]["raw_eml"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, raw);
}

#[tokio::test]
async fn the_content_type_is_ignored() {
    let fx = fixture("content-type", 152).await;
    for (n, content_type) in ["application/json", "text/plain", "application/x-weird"]
        .into_iter()
        .enumerate()
    {
        let id = traveling(n as u64 + 1);
        let raw = eml(&id, "typed oddly");
        let (status, value) = raw_request(
            &fx.app,
            "PUT",
            &format!("/v1/projects/{}/items/{id}", fx.project),
            raw.clone(),
            Some(&fx.token),
            vec![("content-type".to_owned(), content_type.to_owned())],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{content_type}: {value}");
        assert_eq!(value["content_sha256"], sha256_hex(&raw));
    }
}

#[tokio::test]
async fn the_old_post_append_route_is_gone() {
    let fx = fixture("old-route", 153).await;
    let id = traveling(1);
    let raw = eml(&id, "old wire");
    let (status, _) = raw_request(
        &fx.app,
        "POST",
        &format!("/v1/projects/{}/items", fx.project),
        json!({"raw_eml": STANDARD.encode(&raw)})
            .to_string()
            .into_bytes(),
        Some(&fx.token),
        vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("message-id".to_owned(), id.clone()),
        ],
    )
    .await;
    assert!(!status.is_success(), "POST append must not exist: {status}");
    assert!(
        status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
        "{status}"
    );
    // Nothing was stored, and the collection path still reads.
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert!(items["records"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_path_id_wins_over_a_message_id_header() {
    let fx = fixture("path-id", 154).await;
    let path_id = traveling(1);
    let header_id = traveling(2);
    let raw = eml(&path_id, "path id");
    let (status, created) = put_with(
        &fx.app,
        &fx.project,
        Some(&fx.token),
        &path_id,
        &raw,
        &[("message-id", &header_id)],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["message_id"], path_id);
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![path_id]);

    // A valid Message-ID header does not rescue a bad path id.
    let (status, value) = put_with(
        &fx.app,
        &fx.project,
        Some(&fx.token),
        "not-an-id",
        &raw,
        &[("message-id", &header_id)],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"]["code"], "invalid_request");
}

/// The extractor limit sits just above the cap, so both the cap's own 413
/// and an extractor-level 413 (far over the limit) use our JSON shape.
#[tokio::test]
async fn oversize_bodies_get_the_json_413_shape() {
    let fx = fixture("cap-413", 155).await;
    let cap = 64 * 1024;
    for (n, len, expected) in [
        (1u64, cap, StatusCode::CREATED),
        (2, cap + 1, StatusCode::PAYLOAD_TOO_LARGE),
        // Past the route's body limit, rejected before the handler's check.
        (3, 200 * 1024, StatusCode::PAYLOAD_TOO_LARGE),
        (4, 3 * 1024 * 1024, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let id = traveling(n);
        let mut raw = eml(&id, "");
        raw.resize(len, b'x');
        let (status, value) = fx.append(&id, &raw, None, None).await;
        assert_eq!(status, expected, "len {len}: {value}");
        if expected == StatusCode::PAYLOAD_TOO_LARGE {
            assert_eq!(value["error"]["code"], "body_too_large", "len {len}");
        }
    }
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&items), vec![traveling(1)]);
}

/// `GET /items/{id}` with the raw response: status, headers, body bytes.
async fn get_letter(
    app: &Router,
    project: &str,
    token: Option<&str>,
    id: &str,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method("GET")
        .uri(format!("/v1/projects/{project}/items/{id}"));
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes.to_vec())
}

fn header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().unwrap())
}

#[tokio::test]
async fn single_letter_get_returns_raw_bytes_with_envelope_headers() {
    let fx = fixture("get-one", 161).await;
    // Non-UTF-8, NUL, CRLF, no trailing newline.
    let mut raw = b"From: a@p.local\r\nSubject: raw\r\n\r\n".to_vec();
    raw.extend_from_slice(&[0xff, 0xfe, 0x00, 0x80, b'\r', b'\n']);
    raw.extend_from_slice(b"no trailing newline");

    // First letter, no envelope fields: To and Kind headers are omitted.
    let plain_id = traveling(1);
    let (status, value) = fx.append(&plain_id, &raw, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (status, headers, body) =
        get_letter(&fx.app, &fx.project, Some(&fx.token), &plain_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, raw, "byte-identical to what was PUT");
    assert_eq!(header(&headers, "content-type"), Some("message/rfc822"));
    assert_eq!(
        header(&headers, "x-vivi-message-id"),
        Some(plain_id.as_str())
    );
    assert_eq!(header(&headers, "x-vivi-from"), Some("creator"));
    assert_eq!(header(&headers, "x-vivi-sequence"), Some("1"));
    assert_eq!(
        header(&headers, "x-vivi-content-sha256"),
        Some(sha256_hex(&body).as_str())
    );
    assert_eq!(header(&headers, "cache-control"), Some("no-store"));
    assert!(headers.get("x-vivi-to").is_none());
    assert!(headers.get("x-vivi-kind").is_none());

    // Second letter with both fields.
    let id = traveling(2);
    let letter = eml(&id, "kinded");
    let to = address("builder");
    let (status, value) = fx.append(&id, &letter, Some(&to), Some("NEED")).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (status, headers, body) = get_letter(&fx.app, &fx.project, Some(&fx.token), &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, letter);
    assert_eq!(header(&headers, "x-vivi-to"), Some(to.as_str()));
    assert_eq!(header(&headers, "x-vivi-kind"), Some("need"));
    assert_eq!(header(&headers, "x-vivi-sequence"), Some("2"));
}

#[tokio::test]
async fn single_letter_get_applies_list_visibility() {
    let fx = fixture("get-visibility", 171).await;
    let bob = fx.add_member(&traveling(90), "bob", &key(172)).await;
    let carol = fx.add_member(&traveling(91), "carol", &key(173)).await;

    let work = traveling(10);
    let mail = traveling(11);
    let gov = traveling(12);
    assert_eq!(
        fx.append(&work, &eml(&work, "w"), None, Some("task"))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.append(&mail, &eml(&mail, "m"), Some(&address("bob")), None)
            .await
            .0,
        StatusCode::CREATED
    );
    // A governance letter must carry a vouch; re-vouching bob is idempotent.
    let gov_raw = vouch_letter(&gov, "bob", &key(172));
    assert_eq!(
        fx.append(&gov, &gov_raw, Some("postmaster"), None).await.0,
        StatusCode::CREATED
    );

    // Work items and postmaster mail: every member.
    for token in [&fx.token, &bob, &carol] {
        let (status, _, _) = get_letter(&fx.app, &fx.project, Some(token), &work).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, body) = get_letter(&fx.app, &fx.project, Some(token), &gov).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, gov_raw);
    }
    // Mail: sender and recipient only.
    for token in [&fx.token, &bob] {
        let (status, _, _) = get_letter(&fx.app, &fx.project, Some(token), &mail).await;
        assert_eq!(status, StatusCode::OK);
    }

    // A not-visible letter and an unknown id are byte-identical 404s.
    let (hidden_status, hidden_headers, hidden_body) =
        get_letter(&fx.app, &fx.project, Some(&carol), &mail).await;
    let (unknown_status, unknown_headers, unknown_body) =
        get_letter(&fx.app, &fx.project, Some(&carol), &traveling(999)).await;
    assert_eq!(hidden_status, StatusCode::NOT_FOUND);
    assert_eq!(unknown_status, StatusCode::NOT_FOUND);
    assert_eq!(hidden_body, unknown_body);
    assert_eq!(hidden_headers, unknown_headers);
    let value: Value = serde_json::from_slice(&hidden_body).unwrap();
    assert_eq!(value["error"]["code"], "item_not_found");
    assert_eq!(value["error"]["message"], "no such letter");
}

#[tokio::test]
async fn single_letter_get_error_cases() {
    let fx = fixture("get-errors", 181).await;
    let id = traveling(1);
    assert_eq!(
        fx.append(&id, &eml(&id, "x"), None, Some("need")).await.0,
        StatusCode::CREATED
    );
    let json_of = |body: &[u8]| serde_json::from_slice::<Value>(body).unwrap();

    // Malformed id: 400.
    let (status, _, body) = get_letter(&fx.app, &fx.project, Some(&fx.token), "plain-1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json_of(&body)["error"]["code"], "invalid_request");

    // Missing and invalid bearer: 401.
    let (status, _, body) = get_letter(&fx.app, &fx.project, None, &id).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(&body)["error"]["code"], "unauthorized");
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some("not-a-token"), &id).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Unknown project: 404 project_not_found.
    let unknown = format!(
        "{}{}",
        if fx.project.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &fx.project[1..]
    );
    let (status, _, body) = get_letter(&fx.app, &unknown, Some(&fx.token), &id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&body)["error"]["code"], "project_not_found");

    // A token from another project does not open this one.
    let other = fixture("get-errors-other", 182).await;
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&other.token), &id).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An indexed letter whose blob is gone or does not match its hash is a 500,
/// never a 404 (a 404 would say "no such letter" about a letter we hold).
#[tokio::test]
async fn single_letter_get_with_an_unreadable_blob_is_500_not_404() {
    let fx = fixture("get-missing-blob", 191).await;
    let first = traveling(1);
    let second = traveling(2);
    let (_, created) = fx
        .append(&first, &eml(&first, "one"), None, Some("need"))
        .await;
    let (_, other) = fx
        .append(&second, &eml(&second, "two"), None, Some("need"))
        .await;
    let blob = |value: &Value| blob_path(&fx, value["content_sha256"].as_str().unwrap());

    // Swapped blob: reads fine, hash mismatch.
    let target = blob(&created);
    let mut perms = std::fs::metadata(&target).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&target, perms).unwrap();
    std::fs::copy(blob(&other), &target).unwrap();
    let (status, _, body) = get_letter(&fx.app, &fx.project, Some(&fx.token), &first).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
        "internal"
    );

    // Missing blob.
    std::fs::remove_file(blob(&created)).unwrap();
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&fx.token), &first).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    // The other letter is unaffected.
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&fx.token), &second).await;
    assert_eq!(status, StatusCode::OK);
}

/// Vouch `name` for `signing_key` through the creator, without registering.
async fn vouch_only(fx: &Fixture, vouch_id: &str, name: &str, signing_key: &SigningKey) {
    let raw = vouch_letter(vouch_id, name, signing_key);
    let (status, vouched) = fx.append(vouch_id, &raw, Some("postmaster"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{vouched}");
}

#[tokio::test]
async fn registration_requires_a_canonical_mailspace_id() {
    let fx = fixture("ms-id-shape", 201).await;
    vouch_only(&fx, &traveling(90), "builder", &key(202)).await;

    let good = mailspace_for("builder");
    let bad: Vec<Value> = vec![
        Value::Null, // omitted
        json!(""),
        json!(good.to_uppercase()),
        json!(good.replace('-', "")),
        json!(&good[..35]),
        json!(format!("{good}0")),
        json!(good.replace('a', "g")),
        json!(good.replacen('-', "_", 1)),
        json!(7),
    ];
    for mailspace_id in bad {
        let (status, value) = register_with(
            &fx.app,
            &fx.project,
            &key(202),
            "builder",
            mailspace_id.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{mailspace_id}: {value}");
        assert_eq!(value["error"]["code"], "invalid_request");
    }

    // Nothing was registered by the refusals, and the admission still works.
    let (status, identity) = register(&fx.app, &fx.project, &key(202), "builder").await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");
    assert_eq!(identity["mailspace_id"], good);
}

#[tokio::test]
async fn prefix_collision_is_refused_and_the_admission_stays_usable() {
    let fx = fixture("ms-prefix", 211).await;
    vouch_only(&fx, &traveling(90), "builder", &key(212)).await;
    let creator_id = mailspace_for("creator");

    // The same id, and a different id sharing only the 8-char prefix.
    let same_prefix = format!("{}-1111-2222-3333-444444444444", &creator_id[..8]);
    for mailspace_id in [creator_id.clone(), same_prefix] {
        let (status, value) = register_with(
            &fx.app,
            &fx.project,
            &key(212),
            "builder",
            json!(mailspace_id),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{value}");
        assert_eq!(value["error"]["code"], "mailspace_prefix_taken");
    }

    // The admission is intact: the same key retries with a different id.
    let fresh = mailspace_for("builder");
    let (status, identity) =
        register_with(&fx.app, &fx.project, &key(212), "builder", json!(fresh)).await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");
    assert_eq!(identity["mailspace_id"], fresh);

    // An already registered name stays `conflict`, and it is checked before
    // the prefix: here the id's prefix is also taken.
    for mailspace_id in [mailspace_for("elsewhere"), creator_id] {
        let (status, value) = register_with(
            &fx.app,
            &fx.project,
            &key(212),
            "builder",
            json!(mailspace_id),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{value}");
        assert_eq!(value["error"]["code"], "conflict");
    }

    // The directory carries each member's id.
    let (status, directory) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/directory", fx.project),
        None,
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    assert_eq!(directory["members"][0]["name"], "builder");
    assert_eq!(directory["members"][0]["mailspace_id"], fresh);
    assert_eq!(directory["members"][1]["name"], "creator");
    assert_eq!(
        directory["members"][1]["mailspace_id"],
        mailspace_for("creator")
    );
}

#[tokio::test]
async fn legacy_identity_has_a_null_id_and_no_prefix() {
    let fx = fixture("ms-legacy", 221).await;
    let bob = fx.add_member(&traveling(90), "bob", &key(222)).await;

    // Rewrite the creator's record as an identities file from before
    // addresses: no `mailspace_id` key at all.
    let path = fx
        .dir
        .join("projects")
        .join(&fx.project)
        .join("identities.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for identity in file["identities"].as_array_mut().unwrap() {
        if identity["name"] == "creator" {
            assert!(
                identity
                    .as_object_mut()
                    .unwrap()
                    .remove("mailspace_id")
                    .is_some()
            );
        }
    }
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

    // The file still loads, the old bearer still works, and the directory
    // shows null for the legacy member only.
    let (status, directory) = send(
        &fx.app,
        "GET",
        &format!("/v1/projects/{}/directory", fx.project),
        None,
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    assert_eq!(directory["members"][0]["name"], "bob");
    assert_eq!(
        directory["members"][0]["mailspace_id"],
        mailspace_for("bob")
    );
    assert_eq!(directory["members"][1]["name"], "creator");
    assert_eq!(directory["members"][1]["mailspace_id"], Value::Null);

    // Mail to the legacy member's old address cannot reach it by prefix; it
    // still sees what it sent.
    let id = traveling(1);
    let (status, value) = append(
        &fx.app,
        &fx.project,
        &bob,
        &id,
        &eml(&id, "hello"),
        Some(&address("creator")),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&fx.token), &id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&bob), &id).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn recipient_grammar_is_enforced_at_append() {
    let fx = fixture("recipient-grammar", 231).await;
    let letter = eml(&traveling(1), "g");
    let bad = |status: StatusCode, value: &Value, what: &str| {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {value}");
        assert_eq!(value["error"]["code"], "invalid_request", "{what}");
    };

    // 16 addresses is the cap.
    let sixteen: Vec<String> = (0..16).map(|n| format!("r{n}@3f9a1c7e.local")).collect();
    let seventeen: Vec<String> = (0..17).map(|n| format!("r{n}@3f9a1c7e.local")).collect();
    let local65 = format!("{}@x.local", "a".repeat(65));
    let domain65 = format!("a@{}", "b".repeat(65));
    // A list of 8 addresses totalling exactly 1024 bytes, and one over.
    let long_address = format!("{}@{}", "a".repeat(64), "b".repeat(64));
    let max_list = |last_local: usize| {
        let mut parts = vec![long_address.clone(); 7];
        parts.push(format!("{}@{}", "c".repeat(last_local), "d".repeat(64)));
        parts.join(",")
    };
    assert_eq!(max_list(49).len(), 1024);
    assert_eq!(max_list(50).len(), 1025);

    let bad_values = [
        ("bare name", "bob".to_owned()),
        ("bare name with dots", "Bob.s-1_x".to_owned()),
        ("whitespace in local", "two words@x.local".to_owned()),
        ("slash in local", "a/b@x.local".to_owned()),
        ("empty local", "@x.local".to_owned()),
        ("empty domain", "a@".to_owned()),
        ("two at signs", "a@b@c.local".to_owned()),
        ("underscore in domain", "a@b_c.local".to_owned()),
        ("local over 64", local65),
        ("domain over 64", domain65),
        ("empty entry", "a@x.local,,b@x.local".to_owned()),
        ("trailing comma", "a@x.local,".to_owned()),
        ("a bare name in a list", "a@x.local, bob".to_owned()),
        ("postmaster in a list", "postmaster, a@x.local".to_owned()),
        (
            "postmaster after a list",
            "a@x.local, postmaster".to_owned(),
        ),
        ("17 addresses", seventeen.join(",")),
        ("over 1 KiB", max_list(50)),
    ];
    for (what, to) in &bad_values {
        let (status, value) = fx.append(&traveling(1), &letter, Some(to), None).await;
        bad(status, &value, what);
    }

    // Nothing above was stored.
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert!(items["records"].as_array().unwrap().is_empty());

    // Valid forms: stored verbatim, in the same order, entries untouched.
    let good_values = [
        "a@x.local".to_owned(),
        "Mind@3F9A1C7E.LOCAL".to_owned(),
        "a@x.local, b@y.example".to_owned(),
        sixteen.join(","),
        max_list(49),
        // A postmaster-looking local part is an ordinary address.
        "postmaster@x.local".to_owned(),
    ];
    for (n, to) in good_values.iter().enumerate() {
        let id = traveling(10 + n as u64);
        let (status, value) = fx.append(&id, &eml(&id, "ok"), Some(to), None).await;
        assert_eq!(status, StatusCode::CREATED, "{to}: {value}");
        let (status, headers, _) = get_letter(&fx.app, &fx.project, Some(&fx.token), &id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header(&headers, "x-vivi-to"), Some(to.as_str()));
    }

    // A blank header is no recipient at all.
    let id = traveling(30);
    let (status, value) = fx.append(&id, &eml(&id, "blank"), Some(""), None).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (_, headers, _) = get_letter(&fx.app, &fx.project, Some(&fx.token), &id).await;
    assert!(headers.get("x-vivi-to").is_none());

    // `postmaster` is still governance, in any case.
    let id = traveling(31);
    let vouch = vouch_letter(&id, "builder", &key(232));
    let (status, value) = fx.append(&id, &vouch, Some("PostMaster"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (status, identity) = register(&fx.app, &fx.project, &key(232), "builder").await;
    assert_eq!(status, StatusCode::CREATED, "{identity}");
}

#[tokio::test]
async fn mail_is_routed_by_the_recipient_prefix() {
    let fx = fixture("prefix-routing", 241).await;
    let bob = fx.add_member(&traveling(90), "bob", &key(242)).await;
    let carol = fx.add_member(&traveling(91), "carol", &key(243)).await;
    // Sequences 1 and 2 are the vouches (governance mail, member-visible).
    let bob_prefix = prefix_for("bob");
    let carol_prefix = prefix_for("carol");

    let mail = traveling(10);
    let work = traveling(11);
    let legacy = traveling(12);
    let gov = traveling(13);
    let unregistered = traveling(14);
    let both = traveling(15);
    let upper = traveling(16);
    let not_local = traveling(17);

    // 3: mail to a role of bob's mailspace; the local part is not his name.
    let to = format!("mind@{bob_prefix}.local");
    assert_eq!(
        fx.append(&mail, &eml(&mail, "m"), Some(&to), None).await.0,
        StatusCode::CREATED
    );
    // 4: a work item, member-visible.
    assert_eq!(
        fx.append(&work, &eml(&work, "w"), None, Some("need"))
            .await
            .0,
        StatusCode::CREATED
    );
    // 5: mail to a legacy (named, non-prefix) domain: sender only.
    assert_eq!(
        fx.append(
            &legacy,
            &eml(&legacy, "l"),
            Some("mind@faberlang.local"),
            None
        )
        .await
        .0,
        StatusCode::CREATED
    );
    // 6: governance mail: every member.
    let gov_raw = vouch_letter(&gov, "bob", &key(242));
    assert_eq!(
        fx.append(&gov, &gov_raw, Some("postmaster"), None).await.0,
        StatusCode::CREATED
    );
    // 7: a prefix that nobody registered is accepted; sender only.
    assert_eq!(
        fx.append(
            &unregistered,
            &eml(&unregistered, "u"),
            Some("mind@deadbeef.local"),
            None
        )
        .await
        .0,
        StatusCode::CREATED
    );
    // 8: two recipients in two mailspaces: both see it, the sender too.
    let to_both = format!("a@{bob_prefix}.local, b@{carol_prefix}.local");
    assert_eq!(
        fx.append(&both, &eml(&both, "b"), Some(&to_both), None)
            .await
            .0,
        StatusCode::CREATED
    );
    // 9: the domain compare is case-insensitive.
    let to_upper = format!("Mind@{}.LOCAL", bob_prefix.to_uppercase());
    assert_eq!(
        fx.append(&upper, &eml(&upper, "c"), Some(&to_upper), None)
            .await
            .0,
        StatusCode::CREATED
    );
    // 10: only `<prefix>.local` names a mailspace.
    let to_not_local = format!("mind@{bob_prefix}.example");
    assert_eq!(
        fx.append(&not_local, &eml(&not_local, "n"), Some(&to_not_local), None)
            .await
            .0,
        StatusCode::CREATED
    );

    // The sender sees everything it sent plus all shared items.
    let creator_view = read(&fx.app, &fx.project, &fx.token, 0).await;
    assert_eq!(ids(&creator_view).len(), 10);

    // Bob: vouches, his mail, the work item, governance, both, upper.
    let bob_view = read(&fx.app, &fx.project, &bob, 0).await;
    assert_eq!(
        ids(&bob_view),
        vec![
            traveling(90),
            traveling(91),
            mail.clone(),
            work.clone(),
            gov.clone(),
            both.clone(),
            upper.clone(),
        ]
    );
    assert_eq!(bob_view["next_cursor"], 9);

    // Carol: only shared items and the two-recipient mail; the filtered
    // letters (seq 3, 5, 7, 9, 10) never advance the cursor, so it stops at
    // the last letter she can see.
    let carol_view = read(&fx.app, &fx.project, &carol, 0).await;
    assert_eq!(
        ids(&carol_view),
        vec![
            traveling(90),
            traveling(91),
            work.clone(),
            gov.clone(),
            both.clone()
        ]
    );
    assert_eq!(carol_view["next_cursor"], 8);
    let tail = read(&fx.app, &fx.project, &carol, 8).await;
    assert!(tail["records"].as_array().unwrap().is_empty());
    assert_eq!(tail["next_cursor"], 8, "hidden letters hold the cursor");

    // Single GET matches the list: bob reads his mail, carol gets the same
    // 404 she gets for an unknown id.
    let (status, _, body) = get_letter(&fx.app, &fx.project, Some(&bob), &mail).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, eml(&mail, "m"));
    let (hidden_status, hidden_headers, hidden_body) =
        get_letter(&fx.app, &fx.project, Some(&carol), &mail).await;
    let (unknown_status, unknown_headers, unknown_body) =
        get_letter(&fx.app, &fx.project, Some(&carol), &traveling(999)).await;
    assert_eq!(hidden_status, StatusCode::NOT_FOUND);
    assert_eq!(unknown_status, StatusCode::NOT_FOUND);
    assert_eq!(hidden_body, unknown_body);
    assert_eq!(hidden_headers, unknown_headers);
    for private in [&legacy, &unregistered, &upper, &not_local] {
        let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&carol), private).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{private}");
    }
    // Neither the legacy-domain letter nor the wrong-TLD one reaches bob.
    for private in [&legacy, &unregistered, &not_local] {
        let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&bob), private).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{private}");
    }
    // The shared items are visible to carol.
    for shared in [&work, &gov, &both] {
        let (status, _, _) = get_letter(&fx.app, &fx.project, Some(&carol), shared).await;
        assert_eq!(status, StatusCode::OK, "{shared}");
    }
}

#[tokio::test]
async fn letters_are_stamped_with_the_authenticated_senders_prefix() {
    let fx = fixture("from-prefix", 250).await;
    let bob = fx.add_member(&traveling(90), "bob", &key(249)).await;
    let creator_prefix = prefix_for("creator");

    // The letter's own From header claims another mailspace; the stamp is
    // the authenticated sender's regardless.
    let id = traveling(1);
    let forged = format!(
        "From: mind@{}.local\nTo: x@p.local\nMessage-ID: <{id}@p.local>\n\nforged",
        prefix_for("bob")
    )
    .into_bytes();
    let (status, value) = fx.append(&id, &forged, None, Some("need")).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    // A second member's letter carries that member's prefix.
    let bob_id = traveling(2);
    let (status, value) = append(
        &fx.app,
        &fx.project,
        &bob,
        &bob_id,
        &eml(&bob_id, "from bob"),
        None,
        Some("need"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let by_id = |id: &str| {
        items["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["message_id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id(&id)["from_prefix"], creator_prefix);
    assert_eq!(by_id(&bob_id)["from_prefix"], prefix_for("bob"));
    let (status, headers, body) = get_letter(&fx.app, &fx.project, Some(&bob), &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, forged, "the bytes are served untouched");
    assert_eq!(
        header(&headers, "x-vivi-from-prefix"),
        Some(creator_prefix.as_str())
    );

    // A legacy identity (no mailspace id) stamps nothing, and a letter
    // already stamped keeps its stamp: it is fixed at append.
    let path = fx
        .dir
        .join("projects")
        .join(&fx.project)
        .join("identities.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for identity in file["identities"].as_array_mut().unwrap() {
        if identity["name"] == "creator" {
            identity.as_object_mut().unwrap().remove("mailspace_id");
        }
    }
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

    let legacy_id = traveling(3);
    let (status, value) = fx
        .append(&legacy_id, &eml(&legacy_id, "legacy"), None, Some("need"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let (status, headers, _) = get_letter(&fx.app, &fx.project, Some(&bob), &legacy_id).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("x-vivi-from-prefix").is_none());
    let items = read(&fx.app, &fx.project, &fx.token, 0).await;
    let records = items["records"].as_array().unwrap();
    let find = |id: &str| records.iter().find(|r| r["message_id"] == id).unwrap();
    assert_eq!(find(&legacy_id)["from_prefix"], Value::Null);
    assert_eq!(find(&id)["from_prefix"], creator_prefix, "never recomputed");
    // A replay returns the original row and stamps nothing new.
    let (status, replay) = fx.append(&id, &forged, None, Some("need")).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["duplicate"], true);
    let (_, headers, _) = get_letter(&fx.app, &fx.project, Some(&bob), &id).await;
    assert_eq!(
        header(&headers, "x-vivi-from-prefix"),
        Some(creator_prefix.as_str())
    );
}

// ---- append time, paged and filtered list, lookup by handle ----

/// `GET` a path with an optional `Accept-Encoding`; the raw response.
async fn get_raw(
    app: &Router,
    uri: &str,
    token: &str,
    accept_encoding: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    if let Some(encoding) = accept_encoding {
        builder = builder.header("accept-encoding", encoding);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes.to_vec())
}

/// A list with extra query parameters (`"&limit=2&kind=need"`), as JSON.
async fn list(fx_app: &Router, project: &str, token: &str, query: &str) -> (StatusCode, Value) {
    send(
        fx_app,
        "GET",
        &format!("/v1/projects/{project}/items?since=0{query}"),
        None,
        Some(token),
    )
    .await
}

async fn lookup(
    app: &Router,
    project: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        &format!("/v1/projects/{project}/items/lookup"),
        Some(body),
        token,
    )
    .await
}

fn is_rfc3339_secs(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok() && value.len() == 20 && value.ends_with('Z')
}

/// A project with creator and bob, and eight letters from the creator:
/// seq 1 vouch (postmaster), 2 need, 3 task, 4 need, 5 mail to bob,
/// 6 mail to creator itself, 7 mail to an unregistered mailspace, 8 want.
/// Returns the fixture, bob's token, and the ids by sequence (index 0 = seq 1).
async fn windows_fixture(name: &str, seed: u8) -> (Fixture, String, Vec<String>) {
    let fx = fixture(name, seed).await;
    let vouch = traveling(1);
    let bob = fx.add_member(&vouch, "bob", &key(seed + 1)).await;
    let mut ids = vec![vouch];
    let plan: [(Option<String>, Option<&str>); 7] = [
        (None, Some("need")),
        (None, Some("task")),
        (None, Some("need")),
        (Some(address("bob")), None),
        (Some(address("creator")), None),
        (Some("x@cccccccc.local".to_owned()), None),
        (None, Some("want")),
    ];
    for (n, (to, kind)) in plan.iter().enumerate() {
        let id = traveling(n as u64 + 2);
        let (status, value) = fx
            .append(&id, &eml(&id, &format!("letter {n}")), to.as_deref(), *kind)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
        ids.push(id);
    }
    (fx, bob, ids)
}

fn record_ids(page: &Value) -> Vec<String> {
    ids(page)
}

#[tokio::test]
async fn list_filters_pages_and_reports_has_more() {
    let (fx, bob, ids) = windows_fixture("windows-list", 100).await;
    let at = |ns: &[usize]| ns.iter().map(|n| ids[*n].clone()).collect::<Vec<_>>();
    // Bob sees seq 1 (vouch), 2, 3, 4, 5, 8; not 6 or 7.
    let (status, all) = list(&fx.app, &fx.project, &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(record_ids(&all), at(&[0, 1, 2, 3, 4, 7]));
    assert_eq!(all["next_cursor"], 8);
    assert_eq!(all["has_more"], false);
    for record in all["records"].as_array().unwrap() {
        assert!(is_rfc3339_secs(record["appended_at"].as_str().unwrap()));
    }

    // Pages of two follow the visible rows, never short for hidden ones.
    let mut cursor = 0;
    let mut pages = Vec::new();
    loop {
        let (status, page) = send(
            &fx.app,
            "GET",
            &format!("/v1/projects/{}/items?since={cursor}&limit=2", fx.project),
            None,
            Some(&bob),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let more = page["has_more"].as_bool().unwrap();
        if more {
            assert_eq!(record_ids(&page).len(), 2);
        }
        cursor = page["next_cursor"].as_u64().unwrap();
        pages.push((record_ids(&page), cursor, more));
        if !more {
            break;
        }
    }
    assert_eq!(
        pages,
        vec![
            (at(&[0, 1]), 2, true),
            (at(&[2, 3]), 4, true),
            (at(&[4, 7]), 8, false),
        ]
    );

    // Each filter alone, then combined.
    let kind = list(&fx.app, &fx.project, &bob, "&kind=need").await.1;
    assert_eq!(record_ids(&kind), at(&[1, 3]));
    let kind = list(&fx.app, &fx.project, &bob, "&kind=mail").await.1;
    assert_eq!(record_ids(&kind), at(&[0, 4]));
    let kind = list(&fx.app, &fx.project, &bob, "&kind=task,want").await.1;
    assert_eq!(record_ids(&kind), at(&[2, 7]));
    let prefix = prefix_for("bob");
    let to = list(&fx.app, &fx.project, &bob, &format!("&to={prefix}"))
        .await
        .1;
    assert_eq!(record_ids(&to), at(&[4]));
    let to = list(
        &fx.app,
        &fx.project,
        &bob,
        &format!("&to={}", address("bob")),
    )
    .await
    .1;
    assert_eq!(record_ids(&to), at(&[4]));
    // `to` names a letter bob cannot see: filters never widen visibility.
    let to = list(&fx.app, &fx.project, &bob, "&to=cccccccc").await.1;
    assert!(record_ids(&to).is_empty());
    assert_eq!(to["next_cursor"], 0);
    let after = list(&fx.app, &fx.project, &bob, "&after=2000-01-01T00:00:00Z")
        .await
        .1;
    assert_eq!(record_ids(&after).len(), 6);
    let after = list(&fx.app, &fx.project, &bob, "&after=2999-01-01T00:00:00Z").await;
    assert_eq!(after.0, StatusCode::OK);
    assert!(record_ids(&after.1).is_empty());
    assert_eq!(after.1["next_cursor"], 0);
    assert_eq!(after.1["has_more"], false);
    let combined = list(
        &fx.app,
        &fx.project,
        &bob,
        &format!("&after=2000-01-01T00:00:00Z&kind=mail,need&to={prefix}&limit=1"),
    )
    .await
    .1;
    assert_eq!(record_ids(&combined), at(&[4]));
    assert_eq!(combined["has_more"], false);
    // The creator, as sender, sees its own private mail under the filters.
    let own = list(&fx.app, &fx.project, &fx.token, "&to=cccccccc")
        .await
        .1;
    assert_eq!(record_ids(&own), at(&[6]));

    // Malformed parameters are 400 invalid_request; an oversize limit clamps.
    for query in [
        "&limit=0",
        "&limit=abc",
        "&limit=",
        "&limit=-3",
        "&after=yesterday",
        "&after=2026-10-11",
        "&kind=memo",
        "&kind=need,",
        "&to=bob",
        "&to=3f9a1c7",
    ] {
        let (status, value) = list(&fx.app, &fx.project, &bob, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {value}");
        assert_eq!(value["error"]["code"], "invalid_request", "{query}");
    }
    let (status, value) = list(&fx.app, &fx.project, &bob, "&limit=100000").await;
    assert_eq!(status, StatusCode::OK, "{value}");
}

#[tokio::test]
async fn appended_at_is_returned_in_list_and_single_get() {
    let (fx, bob, ids) = windows_fixture("windows-stamp", 110).await;
    let (_, page) = list(&fx.app, &fx.project, &bob, "").await;
    let mut previous = String::new();
    for record in page["records"].as_array().unwrap() {
        let at = record["appended_at"].as_str().unwrap().to_owned();
        assert!(at >= previous, "stamps never decrease with the sequence");
        let (status, headers, _) = get_letter(
            &fx.app,
            &fx.project,
            Some(&bob),
            record["message_id"].as_str().unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header(&headers, "x-vivi-appended-at"), Some(at.as_str()));
        previous = at;
    }
    // A replay answers with the original stamp.
    let before = page["records"][1]["appended_at"].clone();
    let raw = eml(&ids[1], "letter 0");
    let (status, replay) = fx.append(&ids[1], &raw, None, Some("need")).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    let (_, again) = list(&fx.app, &fx.project, &bob, "").await;
    assert_eq!(again["records"][1]["appended_at"], before);
}

#[tokio::test]
async fn lookup_by_handle_wire() {
    let (fx, bob, ids) = windows_fixture("windows-lookup", 120).await;
    let handle = |n: usize| ids[n][4..12].to_owned();
    let (_, listed) = list(&fx.app, &fx.project, &bob, "").await;

    // Hit (any case) and miss; records are the list record shape.
    let upper = handle(1).to_uppercase();
    let (status, found) = lookup(
        &fx.app,
        &fx.project,
        Some(&bob),
        json!({"handles": [upper, "00000000", handle(3)]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(record_ids(&found), vec![ids[1].clone(), ids[3].clone()]);
    assert_eq!(found["missing"], json!(["00000000"]));
    assert_eq!(found["records"][0], listed["records"][1]);
    assert!(found.get("next_cursor").is_none(), "lookup has no cursor");

    // Hidden (creator's mail to another mailspace) is `missing`, exactly
    // like an unknown handle.
    let (_, hidden) = lookup(
        &fx.app,
        &fx.project,
        Some(&bob),
        json!({"handles": [handle(6)]}),
    )
    .await;
    let (_, unknown) = lookup(
        &fx.app,
        &fx.project,
        Some(&bob),
        json!({"handles": ["abcdef12"]}),
    )
    .await;
    assert!(record_ids(&hidden).is_empty());
    assert_eq!(hidden["missing"], json!([handle(6)]));
    assert_eq!(unknown["missing"], json!(["abcdef12"]));
    // The sender sees it.
    let (_, own) = lookup(
        &fx.app,
        &fx.project,
        Some(&fx.token),
        json!({"handles": [handle(6)]}),
    )
    .await;
    assert_eq!(record_ids(&own), vec![ids[6].clone()]);

    // A handle may name several ids.
    let twin_a = format!("msg_{}{:016x}", "5eed0001", 1);
    let twin_b = format!("msg_{}{:016x}", "5eed0001", 2);
    for id in [&twin_a, &twin_b] {
        let (status, value) = fx.append(id, &eml(id, "twin"), None, Some("need")).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }
    let (_, twins) = lookup(
        &fx.app,
        &fx.project,
        Some(&bob),
        json!({"handles": ["5eed0001"]}),
    )
    .await;
    assert_eq!(record_ids(&twins), vec![twin_a, twin_b]);
    assert_eq!(twins["missing"], json!([]));

    // Count and shape: 1..=100, each 8 hex.
    let many: Vec<String> = (0..101).map(|n| format!("ffff{n:04x}")).collect();
    let max: Vec<String> = (0..100).map(|n| format!("ffff{n:04x}")).collect();
    for bad in [
        json!({"handles": many}),
        json!({"handles": []}),
        json!({"handles": ["xyz"]}),
        json!({"handles": ["4b8275c"]}),
        json!({"handles": ["4b8275cg"]}),
        json!({"handles": "4b8275cc"}),
        json!({}),
    ] {
        let (status, value) = lookup(&fx.app, &fx.project, Some(&bob), bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {value}");
        assert_eq!(value["error"]["code"], "invalid_request");
    }
    let (status, value) = lookup(&fx.app, &fx.project, Some(&bob), json!({"handles": max})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["missing"].as_array().unwrap().len(), 100);

    // Auth and project ordering: unknown project 404, missing/bad bearer 401.
    let unknown_project = format!(
        "{}{}",
        if fx.project.starts_with('z') {
            "y"
        } else {
            "z"
        },
        &fx.project[1..]
    );
    let body = json!({"handles": ["4b8275cc"]});
    let (status, value) = lookup(&fx.app, &unknown_project, Some(&bob), body.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
    assert_eq!(value["error"]["code"], "project_not_found");
    let (status, _) = lookup(&fx.app, &fx.project, None, body.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = lookup(&fx.app, &fx.project, Some("not-a-token"), body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Lookup never moves a cursor: the list is unchanged by it.
    let (_, after) = list(&fx.app, &fx.project, &bob, "").await;
    assert_eq!(after["next_cursor"], 10, "only the twins moved the head");
    let before = listed["records"].as_array().unwrap();
    assert_eq!(
        &after["records"].as_array().unwrap()[..before.len()],
        &before[..]
    );
}

#[tokio::test]
async fn list_and_lookup_gzip_only_when_asked() {
    let (fx, bob, ids) = windows_fixture("windows-gzip", 130).await;
    let uri = format!("/v1/projects/{}/items?since=0", fx.project);

    let (status, headers, plain) = get_raw(&fx.app, &uri, &bob, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("content-encoding").is_none());
    let plain_json: Value = serde_json::from_slice(&plain).unwrap();

    let (status, headers, zipped) = get_raw(&fx.app, &uri, &bob, Some("gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-encoding"), Some("gzip"));
    assert_eq!(&zipped[..2], &[0x1f, 0x8b], "gzip magic");
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&zipped[..]), &mut decoded)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&decoded).unwrap(),
        plain_json
    );

    // Lookup compresses the same way.
    let body = json!({"handles": [ids[1][4..12].to_owned()]})
        .to_string()
        .into_bytes();
    let (status, headers, zipped) = raw_post(
        &fx.app,
        &format!("/v1/projects/{}/items/lookup", fx.project),
        &bob,
        &body,
        Some("gzip"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-encoding"), Some("gzip"));
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&zipped[..]), &mut decoded)
        .unwrap();
    let decoded: Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(record_ids(&decoded), vec![ids[1].clone()]);
    let (_, headers, plain) = raw_post(
        &fx.app,
        &format!("/v1/projects/{}/items/lookup", fx.project),
        &bob,
        &body,
        None,
    )
    .await;
    assert!(headers.get("content-encoding").is_none());
    assert_eq!(serde_json::from_slice::<Value>(&plain).unwrap(), decoded);

    // The single-letter route stays uncompressed (raw bytes, byte-exact).
    let (status, headers, body) = {
        let (s, h, b) = get_raw(
            &fx.app,
            &format!("/v1/projects/{}/items/{}", fx.project, ids[1]),
            &bob,
            Some("gzip"),
        )
        .await;
        (s, h, b)
    };
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("content-encoding").is_none());
    assert_eq!(body, eml(&ids[1], "letter 0"));
}

async fn raw_post(
    app: &Router,
    uri: &str,
    token: &str,
    body: &[u8],
    accept_encoding: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(encoding) = accept_encoding {
        builder = builder.header("accept-encoding", encoding);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes.to_vec())
}
