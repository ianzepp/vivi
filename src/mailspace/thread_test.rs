use super::*;

#[test]
fn strips_stacked_reply_prefixes() {
    assert_eq!(strip_reply_prefix("Re: Re[2]: status"), "status");
}

#[test]
fn reply_prefix_is_case_insensitive() {
    assert_eq!(strip_reply_prefix("rE: Status"), "status");
}

fn star(children: usize) -> BTreeMap<String, MailspaceLink> {
    (0..children)
        .map(|n| {
            let child = format!("child-{n}");
            let link = MailspaceLink {
                child_content_id: child.clone(),
                parent_content_id: "seed".into(),
                source: "captured".into(),
            };
            (child, link)
        })
        .collect()
}

#[test]
fn the_limit_caps_the_thread_even_when_neighbours_are_queued() {
    let links = star(5);

    for limit in [1, 2, 3] {
        let included = connected_content_ids("seed", &links, limit, 50);
        assert_eq!(included.len(), limit, "limit {limit}: {included:?}");
        assert!(included.contains("seed"), "the seed is always first");
    }
    assert_eq!(connected_content_ids("seed", &links, 6, 50).len(), 6);
    assert_eq!(connected_content_ids("seed", &links, 50, 50).len(), 6);
    assert!(connected_content_ids("seed", &links, 0, 50).is_empty());
}
