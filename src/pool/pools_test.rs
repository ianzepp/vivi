use super::*;
use crate::pool::fake_provider::Fake;
use tempfile::TempDir;

const DOCKER: &str = "provider = \"docker\"\nslots = 4\nimage = \"ubuntu:24.04\"\n";
const SSH: &str = "provider = \"ssh\"\nslots = 2\n[pools.POOL.ssh]\nhost = \"box\"\n";
const FLY: &str =
    "provider = \"fly\"\nslots = 32\nimage = \"debian:12\"\n[pools.POOL.fly]\napp = \"some-app\"\n";

/// A mailspace whose config holds `tables`.
fn space_with(tables: &str) -> (TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let path = tmp.path().join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(tables);
    std::fs::write(&path, text).unwrap();
    let space = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    (tmp, space)
}

fn table(name: &str, body: &str) -> String {
    format!("\n[pools.{name}]\n{}\n", body.replace("POOL", name))
}

fn two() -> String {
    format!("{}{}", table("local", DOCKER), table("lanes", FLY))
}

fn names(handles: &[PoolHandle]) -> Vec<&str> {
    handles.iter().map(|h| h.id.pool.as_str()).collect()
}

#[test]
fn several_pools_of_every_provider_parse_each_complete_on_its_own() {
    let tables = format!(
        "{}{}{}",
        table("local", DOCKER),
        table("lanes", FLY),
        table("box", SSH)
    );
    let (_tmp, space) = space_with(&tables);
    let all = configured(&space).unwrap();
    assert_eq!(
        all.keys().map(String::as_str).collect::<Vec<_>>(),
        ["box", "lanes", "local"]
    );
    assert_eq!(all["local"].provider, "docker");
    assert_eq!(all["local"].slots, 4);
    assert_eq!(all["lanes"].provider, "fly");
    assert_eq!(all["lanes"].slots, 32);
    assert_eq!(all["lanes"].fly.app.as_deref(), Some("some-app"));
    assert_eq!(all["box"].ssh.host.as_deref(), Some("box"));
    let handles = open(&space, None, Need::Any).unwrap();
    assert_eq!(names(&handles), ["box", "lanes", "local"]);
}

#[test]
fn nothing_is_inherited_between_pools() {
    // `lanes` sets no image of its own; it does not borrow `local`'s.
    let tables = format!(
        "{}{}",
        table("local", DOCKER),
        table("lanes", "provider = \"docker\"\nslots = 2\n")
    );
    let (_tmp, space) = space_with(&tables);
    let said = open(&space, None, Need::Any)
        .map(drop)
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("[pools.lanes]") && said.contains("image"),
        "{said}"
    );
}

#[test]
fn the_retired_single_pool_table_is_rejected_naming_the_new_form() {
    let (_tmp, space) = space_with("\n[pool]\nrepo = \"o/r\"\nslots = 1\n");
    let said = configured(&space).unwrap_err().to_string();
    assert!(
        said.contains("[pools.<name>]") && said.contains("[pool]"),
        "{said}"
    );
    let even_with_new = format!("\n[pool]\nslots = 1\n{}", table("local", DOCKER));
    let (_tmp2, mixed) = space_with(&even_with_new);
    assert!(
        configured(&mixed).is_err(),
        "the old table is never ignored"
    );
}

#[test]
fn the_retired_table_survives_a_config_rewrite_untouched() {
    let (_tmp, mut space) = space_with("\n[pool]\nslots = 1\n");
    space.add_identity("mind").unwrap();
    let again = Mailspace::open_root(space.root.clone()).unwrap();
    assert!(again.config.retired_pool.is_some(), "not erased");
}

#[test]
fn pool_tables_survive_a_config_rewrite() {
    let (_tmp, mut space) = space_with(&two());
    space.add_identity("mind").unwrap();
    let again = Mailspace::open_root(space.root.clone()).unwrap();
    assert_eq!(again.config.pools.len(), 2);
    assert_eq!(
        again.config.pools["lanes"].fly.app.as_deref(),
        Some("some-app")
    );
    assert_eq!(again.config.pools["local"].image, "ubuntu:24.04");
}

#[test]
fn no_pool_table_is_an_error_that_shows_the_form() {
    let (_tmp, space) = space_with("");
    let said = configured(&space).unwrap_err().to_string();
    assert!(said.contains("[pools.<name>]"), "{said}");
    assert!(
        open_for_collect(&space, None).unwrap().is_empty(),
        "wait and reconcile still run"
    );
    assert!(open_for_collect(&space, Some("x")).is_err());
}

#[test]
fn a_fly_pool_without_an_app_is_refused_when_opened() {
    let no_app = FLY.replace("[pools.POOL.fly]\napp = \"some-app\"\n", "");
    let (_tmp, space) = space_with(&table("lanes", &no_app));
    let said = open(&space, None, Need::Any)
        .map(drop)
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("[pools.lanes]") && said.contains("fly.app is required"),
        "{said}"
    );
}

#[test]
fn bad_pool_names_are_refused_when_opened() {
    for bad in ["Local", "a_b", "x-doctor", "-x"] {
        let (_tmp, space) = space_with(&format!("\n[pools.\"{bad}\"]\n{DOCKER}"));
        let said = open(&space, None, Need::Any)
            .map(drop)
            .unwrap_err()
            .to_string();
        assert!(said.contains("pool name"), "{bad}: {said}");
    }
}

#[test]
fn a_provider_only_pool_opens_without_a_repository() {
    let (_tmp, space) = space_with(&table("local", DOCKER));
    let handles = open(&space, None, Need::One).unwrap();
    assert!(handles[0].config.repo_access().is_none());
}

#[test]
fn one_pool_needs_no_flag() {
    let names = ["local"];
    for need in [Need::One, Need::Any] {
        assert_eq!(select(&names, None, need).unwrap(), ["local"]);
        assert_eq!(select(&names, Some("local"), need).unwrap(), ["local"]);
    }
}

#[test]
fn several_pools_require_the_flag_for_the_commands_that_act_on_one() {
    let names = ["lanes", "local"];
    let said = select(&names, None, Need::One).unwrap_err().to_string();
    assert!(
        said.contains("lanes, local") && said.contains("--pool"),
        "{said}"
    );
    assert_eq!(select(&names, Some("lanes"), Need::One).unwrap(), ["lanes"]);
}

#[test]
fn several_pools_cover_all_of_them_without_the_flag_for_the_read_commands() {
    let names = ["lanes", "local"];
    assert_eq!(select(&names, None, Need::Any).unwrap(), ["lanes", "local"]);
    assert_eq!(select(&names, Some("local"), Need::Any).unwrap(), ["local"]);
}

#[test]
fn an_unknown_pool_lists_the_known_ones() {
    let said = select(&["lanes", "local"], Some("nope"), Need::Any)
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("unknown pool \"nope\"") && said.contains("lanes, local"),
        "{said}"
    );
}

#[test]
fn pool_identity_uses_the_mailspace_id() {
    let (_tmp, mut space) = space_with(&two());
    space.config.id = Some("aeff60b0-a1c4-4eaa-b688-7afcff51c659".into());
    for handle in open_unvalidated(&space, None, Need::Any).unwrap() {
        assert_eq!(
            handle.id.mailspace, "aeff60b0-a1c4-4eaa-b688-7afcff51c659",
            "the pool of mailspace id is keyed by the id, not the name slug"
        );
    }
}

#[test]
fn pool_identity_falls_back_to_the_name_slug() {
    let (_tmp, mut space) = space_with(&two());
    space.config.id = None;
    for handle in open_unvalidated(&space, None, Need::Any).unwrap() {
        assert_eq!(
            handle.id.mailspace,
            crate::pool::slot::space_slug(&space.config.name)
        );
    }
}

#[test]
fn open_one_and_open_apply_the_same_rules_to_a_real_mailspace() {
    let (_tmp, space) = space_with(&two());
    assert!(
        open_one(&space, None)
            .map(drop)
            .unwrap_err()
            .to_string()
            .contains("--pool")
    );
    assert_eq!(open_one(&space, Some("local")).unwrap().id.pool, "local");
    assert_eq!(open(&space, Some("local"), Need::Any).unwrap().len(), 1);
    assert_eq!(open(&space, None, Need::Any).unwrap().len(), 2);
    // Identity: the mailspace id (not the name slug) is part of every
    // pool's id.
    let id = open_one(&space, Some("local")).unwrap().id;
    assert_eq!(id.mailspace, space.config.id.as_deref().unwrap());
}

#[test]
fn a_pool_set_covers_its_members_and_an_all_set_also_the_removed() {
    let (a, b) = (Fake::default(), Fake::default());
    let (ia, ib) = (PoolId::new("ms", "a"), PoolId::new("ms", "b"));
    let one = PoolSet::single(&ia, &a);
    assert!(one.covers("a") && !one.covers("b") && !one.covers("gone"));
    assert!(one.find("a").is_some() && one.find("b").is_none());
    let all = PoolSet {
        members: vec![
            Member {
                id: &ia,
                provider: &a,
            },
            Member {
                id: &ib,
                provider: &b,
            },
        ],
        all: true,
    };
    assert!(all.covers("a") && all.covers("b") && all.covers("gone"));
}
