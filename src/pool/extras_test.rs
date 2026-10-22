//! Tests for [`crate::pool::extras`]: every other configured target becomes a
//! read-only checkout, resolved to a commit before any slot is claimed.

use std::path::Path;

use super::*;
use crate::pool::local_fixture::{Fixture, fixture, git};

fn pool_for(remote: &Path) -> PoolConfig {
    toml::from_str(&format!(
        r#"
remote_url = "file://{}"
slots = 1
image = "debian:12"
build_dir = "target"
[harness]
name = "fake"
run = "fake --model {{model}} @{{brief}}"
models = ["m/ok"]
[committer]
name = "bot"
email = "bot@example.com"
"#,
        remote.display()
    ))
    .unwrap()
}

fn pump_for(radix: &Path, gradus: &Path, inferentia: &Path) -> PumpConfig {
    toml::from_str(&format!(
        r#"
[targets.radix]
path = "radix"
remote_url = "file://{}"
branch = "main"
[targets.gradus]
path = "gradus"
remote_url = "file://{}"
branch = "main"
[targets.inferentia]
path = "inferentia"
remote_url = "file://{}"
branch = "main"
"#,
        radix.display(),
        gradus.display(),
        inferentia.display(),
    ))
    .unwrap()
}

struct World {
    radix: Fixture,
    gradus: Fixture,
    inferentia: Fixture,
    pump: PumpConfig,
}

fn world() -> World {
    let (radix, gradus, inferentia) = (fixture(), fixture(), fixture());
    let pump = pump_for(&radix.repo, &gradus.repo, &inferentia.repo);
    World {
        radix,
        gradus,
        inferentia,
        pump,
    }
}

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn a_planner_gets_every_other_target_and_never_its_own_twice() {
    let w = world();
    // The pool's own repository is radix, as on the hub.
    let pool = pool_for(&w.radix.repo);
    let all = ids(&["gradus", "inferentia", "radix"]);

    let extras = resolve(
        "plan",
        &pool,
        &w.pump,
        Path::new("/workspace"),
        None,
        &all,
        &Progress::silent(),
    )
    .unwrap();

    let got: Vec<&str> = extras.iter().map(|e| e.target.id.as_str()).collect();
    assert_eq!(got, ["gradus", "inferentia"], "radix is the job's own");
    assert_eq!(extras[0].from, git(&w.gradus.repo, &["rev-parse", "main"]));
    assert_eq!(
        extras[1].from,
        git(&w.inferentia.repo, &["rev-parse", "main"])
    );
    assert_eq!(extras[0].target.cache_key.len(), 64);
    assert_ne!(extras[0].target.cache_key, extras[1].target.cache_key);
    assert!(extras.iter().all(|e| e.token.expose().is_empty()));
}

#[test]
fn a_job_with_an_explicit_target_skips_it_and_reads_the_rest() {
    let w = world();
    let pool = pool_for(&w.radix.repo);
    let own = target::resolve(&pool, &w.pump, Path::new("/workspace"), "gradus")
        .unwrap()
        .target;
    let all = ids(&["gradus", "gradus", "inferentia", "radix"]);

    let extras = resolve(
        "plan",
        &pool,
        &w.pump,
        Path::new("/workspace"),
        Some(&own),
        &all,
        &Progress::silent(),
    )
    .unwrap();

    let got: Vec<&str> = extras.iter().map(|e| e.target.id.as_str()).collect();
    // Radix is the pool's own default here, but with an explicit primary the
    // job's own source is gradus, so both others are read.
    assert_eq!(got, ["inferentia", "radix"]);
}

#[test]
fn an_unknown_target_is_refused_before_anything_is_claimed() {
    let w = world();
    let pool = pool_for(&w.radix.repo);

    let error = resolve(
        "plan",
        &pool,
        &w.pump,
        Path::new("/workspace"),
        None,
        &ids(&["missing"]),
        &Progress::silent(),
    )
    .err()
    .unwrap()
    .to_string();

    assert!(error.contains("missing"), "{error}");
}
