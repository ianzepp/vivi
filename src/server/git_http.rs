//! Git smart HTTP over each project's lanes.
//!
//! `/git/<project>/<target>.git/<rest>`: `<project>` is a project id or a
//! hosted project name, `<target>` a `[pump.targets.<id>]` of that project.
//! The lane (bare repo) is the one `target_log` reads. Credentials are the
//! machine-key session token, as Basic (`git` with a credential helper, see
//! `vivi key git-credential`) or Bearer. Requests are served by
//! `git-http-backend` as a CGI child. A push that updates a protected
//! branch (`main`, `vivi/main`, the target's main) is refused unless the
//! principal is the host.

use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use tokio::io::AsyncWriteExt;

use crate::server::AppState;
use crate::server::jobs::{self, JobRecord};
use crate::server::mcp::host::hosted_entry;
use crate::server::mcp::live::target_git::lane_for;
use crate::server::mcp::server::{ServerPrincipal, authenticate, resolve_grant};

/// The largest request body (a push pack) the route accepts.
pub const BODY_LIMIT: usize = 512 * 1024 * 1024;

fn plain(status: StatusCode, message: &str) -> Response {
    (
        status,
        [("content-type", "text/plain; charset=utf-8")],
        format!("{message}\n"),
    )
        .into_response()
}

/// A refusal git itself prints: a normal git response whose only packet is
/// `ERR <message>`, which git reports as `remote error: <message>`. A bare
/// HTTP 403 would only show "RPC failed; HTTP 403".
fn git_refusal(service: &str, advertisement: bool, message: &str) -> Response {
    let kind = if advertisement {
        "advertisement"
    } else {
        "result"
    };
    let text = format!("ERR vivi: {message}\n");
    let packet = format!("{:04x}{text}", text.len() + 4);
    (
        StatusCode::OK,
        [
            ("content-type", format!("application/x-{service}-{kind}")),
            ("cache-control", "no-cache".to_owned()),
        ],
        packet,
    )
        .into_response()
}

fn pkt(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend_from_slice(data);
    out
}

/// The capabilities the client sent on its first push command.
fn push_capabilities(body: &[u8]) -> Vec<String> {
    let Some(len) = body
        .get(..4)
        .and_then(|h| std::str::from_utf8(h).ok())
        .and_then(|h| usize::from_str_radix(h, 16).ok())
    else {
        return Vec::new();
    };
    let Some(first) = body.get(4..len) else {
        return Vec::new();
    };
    first
        .splitn(2, |b| *b == 0)
        .nth(1)
        .map(|caps| {
            String::from_utf8_lossy(caps)
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Refuse a push the way a server-side hook would: a push report marking
/// every pushed ref `ng <ref> <reason>`, so git prints
/// `! [remote rejected] <ref> (<reason>)`. Falls back to an ERR packet when
/// the client did not ask for a report.
fn push_refusal(body: &[u8], refs: &[String], reason: &str) -> Response {
    let caps = push_capabilities(body);
    if !caps
        .iter()
        .any(|c| c == "report-status" || c == "report-status-v2")
    {
        return git_refusal("git-receive-pack", false, reason);
    }
    let mut report = pkt(b"unpack ok\n");
    for name in refs {
        report.extend(pkt(format!("ng {name} {reason}\n").as_bytes()));
    }
    report.extend_from_slice(b"0000");
    let body = if caps
        .iter()
        .any(|c| c == "side-band-64k" || c == "side-band")
    {
        let chunk = if caps.iter().any(|c| c == "side-band-64k") {
            65515
        } else {
            995
        };
        let mut out = pkt(format!("\u{2}vivi: {reason}\n").as_bytes());
        for part in report.chunks(chunk) {
            let mut band = vec![1u8];
            band.extend_from_slice(part);
            out.extend(pkt(&band));
        }
        out.extend_from_slice(b"0000");
        out
    } else {
        report
    };
    (
        StatusCode::OK,
        [
            ("content-type", "application/x-git-receive-pack-result"),
            ("cache-control", "no-cache"),
        ],
        body,
    )
        .into_response()
}

fn unauthorized(message: &str) -> Response {
    let mut response = plain(StatusCode::UNAUTHORIZED, message);
    response.headers_mut().insert(
        "www-authenticate",
        HeaderValue::from_static("Basic realm=\"vivi\""),
    );
    response
}

/// The session token: Basic's password, or a Bearer.
fn presented_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("authorization")?.to_str().ok()?;
    if let Some(token) = value.strip_prefix("Bearer ") {
        return Some(token.trim().to_owned());
    }
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    text.split_once(':')
        .map(|(_, password)| password.to_owned())
}

#[derive(Clone)]
struct Route {
    project: String,
    target: String,
    rest: String,
}

fn parse_path(path: &str) -> Option<Route> {
    let mut parts = path.trim_start_matches('/').splitn(3, '/');
    let project = parts.next().filter(|p| !p.is_empty())?;
    let target = parts
        .next()?
        .strip_suffix(".git")
        .filter(|t| !t.is_empty())?;
    let rest = parts.next()?;
    let ok = |s: &str| !s.contains("..") && !s.contains('\\') && !s.contains('\0');
    (ok(project) && ok(target) && ok(rest)).then(|| Route {
        project: project.to_owned(),
        target: target.to_owned(),
        rest: rest.to_owned(),
    })
}

/// A lane: where it is and the branches only the host may move.
struct Lane {
    path: PathBuf,
    protected: Vec<String>,
}

/// Project id, or hosted name, to its lane. Blocking.
fn resolve_lane(state: &AppState, auth: &ServerPrincipal, route: &Route) -> Result<Lane, Response> {
    let meta = if auth.host {
        hosted_entry(state, &route.project)
            .or_else(|| {
                state
                    .projects
                    .load_meta_including_retired(&route.project)
                    .ok()
            })
            .ok_or_else(|| plain(StatusCode::NOT_FOUND, "no such project"))?
    } else {
        let resolved = resolve_grant(state, &auth.grants, &route.project)
            .map_err(|reason| plain(StatusCode::FORBIDDEN, &reason))?;
        state
            .projects
            .load_meta_including_retired(&resolved.project_id)
            .map_err(|_| plain(StatusCode::NOT_FOUND, "no such project"))?
    };
    lane_of(state, meta, route)
}

/// A job ticket's lane: only its own project and target.
fn resolve_job_lane(state: &AppState, job: &JobRecord, route: &Route) -> Result<Lane, Response> {
    if !job.is_project(&route.project) || route.target != job.manifest.target {
        return Err(plain(
            StatusCode::FORBIDDEN,
            "this job ticket reaches only its own project and target",
        ));
    }
    if !job.live() {
        return Err(plain(
            StatusCode::FORBIDDEN,
            &format!(
                "this job is {}; its ticket no longer reaches git",
                job.status()
            ),
        ));
    }
    let meta = state
        .projects
        .load_meta_including_retired(&job.manifest.project_id)
        .map_err(|_| plain(StatusCode::NOT_FOUND, "no such project"))?;
    lane_of(state, meta, route)
}

fn lane_of(
    state: &AppState,
    meta: crate::server::projects::ProjectMeta,
    route: &Route,
) -> Result<Lane, Response> {
    if meta.is_archived() || state.projects.is_retired(&meta.project_id) {
        return Err(plain(StatusCode::FORBIDDEN, "project is archived"));
    }
    let root = meta
        .mailspace_root
        .ok_or_else(|| plain(StatusCode::NOT_FOUND, "the project is not hosted here"))?;
    let (path, main) =
        lane_for(&root, &route.target).map_err(|message| plain(StatusCode::NOT_FOUND, &message))?;
    let main_ref = if main.starts_with("refs/") {
        main
    } else {
        format!("refs/heads/{main}")
    };
    let mut protected = vec![
        "refs/heads/main".to_owned(),
        "refs/heads/vivi/main".to_owned(),
    ];
    if !protected.contains(&main_ref) {
        protected.push(main_ref);
    }
    Ok(Lane { path, protected })
}

/// The ref names a receive-pack request updates, from its leading pkt-lines.
fn pushed_refs(body: &[u8]) -> Result<Vec<String>, String> {
    let mut refs = Vec::new();
    let mut at = 0;
    loop {
        let Some(len_hex) = body.get(at..at + 4) else {
            break;
        };
        let len = std::str::from_utf8(len_hex)
            .ok()
            .and_then(|h| usize::from_str_radix(h, 16).ok())
            .ok_or("malformed push command list")?;
        if len == 0 {
            break;
        }
        let payload = body
            .get(at + 4..at + len)
            .ok_or("truncated push command list")?;
        at += len;
        let line = payload.split(|b| *b == 0).next().unwrap_or_default();
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\n');
        if line.starts_with("push-cert") {
            return Err("signed pushes are not supported".to_owned());
        }
        if line.starts_with("shallow ") {
            continue;
        }
        let mut fields = line.split(' ');
        let (_old, _new, name) = (fields.next(), fields.next(), fields.next());
        refs.push(name.ok_or("malformed push command")?.to_owned());
    }
    Ok(refs)
}

fn gunzip(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(body)
        .read_to_end(&mut out)
        .map_err(|e| format!("cannot decompress the request body: {e}"))?;
    Ok(out)
}

pub async fn git_handler(
    State(state): State<AppState>,
    Path(path): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(token) = presented_token(&headers) else {
        return unauthorized("credentials required");
    };
    let job: Option<JobRecord> = if token.starts_with(jobs::TICKET_PREFIX) {
        let (lookup_state, lookup_token) = (state.clone(), token.clone());
        let found = tokio::task::spawn_blocking(move || {
            jobs::authenticate_ticket(&lookup_state, &lookup_token, None)
        })
        .await
        .ok()
        .flatten();
        let Some(found) = found else {
            return unauthorized("invalid or expired token");
        };
        Some(found)
    } else {
        None
    };
    let auth = match &job {
        Some(job) => ServerPrincipal {
            principal: format!("job:{}", job.manifest.job_id),
            key_id: None,
            host: false,
            grants: Vec::new(),
        },
        None => match authenticate(&state, &token) {
            Ok(auth) => auth,
            Err(_) => return unauthorized("invalid or expired token"),
        },
    };
    let Some(route) = parse_path(&path) else {
        return plain(
            StatusCode::NOT_FOUND,
            "expected /git/<project>/<target>.git/<info/refs|git-upload-pack|git-receive-pack>",
        );
    };
    let query = uri.query().unwrap_or_default().to_owned();
    let service = if route.rest.ends_with("git-receive-pack") || query.contains("git-receive-pack")
    {
        "git-receive-pack"
    } else if route.rest.ends_with("git-upload-pack") || query.contains("git-upload-pack") {
        "git-upload-pack"
    } else {
        "-"
    };

    let lane = {
        let (state, auth_for, route_for, job_for) =
            (state.clone(), auth.clone(), route.clone(), job.clone());
        match tokio::task::spawn_blocking(move || match &job_for {
            Some(job) => resolve_job_lane(&state, job, &route_for),
            None => resolve_lane(&state, &auth_for, &route_for),
        })
        .await
        {
            Ok(Ok(lane)) => lane,
            Ok(Err(response)) => {
                log(&auth, &route, service, response.status());
                if service == "-" {
                    return response;
                }
                let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap_or_default();
                let message = String::from_utf8_lossy(&bytes).trim().to_owned();
                return git_refusal(service, route.rest == "info/refs", &message);
            }
            Err(_) => return plain(StatusCode::INTERNAL_SERVER_ERROR, "lane lookup failed"),
        }
    };

    let gzipped = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    let body = if gzipped {
        match gunzip(&body) {
            Ok(plain_body) => plain_body,
            Err(message) => return plain(StatusCode::BAD_REQUEST, &message),
        }
    } else {
        body.to_vec()
    };

    if service == "git-receive-pack" && method == Method::POST && !auth.host {
        let refs = match pushed_refs(&body) {
            Ok(refs) => refs,
            Err(message) => {
                return plain(StatusCode::BAD_REQUEST, &message);
            }
        };
        if let Some(job) = &job {
            let allowed = format!("refs/heads/{}", job.manifest.branch);
            if refs.is_empty() || refs.iter().any(|name| *name != allowed) {
                log(&auth, &route, service, StatusCode::FORBIDDEN);
                return push_refusal(
                    &body,
                    &refs,
                    &format!("a job ticket may push only {allowed}"),
                );
            }
        } else if let Some(name) = refs.iter().find(|name| lane.protected.contains(name)) {
            log(&auth, &route, service, StatusCode::FORBIDDEN);
            return push_refusal(
                &body,
                &refs,
                &format!(
                    "push to {name} refused: only the host may update {}; push a work branch and let the pump land it",
                    lane.protected.join(", ")
                ),
            );
        }
    }

    let response = run_backend(&lane, &route, &method, &query, &headers, &auth, body).await;
    log(&auth, &route, service, response.status());
    response
}

fn log(auth: &ServerPrincipal, route: &Route, service: &str, status: StatusCode) {
    tracing::info!(
        "git {} project={} target={} service={} status={}",
        auth.principal,
        route.project,
        route.target,
        service,
        status.as_u16()
    );
}

async fn backend_path() -> Option<PathBuf> {
    let out = tokio::process::Command::new("git")
        .arg("--exec-path")
        .output()
        .await
        .ok()?;
    let dir = String::from_utf8(out.stdout).ok()?;
    Some(PathBuf::from(dir.trim()).join("git-http-backend"))
}

async fn run_backend(
    lane: &Lane,
    route: &Route,
    method: &Method,
    query: &str,
    headers: &HeaderMap,
    auth: &ServerPrincipal,
    body: Vec<u8>,
) -> Response {
    let (Some(parent), Some(name)) = (
        lane.path.parent(),
        lane.path.file_name().and_then(|n| n.to_str()),
    ) else {
        return plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the lane path has no parent",
        );
    };
    let Some(backend) = backend_path().await else {
        return plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "git-http-backend not found",
        );
    };
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let mut command = tokio::process::Command::new(backend);
    command
        .env("GIT_PROJECT_ROOT", parent)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", format!("/{name}/{}", route.rest))
        .env("REQUEST_METHOD", method.as_str())
        .env("QUERY_STRING", query)
        .env("CONTENT_TYPE", header("content-type"))
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("REMOTE_USER", &auth.principal)
        .env(
            "REMOTE_ADDR",
            headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("0.0.0.0"),
        )
        .env_remove("HTTP_CONTENT_ENCODING")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(protocol) = headers.get("git-protocol").and_then(|v| v.to_str().ok()) {
        command.env("GIT_PROTOCOL", protocol);
        command.env("HTTP_GIT_PROTOCOL", protocol);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("cannot run git-http-backend: {e}"),
            );
        }
    };
    let mut stdin = child.stdin.take();
    let writer = tokio::spawn(async move {
        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(&body).await;
            let _ = stdin.shutdown().await;
        }
    });
    let output = child.wait_with_output().await;
    let _ = writer.await;
    let output = match output {
        Ok(output) => output,
        Err(e) => {
            return plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("git-http-backend failed: {e}"),
            );
        }
    };
    if output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("git-http-backend gave no output: {}", stderr.trim()),
        );
    }
    cgi_response(&output.stdout)
}

/// CGI output: header lines, a blank line, the body. `Status:` is the code.
fn cgi_response(out: &[u8]) -> Response {
    let (head, body) = split_cgi(out);
    let mut status = StatusCode::OK;
    let mut response_headers = HeaderMap::new();
    for line in String::from_utf8_lossy(head).lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("status") {
            if let Some(code) = value
                .split_whitespace()
                .next()
                .and_then(|c| c.parse::<u16>().ok())
                .and_then(|c| StatusCode::from_u16(c).ok())
            {
                status = code;
            }
        } else if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.trim().as_bytes()),
            HeaderValue::from_str(value),
        ) {
            response_headers.append(name, value);
        }
    }
    let mut response = Response::new(Body::from(body.to_vec()));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

fn split_cgi(out: &[u8]) -> (&[u8], &[u8]) {
    for (pattern, skip) in [(&b"\r\n\r\n"[..], 4), (&b"\n\n"[..], 2)] {
        if let Some(at) = out.windows(pattern.len()).position(|w| w == pattern) {
            return (&out[..at], &out[at + skip..]);
        }
    }
    (out, &[])
}
