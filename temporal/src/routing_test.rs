use super::*;

fn hosts(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn template_names_the_host_queue() {
    assert_eq!(queue_for("agent-{host}", "studio"), "agent-studio");
    assert_eq!(queue_for("spike", "studio"), "spike");
}

#[test]
fn preferred_host_goes_first_and_every_host_once() {
    let all = hosts(&["studio", "studio-02", "mini-m1"]);
    for key in ["a", "b", "c", "slice-n1-u1"] {
        let order = hosts_in_order(&all, Some("studio-02"), key);
        assert_eq!(order[0], "studio-02");
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, hosts(&["mini-m1", "studio", "studio-02"]));
    }
}

#[test]
fn unknown_preferred_host_is_ignored() {
    let all = hosts(&["studio", "studio-02"]);
    let order = hosts_in_order(&all, Some("gone"), "k");
    assert_eq!(order.len(), 2);
    assert_eq!(order, hosts_in_order(&all, None, "k"));
}

#[test]
fn order_is_stable_and_spreads_keys() {
    let all = hosts(&["h1", "h2", "h3", "h4"]);
    assert_eq!(
        hosts_in_order(&all, None, "x"),
        hosts_in_order(&all, None, "x")
    );
    let firsts: std::collections::BTreeSet<String> = (0..32)
        .map(|i| hosts_in_order(&all, None, &format!("slice-{i}"))[0].clone())
        .collect();
    assert!(firsts.len() > 1, "every key picked the same first host");
}

#[test]
fn placements_use_affinity_then_fallback_waits() {
    let mut routing = Routing::single("q");
    routing.agent = "agent-{host}".into();
    routing.affinity_wait_ms = 1_000;
    routing.fallback_wait_ms = 9_000;
    let all = hosts(&["studio", "studio-02"]);
    let got = placements(&routing, &routing.agent, &all, Some("studio-02"), "k");
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].queue, "agent-studio-02");
    assert_eq!(got[0].wait, Duration::from_millis(1_000));
    assert_eq!(got[1].queue, "agent-studio");
    assert_eq!(got[1].wait, Duration::from_millis(9_000));
}

#[test]
fn a_lone_host_gets_the_fallback_wait() {
    let mut routing = Routing::single("q");
    routing.affinity_wait_ms = 1_000;
    routing.fallback_wait_ms = 9_000;
    let got = placements(&routing, "agent-{host}", &hosts(&["mini-m1"]), None, "k");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].queue, "agent-mini-m1");
    assert_eq!(got[0].wait, Duration::from_millis(9_000));
}

#[test]
fn review_hosts_default_to_agent_hosts() {
    let mut routing = Routing::single("q");
    routing.agent_hosts = hosts(&["studio", "studio-02"]);
    assert_eq!(routing.review_hosts(), &hosts(&["studio", "studio-02"])[..]);
    routing.review_hosts = hosts(&["mini-m1"]);
    assert_eq!(routing.review_hosts(), &hosts(&["mini-m1"])[..]);
}

#[test]
fn review_on_its_own_hosts_ignores_the_implement_host() {
    let mut routing = Routing::single("q");
    routing.affinity_wait_ms = 1_000;
    routing.fallback_wait_ms = 9_000;
    routing.agent_hosts = hosts(&["studio", "studio-02"]);
    routing.review_hosts = hosts(&["mini-m1", "mini-m2"]);
    // The slice implemented on studio-02; review never goes there, and
    // each slice keeps one stable order over the review hosts.
    for key in ["slice-n1-s1", "slice-n1-s2", "slice-n2-s1"] {
        let got = placements(
            &routing,
            "agent-{host}",
            routing.review_hosts(),
            Some("studio-02"),
            key,
        );
        let queues: Vec<&str> = got.iter().map(|p| p.queue.as_str()).collect();
        assert_eq!(queues.len(), 2);
        assert!(queues.contains(&"agent-mini-m1") && queues.contains(&"agent-mini-m2"));
        assert_eq!(got[0].wait, Duration::from_millis(1_000));
        assert_eq!(got[1].wait, Duration::from_millis(9_000));
        let again = placements(&routing, "agent-{host}", routing.review_hosts(), None, key);
        assert_eq!(got, again);
    }
    // A review host that last held the slice goes first.
    let got = placements(
        &routing,
        "agent-{host}",
        routing.review_hosts(),
        Some("mini-m2"),
        "slice-n1-s1",
    );
    assert_eq!(got[0].host, "mini-m2");
}

#[test]
fn no_hosts_no_placements() {
    let routing = Routing::single("q");
    assert!(placements(&routing, "agent-{host}", &[], None, "k").is_empty());
}
