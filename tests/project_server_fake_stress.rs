//! Stress for the fake project server's accept path: connections made
//! immediately after `ProjectServer::start()` returns must all be served —
//! 200 in a tight sequential loop, then a burst of 200 from 8 threads.
//! This guards the macOS hazard where an accepted socket inherited
//! `O_NONBLOCK` from the nonblocking listener and the fake closed
//! connections before their request bytes had arrived.

#[path = "support/project_server.rs"]
mod project_server;

use std::sync::{Arc, Mutex};
use std::thread;

use project_server::{ProjectServer, http};

/// Any string passes as a PEM at challenge time; the fake binds it verbatim.
const PEM_BODY: &str =
    r#"{"public_key_pem":"-----BEGIN PUBLIC KEY-----\nstress\n-----END PUBLIC KEY-----\n"}"#;

/// One POST to the identity challenge route; returns the HTTP status.
fn challenge(server: &ProjectServer) -> u16 {
    let reply = http(
        &server.url(),
        "POST",
        &format!("/v1/projects/{}/identities/challenge", server.project()),
        &[("Content-Type", "application/json")],
        Some(PEM_BODY),
    );
    reply.status
}

/// The fake must serve 200 connections made in a tight loop straight after
/// construction: none may be dropped before its request bytes arrive.
#[test]
fn two_hundred_connections_made_immediately_are_all_served() {
    let server = ProjectServer::start();
    for index in 0..200 {
        let status = challenge(&server);
        assert_eq!(status, 200, "sequential request {index} was not served");
    }
    assert_eq!(server.requests_seen(), 200);
}

/// A burst of 200 connections from 8 parallel threads must all be served
/// too: no accepted connection may be dropped under load.
#[test]
fn two_hundred_connections_from_eight_threads_are_all_served() {
    let server = Arc::new(ProjectServer::start());
    let statuses = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..8 {
            let server = Arc::clone(&server);
            let statuses = &statuses;
            scope.spawn(move || {
                let mut local = Vec::new();
                for _ in 0..25 {
                    local.push(challenge(&server));
                }
                statuses.lock().unwrap().extend(local);
            });
        }
    });
    let statuses = statuses.into_inner().unwrap();
    assert_eq!(statuses.len(), 200);
    for (index, status) in statuses.iter().enumerate() {
        assert_eq!(*status, 200, "burst request {index} was not served");
    }
    assert_eq!(server.requests_seen(), 200);
}
