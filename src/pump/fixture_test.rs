//! A real mailspace project for the pump tests that act on pump records:
//! the `operator` and `mind` identities, a `[pump]` table, a pump context
//! whose project and home are the project's own, and needs to send records
//! for.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{NeedRecord, State, Status, Unit};

/// The fake clock's start.
pub(crate) const T0: u64 = 1_000_000;

pub(crate) struct Project {
    pub dir: TempDir,
    pub mailspace: Mailspace,
    pub ctx: Ctx,
    pub host: Arc<FakeHost>,
}

impl Project {
    /// The project root.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The pump home `vivi pump` resolves for this project.
    pub fn home(&self) -> PathBuf {
        self.dir.path().join(".vivi").join("pump")
    }

    /// Send a need to the pump identity and return its handle.
    pub fn need(&self, subject: &str) -> String {
        send_need(&self.mailspace, subject, "mind")
    }

    /// A pump context over the project, home at [`Project::home`].
    pub fn pump_ctx(&self) -> Ctx {
        let mut ctx = self.ctx.clone();
        ctx.home = self.home();
        fs::create_dir_all(&ctx.home).unwrap();
        ctx
    }
}

/// A fresh project. The pump's home is created lazily by the code under test.
pub(crate) fn project() -> Project {
    let dir = tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(dir.path())).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("elsewhere").unwrap();
    let toml = dir.path().join(".vivi").join("mailspace.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[pump]\nbranch = \"next\"\n");
    fs::write(&toml, text).unwrap();
    let (mut ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    ctx.project = dir.path().to_path_buf();
    ctx.home = dir.path().join(".vivi").join("pump");
    ctx.mailspace = String::from("fixture");
    Project {
        dir,
        mailspace,
        ctx,
        host,
    }
}

/// Send a need from `operator` to `to` and return its handle.
pub(crate) fn send_need(mailspace: &Mailspace, subject: &str, to: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

/// State holding `need` planned into the given `(uid, status)` units.
pub(crate) fn planned(need: &str, units: &[(&str, Status)]) -> State {
    let mut st = State::default();
    st.needs.insert(
        need.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            units: units.iter().map(|(uid, _)| (*uid).to_owned()).collect(),
            ..NeedRecord::default()
        },
    );
    for (uid, status) in units {
        st.units.insert(
            (*uid).to_owned(),
            Unit {
                need: need.to_owned(),
                target: String::from("default"),
                status: *status,
                ..Unit::default()
            },
        );
    }
    st
}
