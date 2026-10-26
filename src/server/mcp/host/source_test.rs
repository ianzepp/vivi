use super::check;

#[test]
fn github_urls_are_accepted() {
    for source in [
        "https://github.com/ianzepp/vivi",
        "https://github.com/ianzepp/vivi.git",
        "https://github.com/a_b-c.d/r.e_p-o.git",
    ] {
        assert!(check(source).is_ok(), "{source:?} should be accepted");
    }
}

#[test]
fn every_refused_form_is_refused() {
    for source in [
        "/mnt/vivi/lanes/faberlang/radix.git",
        "file:///mnt/vivi/lanes/faberlang/radix.git",
        "ssh://git@github.com/owner/repo.git",
        "git@github.com:owner/repo.git",
        "https://gitlab.com/owner/repo.git",
        "https://user@github.com/owner/repo.git",
        "https://github.com/owner/repo?ref=main",
        "https://github.com/owner/repo#main",
        "https://github.com/owner/repo/",
        "https://github.com/owner/repo/extra",
        "https://github.com/owner",
        "https://github.com//repo",
        "https://github.com:443/owner/repo.git",
        "",
        " https://github.com/owner/repo",
        "https://github.com/owner/repo.git ",
    ] {
        assert!(check(source).is_err(), "{source:?} should be refused");
    }
}

#[test]
fn the_refusal_states_the_rule() {
    let error = match check("https://gitlab.com/owner/repo.git") {
        Ok(()) => panic!("expected a refusal"),
        Err(error) => error,
    };
    let message = error.message;
    assert!(message.contains("source"), "{message}");
    assert!(message.contains("https://github.com/"), "{message}");
    assert!(message.contains(r"^https://github\.com/"), "{message}");
    assert!(!message.contains('\n'), "{message}");
}
