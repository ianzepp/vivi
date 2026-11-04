use super::*;

fn worker(text: &str) -> PoolWorker {
    toml::from_str(text).unwrap()
}

#[test]
fn a_complete_table_validates_and_renders_its_request() {
    let w = worker("host = \"studio\"\nkind = \"apfs\"\nsize_gib = 12\n");
    w.validate("p").unwrap();
    assert_eq!(w.request(), "kind=apfs size_gib=12");
    assert_eq!(w.root_or_default(), DEFAULT_WORKER_ROOT);
}

#[test]
fn a_dir_boundary_needs_no_size() {
    worker("host = \"studio\"\nkind = \"dir\"\n")
        .validate("p")
        .unwrap();
}

#[test]
fn host_kind_and_size_are_checked() {
    let bad_values = [
        (
            "host = \"studio\"\nkind = \"tmpfs\"\n",
            "kind must be one of",
        ),
        ("host = \"studio\"\nkind = \"dir\"\nsize = 3\n", "size"),
    ];
    for (text, want) in bad_values {
        let said = worker(text).validate("p").unwrap_err().to_string();
        assert!(said.contains(want), "{text:?}: {said}");
    }
    let absent = [
        ("kind = \"dir\"\n", "host is required"),
        ("host = \"studio\"\n", "kind is required"),
        (
            "host = \"studio\"\nkind = \"ram\"\n",
            "size_gib is required",
        ),
        (
            "host = \"studio\"\nkind = \"apfs\"\nsize_gib = 0\n",
            "size_gib is required",
        ),
    ];
    for (text, want) in absent {
        let config = worker(text);
        config.validate("p").unwrap();
        let mut said = Vec::new();
        config.missing(&mut |_, message| said.push(message.to_string()));
        assert!(said.iter().any(|m| m.contains(want)), "{text:?}: {said:?}");
    }
}
