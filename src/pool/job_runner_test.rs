//! The job runner in read-only mode, run for real under bash against a
//! throwaway origin repository: no slot, no provider, a stand-in shell
//! harness. The runner runs as the ordinary user (`UNPRIVILEGED`), the way an
//! SSH or local slot runs it.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::RUNNER;
use crate::pool::collect::JobResult;
use crate::pool::local_fixture::{Fixture, fixture, git};

struct Rig {
    f: Fixture,
    work: tempfile::TempDir,
    start: String,
}

impl Rig {
    fn new() -> Self {
        let f = fixture();
        let work = tempfile::tempdir().unwrap();
        git(
            work.path(),
            &["clone", "-q", &f.repo.display().to_string(), "repo"],
        );
        let start = git(&f.repo, &["rev-parse", "main"]);
        Self { f, work, start }
    }

    fn repo(&self) -> PathBuf {
        self.work.path().join("repo")
    }

    fn origin_refs(&self) -> String {
        git(&self.f.repo, &["for-each-ref"])
    }

    fn branches(&self) -> String {
        git(
            &self.repo(),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
            ],
        )
    }

    /// Run the runner as a read-only job with `harness`; the result it wrote.
    fn run(&self, harness: &str, build_dir: &str) -> JobResult {
        self.run_with_facts(harness, build_dir, &[])
    }

    /// Like [`Rig::run`], with `facts` as the pool's fact commands.
    fn run_with_facts(&self, harness: &str, build_dir: &str, facts: &[&str]) -> JobResult {
        let job = self.work.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        let params = format!(
            "REMOTE='file://{}'\nJOB='j1'\nTASK='t1'\nFROM='main'\nBRANCH=''\nDEPTH='1'\nBUDGET='60'\nMODEL='standin'\nSLOT='s1'\nSUBJECT='review'\nCOMMIT_NAME='Bot'\nCOMMIT_EMAIL='bot@localhost'\nENV_NAMES=''\nREAD_ONLY='1'\n",
            self.f.repo.display()
        );
        for (name, content) in [
            ("params.env", params.as_str()),
            ("token", ""),
            ("harness.cmd", harness),
            ("brief.md", "review it\n"),
            ("rules.md", "rules\n"),
        ] {
            std::fs::write(job.join(name), content).unwrap();
        }
        if !facts.is_empty() {
            let list: String = facts.iter().map(|c| format!("{c}\n")).collect();
            std::fs::write(job.join("facts.cmd"), list).unwrap();
        }
        let home = self.work.path().join("home");
        let shims = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pool/local-bin");
        let path = format!("{}:{}", shims.display(), std::env::var("PATH").unwrap());
        let out = Command::new("bash")
            .arg("-c")
            .arg(RUNNER)
            .env("WORK", self.work.path())
            .env("UNPRIVILEGED", "1")
            .env("BUILD_DIR", build_dir)
            .env("BUILD_PERSISTS", "1")
            .env("HOME", &home)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stdout).into_owned();
        let text = std::fs::read_to_string(job.join("result"))
            .unwrap_or_else(|e| panic!("no result ({e}): {log}"));
        JobResult::parse(&text)
    }

    fn assert_clean_and_detached(&self) {
        let repo = self.repo();
        assert_eq!(
            git(
                &repo,
                &[
                    "status",
                    "--porcelain",
                    "--",
                    ".",
                    ":(exclude)build",
                    ":(exclude)out"
                ]
            ),
            ""
        );
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), self.start);
        assert_eq!(
            git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "HEAD",
            "detached"
        );
    }
}

#[test]
fn a_tree_the_agent_only_read_is_reported_unchanged() {
    let rig = Rig::new();
    let (branches, refs) = (rig.branches(), rig.origin_refs());
    let result = rig.run(
        "cat a.txt; git log --oneline | head -1; echo receipt",
        "build",
    );
    assert_eq!(result.push(), "read_only_unchanged");
    assert_eq!(result.get("branch"), "");
    assert_eq!(result.get("sha"), "");
    assert_eq!(result.get("discarded_count"), "");
    assert!(result.discarded_paths().is_empty());
    assert_eq!(result.get("start"), rig.start);
    assert_eq!(rig.branches(), branches, "no branch made or moved");
    assert_eq!(rig.origin_refs(), refs, "nothing was published");
    rig.assert_clean_and_detached();
}

#[test]
fn a_modified_file_a_new_file_and_a_staged_file_are_listed_and_discarded() {
    let rig = Rig::new();
    let (branches, refs) = (rig.branches(), rig.origin_refs());
    let harness = "echo extra >> a.txt; echo n > new.txt; echo s > staged.txt; git add staged.txt; mkdir -p sub; echo d > sub/deep.txt";
    let result = rig.run(harness, "build");
    assert_eq!(result.push(), "read_only_discarded");
    assert_eq!(result.get("discarded_count"), "4");
    assert_eq!(
        result.discarded_paths(),
        ["a.txt", "new.txt", "staged.txt", "sub/deep.txt"]
    );
    assert_eq!(result.push_line(), "read_only_discarded 4");
    let repo = rig.repo();
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "one\ntwo\n"
    );
    for gone in ["new.txt", "staged.txt", "sub/deep.txt"] {
        assert!(!repo.join(gone).exists(), "{gone} survived");
    }
    assert_eq!(rig.branches(), branches);
    assert_eq!(rig.origin_refs(), refs);
    rig.assert_clean_and_detached();
}

#[test]
fn a_change_inside_the_build_directory_is_not_a_change_and_is_kept() {
    let rig = Rig::new();
    let harness = "mkdir -p out; echo o > out/obj.o";
    let result = rig.run(harness, "out");
    assert_eq!(result.push(), "read_only_unchanged", "{:?}", result.fields);
    assert!(
        rig.repo().join("out/obj.o").is_file(),
        "the warm build directory stays"
    );
    let both = rig.run("mkdir -p out; echo o > out/more.o; echo n > new.txt", "out");
    assert_eq!(both.discarded_paths(), ["new.txt"]);
    assert!(rig.repo().join("out/more.o").is_file());
}

#[test]
fn the_list_of_paths_is_capped_and_the_count_is_not() {
    let rig = Rig::new();
    let harness = "mkdir many; for i in $(seq 1 60); do echo $i > many/f$i.txt; done";
    let result = rig.run(harness, "build");
    assert_eq!(result.get("discarded_count"), "60");
    let listed = result.discarded_paths();
    assert_eq!(listed.len(), 50);
    assert!(listed.iter().all(|p| p.starts_with("many/f")));
    assert!(!rig.repo().join("many").exists());
}

#[test]
fn a_commit_the_agent_made_is_reported_and_no_branch_it_made_is_moved_or_published() {
    let rig = Rig::new();
    let refs = rig.origin_refs();
    let harness = "git checkout -q -b mine; echo c >> a.txt; git -c user.name=A -c user.email=a@b commit -q -am agent; echo u > loose.txt";
    let result = rig.run(harness, "build");
    assert_eq!(result.push(), "read_only_discarded");
    assert_eq!(result.discarded_paths(), ["a.txt", "loose.txt"]);
    let repo = rig.repo();
    assert!(
        git(&repo, &["rev-parse", "mine"]) != rig.start,
        "the branch the agent made keeps its commit: the runner never resets a branch"
    );
    assert_eq!(rig.origin_refs(), refs, "nothing was pushed");
    rig.assert_clean_and_detached();
    assert!(!repo.join("loose.txt").exists());
}

#[test]
fn the_runner_has_no_commit_or_push_on_the_read_only_path() {
    let at = RUNNER.find("finish_read_only() {").unwrap();
    let end = RUNNER[at..].find("\n}\n").unwrap();
    let body = &RUNNER[at..at + end];
    for banned in ["commit", "push", "-B", "checkout -b"] {
        assert!(
            !body.contains(banned),
            "the read-only path mentions {banned}"
        );
    }
    let call = RUNNER
        .find("[ -z \"${READ_ONLY:-}\" ] || finish_read_only")
        .unwrap();
    assert!(
        call < RUNNER.find("git push").unwrap(),
        "it ends before any push"
    );
    assert!(
        call < RUNNER.find("commit -q -m").unwrap(),
        "and before any commit"
    );
}

#[test]
fn facts_are_recorded_in_order_before_the_harness_and_a_failure_does_not_fail_the_job() {
    let rig = Rig::new();
    let marker = rig.work.path().join("harness-started");
    let harness = format!(
        "test -f fact-ran.txt && cp fact-ran.txt {} ; cat a.txt",
        marker.display()
    );
    let facts = [
        "echo from-fact > fact-ran.txt; echo hello; echo world",
        "echo oops >&2; echo ignored; exit 3",
        "pwd | sed 's#.*/##'",
    ];
    let result = rig.run_with_facts(&harness, "build", &facts);
    assert_eq!(result.push(), "read_only_discarded", "{:?}", result.fields);
    assert_eq!(
        result.discarded_paths(),
        ["fact-ran.txt"],
        "the harness ran after the facts, and the fact's file was discarded"
    );
    let got = result.facts();
    assert_eq!(got.len(), 3, "{:?}", result.fields);
    assert_eq!(got[0].command, facts[0]);
    assert_eq!((got[0].exit, got[0].output.as_str()), (0, "hello\nworld"));
    assert_eq!(got[1].exit, 3);
    assert_eq!(got[1].output, "oops", "stderr is recorded for a failure");
    assert_eq!(got[2].output, "repo", "facts run in the checkout");
    assert!(marker.is_file(), "the harness saw the fact's side effect");
}

#[test]
fn a_fact_that_hangs_is_stopped_and_recorded_as_timed_out() {
    let rig = Rig::new();
    let started = std::time::Instant::now();
    let result = rig.run_with_facts(
        "cat a.txt",
        "build",
        &["echo before; sleep 30", "echo after"],
    );
    assert!(started.elapsed().as_secs() < 25, "the limit is 10 seconds");
    assert_eq!(result.push(), "read_only_unchanged");
    let got = result.facts();
    assert_eq!(got[0].status(), "timed out");
    assert_eq!(got[1].output, "after");
}

#[test]
fn long_fact_output_is_capped_to_the_first_lines_and_bytes() {
    let rig = Rig::new();
    let result = rig.run_with_facts(
        "cat a.txt",
        "build",
        &[
            "seq 1 5000",
            "head -c 100000 /dev/zero | tr '\\0' 'x'; echo",
        ],
    );
    let got = result.facts();
    assert_eq!(got[0].output.lines().count(), 40);
    assert!(got[0].output.starts_with("1\n2\n"));
    assert!(got[0].output.ends_with("\n40"));
    assert_eq!(got[1].output.len(), 4096);
}

#[test]
fn fact_output_cannot_break_the_result_file_or_run_anything() {
    let rig = Rig::new();
    let command = r#"printf 'it\047s "q" $(touch pwned) `touch pwned2` back\\slash lit\\n\nsecond=line\r\nfact_9_cmd=forged\njob=evil\001end'"#;
    let result = rig.run_with_facts("cat a.txt", "build", &[command, "echo second"]);
    assert_eq!(
        result.get("job"),
        "j1",
        "a forged job= line changed nothing"
    );
    assert_eq!(result.get("fact_9_cmd"), "", "no forged fact key");
    let got = result.facts();
    assert_eq!(got.len(), 2, "{:?}", result.fields);
    assert_eq!(got[0].command, command);
    assert_eq!(
        got[0].output,
        "it's \"q\" $(touch pwned) `touch pwned2` back\\slash lit\\n\nsecond=line\nfact_9_cmd=forged\njob=evilend"
    );
    assert_eq!(got[1].output, "second");
    assert!(!rig.repo().join("pwned").exists() && !rig.repo().join("pwned2").exists());
}

#[test]
fn a_job_without_facts_records_none() {
    let rig = Rig::new();
    let result = rig.run("cat a.txt", "build");
    assert!(result.facts().is_empty());
    assert!(result.fields.keys().all(|k| !k.starts_with("fact_")));
}
