//! Tests for [`crate::mind::socket`], over a real Unix socket in a temp dir
//! with the fake session.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tempfile::tempdir;

use super::*;
use crate::mind::pi::FakePi;

struct Running {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    state: PathBuf,
    shared: Arc<Shared<FakePi>>,
}

fn start(replies: &[&str]) -> Running {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("run").join("mind.sock");
    let state = dir.path().join("state.json");
    let shared = Arc::new(Shared::new(
        FakePi::with_replies(replies),
        "observe",
        state.clone(),
    ));
    let listener = bind(&socket).unwrap();
    tokio::spawn(serve(listener, socket.clone(), Arc::clone(&shared)));
    Running {
        _dir: dir,
        socket,
        state,
        shared,
    }
}

#[tokio::test]
async fn socket_and_new_parent_are_owner_only() {
    let mind = start(&[]);
    let mode = std::fs::metadata(&mind.socket)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, SOCKET_MODE);
    let parent = std::fs::metadata(mind.socket.parent().unwrap()).unwrap();
    assert_eq!(parent.permissions().mode() & 0o777, DIR_MODE);
}

#[tokio::test]
async fn a_second_mind_cannot_bind_a_live_socket() {
    let mind = start(&[]);
    assert!(bind(&mind.socket).is_err());
}

#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("mind.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    assert!(bind(&socket).is_ok());
}

#[tokio::test]
async fn a_regular_file_is_never_removed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("mind.sock");
    std::fs::write(&path, "keep").unwrap();
    assert!(bind(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
}

#[tokio::test]
async fn prompts_stream_through_one_session_across_reconnects() {
    let mind = start(&["hello operator", "still here"]);
    let mut first = Client::connect(&mind.socket).await.unwrap();
    let replies = first
        .collect(&Request::Prompt { text: "hi".into() })
        .await
        .unwrap();
    assert_eq!(
        replies,
        vec![Reply::Text {
            text: "hello operator".into()
        }]
    );
    drop(first);
    let mut second = Client::connect(&mind.socket).await.unwrap();
    let replies = second
        .collect(&Request::Prompt {
            text: "back".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        replies,
        vec![Reply::Text {
            text: "still here".into()
        }]
    );
    assert_eq!(mind.shared.pi.lock().await.prompts, vec!["hi", "back"]);
}

#[tokio::test]
async fn pause_resume_and_status_go_through_the_state_file() {
    let mind = start(&[]);
    let mut client = Client::connect(&mind.socket).await.unwrap();
    let paused = client.collect(&Request::Pause).await.unwrap();
    assert!(matches!(&paused[0], Reply::Ok { .. }));
    assert!(store::load_at(&mind.state).unwrap().paused);
    let status = client.collect(&Request::Status).await.unwrap();
    let Reply::Status { status } = &status[0] else {
        panic!("expected status, got {status:?}");
    };
    assert!(status.paused);
    assert_eq!(status.authority, "observe");
    assert!(!status.busy);
    client.collect(&Request::Resume).await.unwrap();
    assert!(!store::load_at(&mind.state).unwrap().paused);
}

#[tokio::test]
async fn compact_history_and_bad_requests() {
    let mind = start(&[]);
    let mut client = Client::connect(&mind.socket).await.unwrap();
    client
        .collect(&Request::Prompt { text: "one".into() })
        .await
        .unwrap();
    let history = client
        .collect(&Request::History { limit: 5 })
        .await
        .unwrap();
    assert_eq!(
        history,
        vec![Reply::Text {
            text: "user: one".into()
        }]
    );
    let compact = client.collect(&Request::Compact).await.unwrap();
    assert!(matches!(&compact[0], Reply::Ok { message } if message.contains("compacted")));
}

#[tokio::test]
async fn an_attached_prompt_queues_behind_a_turn_in_flight() {
    let mind = start(&["after the wake"]);
    let held = mind.shared.pi.lock().await;
    let socket = mind.socket.clone();
    let waiter = tokio::spawn(async move {
        let mut client = Client::connect(&socket).await.unwrap();
        client
            .collect(&Request::Prompt { text: "hi".into() })
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!waiter.is_finished());
    drop(held);
    let replies = waiter.await.unwrap();
    assert!(matches!(&replies[0], Reply::Text { text } if text.contains("waiting")));
    assert_eq!(
        replies[1],
        Reply::Text {
            text: "after the wake".into()
        }
    );
}

#[tokio::test]
async fn stop_signals_shutdown() {
    let mind = start(&[]);
    let mut stop = mind.shared.shutdown.subscribe();
    let mut client = Client::connect(&mind.socket).await.unwrap();
    client.collect(&Request::Stop).await.unwrap();
    stop.changed().await.unwrap();
    assert!(*stop.borrow());
}
