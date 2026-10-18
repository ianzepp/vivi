#![allow(dead_code)]
//! Tiny loopback HTTP script for proxy integration tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One recorded inbound request.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// One scripted reply.
#[derive(Clone)]
pub struct Script {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub sse: bool,
}

/// Shared mock state.
#[derive(Clone)]
pub struct Mock {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    scripts: Vec<Script>,
    seen: Vec<Seen>,
    token_status: u16,
    token_body: String,
}

impl Mock {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                scripts: Vec::new(),
                seen: Vec::new(),
                token_status: 200,
                token_body: r#"{"access_token":"new","refresh_token":"r2","expires_in":3600}"#
                    .into(),
            })),
        }
    }

    pub fn push(&self, script: Script) {
        self.inner.lock().unwrap().scripts.push(script);
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.inner.lock().unwrap().seen.clone()
    }

    pub fn set_token(&self, status: u16, body: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.token_status = status;
        inner.token_body = body.to_string();
    }
}

/// Bind a loopback listener and serve `mock` until the process ends.
pub fn serve(mock: Mock) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mock = mock.clone();
            thread::spawn(move || handle(stream, &mock));
        }
    });
    addr
}

fn handle(mut stream: TcpStream, mock: &Mock) {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let Some((head, body)) = read_request(&mut stream) else {
        return;
    };
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let headers = header_list(lines);
    let seen = Seen {
        method: method.clone(),
        path: path.clone(),
        headers,
        body,
    };
    mock.inner.lock().unwrap().seen.push(seen);
    if path.contains("/oauth/token") || path.ends_with("/token") {
        write_token(&mut stream, mock);
        return;
    }
    let script = next_script(mock);
    write_script(&mut stream, &script);
}

fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut raw = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(idx) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..idx]).into_owned();
            let mut body = raw[idx + 4..].to_vec();
            let want = content_length(&head).unwrap_or(body.len());
            while body.len() < want {
                let n = stream.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
            }
            return Some((head, body));
        }
        if raw.len() > 1024 * 1024 {
            return None;
        }
    }
    None
}

fn content_length(head: &str) -> Option<usize> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

fn header_list<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    headers
}

fn next_script(mock: &Mock) -> Script {
    let mut inner = mock.inner.lock().unwrap();
    if inner.scripts.is_empty() {
        Script {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: b"{\"ok\":true}".to_vec(),
            sse: false,
        }
    } else {
        inner.scripts.remove(0)
    }
}

fn write_token(stream: &mut TcpStream, mock: &Mock) {
    let inner = mock.inner.lock().unwrap();
    let body = inner.token_body.clone();
    let status = inner.token_status;
    drop(inner);
    let _ = write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn write_script(stream: &mut TcpStream, script: &Script) {
    if script.sse {
        let _ = write!(
            stream,
            "HTTP/1.1 {} OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
            script.status
        );
        let _ = stream.write_all(&script.body);
        return;
    }
    let _ = write!(
        stream,
        "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n",
        script.status,
        script.body.len()
    );
    for (name, value) in &script.headers {
        let _ = write!(stream, "{name}: {value}\r\n");
    }
    let _ = write!(stream, "\r\n");
    let _ = stream.write_all(&script.body);
}

/// Helper SSE body of one or more events.
pub fn sse_body(events: &[&str]) -> Vec<u8> {
    let mut out = String::new();
    for event in events {
        out.push_str("data: ");
        out.push_str(event);
        out.push_str("\n\n");
    }
    out.into_bytes()
}
