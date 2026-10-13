//! The job runner and a bridge grant: it reads `bridge.key` once, gives the
//! harness only the three `VIVI_MCP_*` variables, and leaves the key in no
//! file of the job directory. Run for real under bash against a throwaway
//! origin repository, unprivileged, like `job_runner_test.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::RUNNER;
use crate::pool::bridge::mcp_config_json;
use crate::pool::collect::JobResult;
use crate::pool::local_fixture::{Fixture, fixture, git};

const KEY: &str = "vivi_bridge_SECRET0123456789abcdef";
const SERVER: &str = "http://127.0.0.1:8080";
const PROJECT: &str = "abcde-fghjk";

struct Rig {
    f: Fixture,
    work: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let f = fixture();
        let work = tempfile::tempdir().unwrap();
        git(
            work.path(),
            &["clone", "-q", &f.repo.display().to_string(), "repo"],
        );
        Self { f, work }
    }

    fn job(&self) -> PathBuf {
        self.work.path().join("job")
    }

    /// Run the runner as a read-only job with `harness`; `bridge` is the
    /// content of `bridge.key`, or None for a job without a grant.
    fn run(&self, harness: &str, bridge: Option<&str>) -> JobResult {
        let job = self.job();
        std::fs::create_dir_all(&job).unwrap();
        let mut params = format!(
            "REMOTE='file://{}'\nJOB='j1'\nTASK='t1'\nFROM='main'\nBRANCH=''\nDEPTH='1'\n\
             BUDGET='60'\nMODEL='standin'\nSLOT='s1'\nSUBJECT='review'\nCOMMIT_NAME='Bot'\n\
             COMMIT_EMAIL='bot@localhost'\nENV_NAMES=''\nREAD_ONLY='1'\n",
            self.f.repo.display()
        );
        if bridge.is_some() {
            params.push_str(&format!(
                "BRIDGE_SERVER='{SERVER}'\nBRIDGE_PROJECT='{PROJECT}'\n"
            ));
        }
        for (name, content) in [
            ("params.env", params.as_str()),
            ("token", ""),
            ("harness.cmd", harness),
            ("brief.md", "review it\n"),
            ("rules.md", "rules\n"),
        ] {
            std::fs::write(job.join(name), content).unwrap();
        }
        if let Some(key) = bridge {
            std::fs::write(job.join("bridge.key"), key).unwrap();
            std::fs::write(job.join("mcp.json"), mcp_config_json(SERVER, PROJECT)).unwrap();
        }
        let out = Command::new("bash")
            .arg("-c")
            .arg(RUNNER)
            .env("WORK", self.work.path())
            .env("UNPRIVILEGED", "1")
            .env("BUILD_DIR", "build")
            .env("BUILD_PERSISTS", "1")
            .env("HOME", self.work.path().join("home"))
            .env("PATH", shimmed_path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stdout).into_owned();
        std::fs::read_to_string(job.join("result"))
            .map(|text| JobResult::parse(&text))
            .unwrap_or_else(|e| panic!("no result ({e}): {log}"))
    }

    /// Every file under the job directory, however deep.
    fn job_files(&self) -> Vec<PathBuf> {
        walk(&self.job())
    }
}

/// The runner's PATH: the repo's command shims first (they stand in for
/// commands a minimal slot may lack), then the machine's own.
fn shimmed_path() -> String {
    let shims = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pool/local-bin");
    format!("{}:{}", shims.display(), std::env::var("PATH").unwrap())
}

/// Every file under `dir`, however deep.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// What the harness prints for the three bridge variables, in order.
fn printenv_harness(to: &Path) -> String {
    format!(
        "printenv VIVI_MCP_API_KEY VIVI_MCP_SERVER VIVI_MCP_PROJECT_ID > {}",
        to.display()
    )
}

#[test]
fn the_harness_sees_the_bridge_as_exactly_the_three_variables() {
    let rig = Rig::new();
    let seen = rig.work.path().join("harness-env.txt");
    let result = rig.run(&printenv_harness(&seen), Some(KEY));
    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    let got = std::fs::read_to_string(&seen).unwrap();
    assert_eq!(got, format!("{KEY}\n{SERVER}\n{PROJECT}\n"), "all three");
}

#[test]
fn the_key_lands_in_no_file_of_the_job_directory() {
    let rig = Rig::new();
    let result = rig.run("cat brief.md", Some(KEY));
    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    assert!(
        !rig.job().join("bridge.key").exists(),
        "removed once read (UNPRIVILEGED)"
    );
    let params = std::fs::read_to_string(rig.job().join("params.env")).unwrap();
    assert!(
        params.contains(&format!("BRIDGE_SERVER='{SERVER}'\n")),
        "{params}"
    );
    assert!(
        params.contains(&format!("BRIDGE_PROJECT='{PROJECT}'\n")),
        "{params}"
    );
    assert!(
        !params.contains(KEY),
        "params.env never holds the key: {params}"
    );
    for path in rig.job_files() {
        let bytes = std::fs::read(&path).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(KEY),
            "{} holds the key",
            path.display()
        );
    }
}

#[test]
fn a_job_without_a_grant_sets_none_of_the_three() {
    let rig = Rig::new();
    let seen = rig.work.path().join("harness-env-none.txt");
    let result = rig.run(&printenv_harness(&seen), None);
    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        "",
        "no bridge variables without a grant"
    );
    assert!(!rig.job().join("bridge.key").exists());
    assert!(
        !rig.work.path().join("home/mcp.json").exists(),
        "no mcp.json in the agent's home without a grant"
    );
    let params = std::fs::read_to_string(rig.job().join("params.env")).unwrap();
    assert!(!params.contains("BRIDGE_"), "{params}");
}

#[test]
fn the_agent_finds_the_mcp_json_at_the_path_the_harness_used() {
    let rig = Rig::new();
    let seen = rig.work.path().join("mcp-seen.txt");
    let result = rig.run(
        &format!("cat /home/hand/mcp.json > {}", seen.display()),
        Some(KEY),
    );
    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    let text = std::fs::read_to_string(&seen).unwrap();
    assert!(text.contains("VIVI_MCP_SERVER"), "{text}");
    assert!(text.contains("VIVI_MCP_PROJECT_ID"), "{text}");
    assert!(text.contains("${VIVI_MCP_API_KEY}"), "{text}");
    assert!(!text.contains(KEY), "{text}");
}
