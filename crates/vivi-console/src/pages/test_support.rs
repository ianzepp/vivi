//! Test helpers for the job pages: a project whose store is written by the
//! real pool record path, and a client over the console router.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tempfile::TempDir;
use tower::ServiceExt;
use vivi_core::mailspace::{Mailspace, SendRequest};
use vivi_storage::storage::Storage;

use crate::host::{Catalog, ProjectRef};
use crate::test_support::FakeHost;

/// A project on disk with one task, written by vivi-core as a running
/// vivi writes it. The extra writer keeps the WAL index in place.
pub struct Project {
    pub _dir: TempDir,
    pub root: PathBuf,
    pub space: Mailspace,
    /// The task's handle.
    pub task: String,
    _writer: Storage,
}

/// A project with one task whose body is `brief`.
pub fn project_with_task(brief: &str) -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let mut space = Mailspace::init(Some(&root)).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let body = format!("+++\nfrom = \"main\"\nbranch = \"factory/x\"\n+++\n\n{brief}\n");
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Fix the edge".into(),
            body,
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let task = sent.delivered[0].handle.clone();
    let writer = Storage::open_mailspace(&root.join(".vivi")).unwrap();
    Project {
        _dir: dir,
        root,
        space,
        task,
        _writer: writer,
    }
}

impl Project {
    /// Record a pool event on the task: the event note and its reply text.
    pub fn record(&self, event: &str, note: &str, text: &str) {
        self.space
            .record_pool_note(&self.task, event, note, text)
            .unwrap();
    }

    /// Record a dispatch of `job` at epoch second `at`.
    pub fn dispatch(&self, job: &str, at: i64) {
        let note = format!(
            "{{\"job\":\"{job}\",\"pool\":\"main\",\"slot\":\"slot-1\",\"machine\":\"studio-01\",\
             \"model\":\"model-a\",\"need\":\"a1f3c2d0ff\",\"kind\":\"build\",\"from\":\"main\",\
             \"branch\":\"factory/x\",\"start\":\"0123456789abcdef0123\",\"budget\":3300,\"at\":{at}}}"
        );
        self.record(
            "pool_dispatched",
            &note,
            &format!("pool dispatch {job}\npool main  slot slot-1"),
        );
    }
}

/// The catalog for a project at `root`, named `demo`.
pub fn catalog(root: &Path) -> Catalog {
    Catalog {
        projects: vec![ProjectRef {
            id: "proj-1".into(),
            name: "demo".into(),
            mailspace_root: root.to_path_buf(),
        }],
    }
}

/// Fetch `uri` from the console serving the project at `root`.
pub async fn get(root: &Path, uri: &str) -> (StatusCode, String) {
    let app = crate::router(Arc::new(FakeHost {
        catalog: catalog(root),
    }));
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}
