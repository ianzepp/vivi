use super::*;

fn pod(id: &str, index: u32, created: Option<i64>) -> Entry {
    Entry {
        id: id.to_string(),
        index,
        created_ms: created,
    }
}

#[test]
fn a_lone_pod_keeps_its_slot() {
    let me = pod("a", 1, Some(10));
    assert_eq!(verdict(&me, &[me.clone()], 2), Verdict::Keep);
}

#[test]
fn of_two_pods_with_one_index_the_one_made_first_keeps_it() {
    let (a, b) = (pod("a", 1, Some(100)), pod("b", 1, Some(105)));
    let seen = [a.clone(), b.clone()];
    assert_eq!(verdict(&a, &seen, 3), Verdict::Keep);
    assert_eq!(
        verdict(&b, &seen, 3),
        Verdict::IndexTaken { by: "a".into() }
    );
}

#[test]
fn the_same_creation_time_is_broken_by_id_so_both_agree() {
    let (a, b) = (pod("a", 1, Some(100)), pod("b", 1, Some(100)));
    let seen = [a.clone(), b.clone()];
    assert_eq!(verdict(&a, &seen, 3), Verdict::Keep);
    assert_eq!(
        verdict(&b, &seen, 3),
        Verdict::IndexTaken { by: "a".into() }
    );
}

#[test]
fn the_pool_never_keeps_more_pods_than_its_slots() {
    let pods: Vec<Entry> = (1..=3)
        .map(|i| pod(&format!("p{i}"), i, Some(i64::from(i))))
        .collect();
    // slots = 2: the third pod made is over the cap, the first two stay.
    assert_eq!(verdict(&pods[0], &pods, 2), Verdict::Keep);
    assert_eq!(verdict(&pods[1], &pods, 2), Verdict::Keep);
    assert_eq!(verdict(&pods[2], &pods, 2), Verdict::OverCap { before: 2 });
}

#[test]
fn a_pod_that_cannot_be_ordered_gives_way_when_it_could_matter() {
    let (mine, same_index) = (pod("a", 1, Some(100)), pod("b", 1, None));
    assert_eq!(
        verdict(&mine, &[mine.clone(), same_index], 3),
        Verdict::Unordered { with: "b".into() }
    );
    // Another index and room to spare: it cannot win anything from us.
    let other = pod("c", 2, None);
    assert_eq!(
        verdict(&mine, &[mine.clone(), other.clone()], 3),
        Verdict::Keep
    );
    // ... but with the pool already full without us, it might.
    assert_eq!(
        verdict(&mine, &[mine.clone(), other], 1),
        Verdict::Unordered { with: "c".into() }
    );
    // A pod with no time of its own cannot be ordered against anyone.
    let blind = pod("d", 1, None);
    let rival = pod("e", 1, Some(1));
    assert!(matches!(
        verdict(&blind, &[blind.clone(), rival], 3),
        Verdict::Unordered { .. }
    ));
}

#[test]
fn the_lowest_free_index_skips_the_taken_ones() {
    assert_eq!(lowest_free(&[]), 1);
    assert_eq!(lowest_free(&[1, 2, 4]), 3);
    assert_eq!(lowest_free(&[2]), 1);
}

#[test]
fn reasons_are_plain_and_a_kept_pod_has_none() {
    assert_eq!(Verdict::Keep.reason(1, 2), None);
    let text = Verdict::IndexTaken { by: "x".into() }.reason(1, 2).unwrap();
    assert!(text.contains("slot 1") && text.contains('x'));
    assert!(
        Verdict::OverCap { before: 2 }
            .reason(3, 2)
            .unwrap()
            .contains("allows 2")
    );
}

/// The race of the design, played out. Two dispatchers pick slot 1 and make a
/// pod each; the provider orders them by creation. Whatever either sees, at
/// most one of them stays once the later pod can see the earlier one.
#[test]
fn a_simulated_race_leaves_exactly_one_winner_once_both_are_listed() {
    for (a_made, b_made) in [(100, 105), (105, 100), (100, 100)] {
        let a = pod("pod-a", 1, Some(a_made));
        let b = pod("pod-b", 1, Some(b_made));
        // First look of each, before the other is listed: both keep (the
        // known gap the second look exists to close).
        assert_eq!(verdict(&a, &[a.clone()], 3), Verdict::Keep);
        assert_eq!(verdict(&b, &[b.clone()], 3), Verdict::Keep);
        // Second look, with both listed: exactly one keeps.
        let seen = [a.clone(), b.clone()];
        let kept = [&a, &b]
            .into_iter()
            .filter(|p| verdict(p, &seen, 3) == Verdict::Keep)
            .count();
        assert_eq!(kept, 1, "made at {a_made} and {b_made}");
    }
}
