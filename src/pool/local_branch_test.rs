use super::*;
use crate::pool::local_fixture::{fixture, git};

const TIP: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

fn refs(from: &str, branch: &str) -> Refs {
    Refs {
        from: from.into(),
        branch: branch.into(),
    }
}

/// A repository where `main` is the default and the primary branch, `topic`
/// does not exist, and `from` names a commit.
fn facts() -> Facts {
    Facts {
        repo: PathBuf::from("/repo"),
        branch_tip: None,
        from_commit: Some(TIP.into()),
        default_branch: Some("main".into()),
        primary: Some((PathBuf::from("/repo"), "main".into())),
        holder: None,
        allow_default: false,
    }
}

fn refused(refs: &Refs, facts: &Facts) -> String {
    decide(refs, facts).unwrap_err().to_string()
}

#[test]
fn a_new_branch_is_created_at_the_commit_from_names() {
    let got = decide(&refs("abc123", "topic"), &facts()).unwrap();
    assert_eq!(got.action, BranchAction::Create);
    assert_eq!(got.start, TIP);
}

#[test]
fn a_stack_creates_the_new_branch_at_the_tip_of_the_other_branch() {
    let mut f = facts();
    f.from_commit = Some(OTHER.into());
    let got = decide(&refs("base-branch", "topic"), &f).unwrap();
    assert_eq!(
        (got.action, got.start.as_str()),
        (BranchAction::Create, OTHER)
    );
}

#[test]
fn an_existing_branch_is_continued_as_it_is_when_from_is_that_branch() {
    let mut f = facts();
    f.branch_tip = Some(OTHER.into());
    f.from_commit = Some(OTHER.into());
    let got = decide(&refs("topic", "topic"), &f).unwrap();
    assert_eq!(got.action, BranchAction::Continue);
    assert_eq!(got.start, OTHER, "it starts at the branch's own tip");
}

#[test]
fn an_existing_branch_with_any_other_from_is_refused_and_never_reset() {
    let mut f = facts();
    f.branch_tip = Some(OTHER.into());
    for from in ["main", "abc123", "topic2", "refs/heads/topic"] {
        let said = refused(&refs(from, "topic"), &f);
        assert!(said.contains("already exists"), "{said}");
        assert!(said.contains("refusing to move or reset"), "{said}");
        assert!(said.contains("--from topic"), "names the way out: {said}");
    }
}

#[test]
fn from_naming_nothing_is_refused() {
    let mut f = facts();
    f.from_commit = None;
    assert!(refused(&refs("nope", "topic"), &f).contains("not a commit or branch"));
}

#[test]
fn continuing_a_branch_that_does_not_exist_is_refused() {
    let said = refused(&refs("topic", "topic"), &facts());
    assert!(said.contains("does not exist"), "{said}");
}

#[test]
fn the_default_branch_is_refused_unless_the_override_is_given() {
    let mut f = facts();
    f.primary = None;
    f.branch_tip = Some(OTHER.into());
    let said = refused(&refs("main", "main"), &f);
    assert!(said.contains("default branch \"main\""), "{said}");
    assert!(said.contains("--allow-default-branch"), "{said}");
    f.allow_default = true;
    let got = decide(&refs("main", "main"), &f).unwrap();
    assert_eq!(got.action, BranchAction::Continue);
}

#[test]
fn the_primary_checkouts_branch_is_refused_unless_the_override_is_given() {
    let mut f = facts();
    f.default_branch = Some("trunk".into());
    f.primary = Some((PathBuf::from("/repo"), "work".into()));
    f.branch_tip = Some(OTHER.into());
    let said = refused(&refs("work", "work"), &f);
    assert!(said.contains("primary checkout /repo"), "{said}");
    f.allow_default = true;
    assert!(decide(&refs("work", "work"), &f).is_ok());
}

#[test]
fn a_branch_held_by_a_worktree_is_refused_naming_its_path_override_or_not() {
    let mut f = facts();
    f.branch_tip = Some(OTHER.into());
    f.holder = Some(PathBuf::from("/elsewhere/tree"));
    for allow in [false, true] {
        f.allow_default = allow;
        let said = refused(&refs("topic", "topic"), &f);
        assert!(said.contains("checked out in /elsewhere/tree"), "{said}");
    }
    // A branch that does not exist yet can still be held (an unborn branch).
    f.branch_tip = None;
    assert!(refused(&refs("main", "topic"), &f).contains("/elsewhere/tree"));
}

#[test]
fn the_primary_branch_with_the_override_is_still_refused_as_held() {
    let mut f = facts();
    f.branch_tip = Some(OTHER.into());
    f.allow_default = true;
    f.holder = Some(PathBuf::from("/repo"));
    assert!(refused(&refs("main", "main"), &f).contains("checked out in /repo"));
}

#[test]
fn prepare_reads_a_real_repository_and_applies_the_table() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let main_tip = git(&f.repo, &["rev-parse", "main"]);
    let got = prepare(&repo, &refs("main", "topic"), false).unwrap();
    assert_eq!(
        (got.action, got.start.as_str()),
        (BranchAction::Create, main_tip.as_str())
    );
    // Neither branch is made by looking.
    assert_eq!(git(&f.repo, &["branch", "--list", "topic"]), "");

    git(&f.repo, &["branch", "topic"]);
    let said = prepare(&repo, &refs("main", "topic"), false)
        .unwrap_err()
        .to_string();
    assert!(said.contains("already exists"), "{said}");
    let got = prepare(&repo, &refs("topic", "topic"), false).unwrap();
    assert_eq!(got.action, BranchAction::Continue);

    let said = prepare(&repo, &refs("topic", "main"), false)
        .unwrap_err()
        .to_string();
    assert!(said.contains("default branch"), "{said}");

    let held = f.dir.path().join("held");
    git(
        &f.repo,
        &["worktree", "add", "-q", held.to_str().unwrap(), "topic"],
    );
    let said = prepare(&repo, &refs("topic", "topic"), false)
        .unwrap_err()
        .to_string();
    assert!(
        said.contains(&format!(
            "checked out in {}",
            held.canonicalize().unwrap().display()
        )),
        "{said}"
    );
}

#[test]
fn the_base_commit_is_the_requested_one_else_the_default_branch_tip() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let main_tip = git(&f.repo, &["rev-parse", "main"]);
    let first = git(&f.repo, &["rev-parse", "main~1"]);
    assert_eq!(base_commit(&repo, None).unwrap(), main_tip);
    assert_eq!(base_commit(&repo, Some("main~1")).unwrap(), first);
    assert!(
        base_commit(&repo, Some("nope"))
            .unwrap_err()
            .to_string()
            .contains("not a commit")
    );
}
