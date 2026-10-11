use super::*;

fn sample() -> PodName {
    PodName {
        mailspace: "my-space".into(),
        pool: "gpu-big".into(),
        index: 2,
        task: "3fa9c1d2".into(),
        claimed: 1_790_000_000,
        deadline: 1_790_003_900,
        nonce: "0123456789ab".into(),
    }
}

#[test]
fn a_name_round_trips_even_when_the_slugs_hold_dashes() {
    let name = sample().encode().unwrap();
    assert_eq!(
        name,
        "vivi-rp1-my-space-gpu-big-8-2-3fa9c1d2-1790000000-1790003900-0123456789ab"
    );
    assert_eq!(PodName::decode(&name), Some(sample()));
}

#[test]
fn the_mailspace_length_is_what_splits_the_slugs() {
    // The same text read with a different mailspace length is a different pool.
    let mut other = sample();
    other.mailspace = "my".into();
    other.pool = "space-gpu-big".into();
    let name = other.encode().unwrap();
    let back = PodName::decode(&name).unwrap();
    assert_eq!(
        (back.mailspace.as_str(), back.pool.as_str()),
        ("my", "space-gpu-big")
    );
    assert_ne!(name, sample().encode().unwrap());
}

#[test]
fn ownership_needs_both_the_mailspace_and_the_pool() {
    let name = sample();
    assert!(name.is_of(&PoolId::new("My Space", "gpu-big")));
    assert!(!name.is_of(&PoolId::new("my-space", "gpu")));
    assert!(!name.is_of(&PoolId::new("my", "space-gpu-big")));
}

#[test]
fn a_task_handle_that_is_not_a_plain_word_is_hashed() {
    assert_eq!(task_field("3fa9c1d2"), "3fa9c1d2");
    let hashed = task_field("<a.b@host>");
    assert!(hashed.starts_with('x') && hashed.len() == 11);
    assert_eq!(hashed, task_field("<a.b@host>"));
    assert_ne!(hashed, task_field("<a.c@host>"));
    assert!(task_field(&"a".repeat(30)).starts_with('x'));
    assert!(is_word(&hashed, MAX_TASK_LEN));
}

#[test]
fn hostile_names_do_not_decode() {
    let good = sample().encode().unwrap();
    let cases = [
        String::new(),
        "vivi-rp1-".to_string(),
        "vivi-rp2-my-space-gpu-big-8-2-3fa9c1d2-1790000000-1790003900-0123456789ab".to_string(),
        good.replace("-8-2-", "-80-2-"),
        good.replace("-8-2-", "-x-2-"),
        good.replace("-0123456789ab", "-0123456789AB"),
        good.replace("-0123456789ab", "-0123456789a"),
        good.replace("1790000000", "17900000"),
        good.replace("-2-3fa9", "--3fa9"),
        good.replace("3fa9c1d2", "3fa9;c1d2"),
        format!("{good}-extra"),
        good.replace("my-space", "my space"),
        good.replace("-8-", "-0-"),
        "vivi-rp1--gpu-0-1-t-1790000000-1790003900-0123456789ab".to_string(),
        format!("vivi-rp1-{}", "-".repeat(300)),
        good.replace('-', "_"),
        format!("{good}\n"),
    ];
    for name in cases {
        assert_eq!(PodName::decode(&name), None, "{name:?}");
    }
}

#[test]
fn a_part_outside_the_layout_is_refused_when_encoding() {
    for change in [
        |n: &mut PodName| n.mailspace = "My Space".into(),
        |n: &mut PodName| n.pool = "a;rm".into(),
        |n: &mut PodName| n.task = "a-b".into(),
        |n: &mut PodName| n.nonce = "xyz".into(),
        |n: &mut PodName| n.claimed = 12,
        |n: &mut PodName| n.index = 1000,
        |n: &mut PodName| n.mailspace = String::new(),
    ] {
        let mut name = sample();
        change(&mut name);
        assert!(name.encode().is_err(), "{name:?}");
    }
}

#[test]
fn a_name_over_the_provider_limit_is_refused() {
    let mut name = sample();
    name.pool = "p".repeat(190);
    let err = name.encode().unwrap_err().to_string();
    assert!(err.contains("at most 191"), "{err}");
    assert!(check_fits(&PoolId::new("ms", &"p".repeat(150))).is_err());
    assert!(check_fits(&PoolId::new("my-space", "gpu-big")).is_ok());
}

#[test]
fn nonces_are_twelve_hex_digits_and_differ() {
    let (a, b) = (new_nonce(), new_nonce());
    assert!(is_hex(&a, NONCE_LEN) && is_hex(&b, NONCE_LEN));
    assert_ne!(a, b);
}

#[test]
fn the_longest_name_still_fits() {
    let id = PoolId::new(&"m".repeat(40), &"p".repeat(40));
    assert!(check_fits(&id).is_ok());
}

#[test]
fn signs_and_leading_zeros_are_not_numbers() {
    let good = sample().encode().unwrap();
    for name in [
        good.replace("-8-2-", "-+8-2-"),
        good.replace("-8-2-", "-8-02-"),
        good.replace("-8-2-", "-8-+2-"),
        good.replace("1790003900", "01790003900"),
    ] {
        assert_eq!(PodName::decode(&name), None, "{name:?}");
    }
}
