//! RC3 on a second member (pump-flow plan 3.4(b)). The hub, which holds the
//! pump identity, writes a run through `MailspacePort`: the need, the run
//! sentinel and steps, one `bound` letter naming all of them, the closes with
//! verdicts, and finally the travelling need close. A second member receives
//! those exact letters in an adversarial order and must keep the need open
//! until the sentinel closes, bind a unit whose task letter arrives after the
//! `bound` letter, ignore an absorbed named unit, and in the end fold the need
//! to `done`.

use std::rc::Rc;

use crate::mailspace::{Mailspace, RoleUpdate};
use crate::pump::flow::mail::MailspacePort;
use crate::storage::BACKLOG_GRAPH_CODE;
use crate::sync::fake_server::{Checkout, FakeServer, Item, checkout};
use crate::sync::transport::{Envelope, Transport};

/// The hub holds the pump identity `operator`, an operator-kind role (the
/// shipped pump setup), so it may close for everyone.
fn hub() -> Checkout {
    let mut hub = checkout("hub", &["mind"]);
    hub.mailspace
        .add_role(
            "operator",
            RoleUpdate {
                kind: Some(Some("operator".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    hub
}

fn member() -> Checkout {
    checkout("member", &["mind"])
}

/// The letters the hub put on the wire, by their part in the run.
struct Letters {
    need: Item,
    post: Item,
    bound: Item,
    sentinel: Item,
    pred: Item,
    succ: Item,
    unit: Item,
    done_pred: Item,
    done_succ: Item,
    done_unit: Item,
    done_sentinel: Item,
    done_need: Item,
}

/// The run's handles on the hub (the member holds the same handles).
struct Run {
    need: String,
    sentinel: String,
    pred: String,
    succ: String,
    unit: String,
    letters: Letters,
}

fn header(bytes: &[u8], name: &str) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse(bytes)?;
    parsed
        .headers()
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(name))
        .and_then(|header| match header.value() {
            mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
            _ => None,
        })
}

fn subject(item: &Item) -> String {
    let parsed = mail_parser::MessageParser::default()
        .parse(&item.bytes)
        .unwrap();
    parsed.subject().unwrap_or_default().to_string()
}

/// The one letter on the wire matching `want`.
fn pick(items: &[Item], what: &str, want: impl Fn(&Item) -> bool) -> Item {
    let hits: Vec<&Item> = items.iter().filter(|item| want(item)).collect();
    assert_eq!(hits.len(), 1, "exactly one letter for {what}");
    hits[0].clone()
}

fn event(item: &Item) -> Option<String> {
    header(&item.bytes, "X-Vivi-Event")
}

fn task_letter(items: &[Item], name: &str) -> Item {
    pick(items, name, |item| {
        item.kind.as_deref() == Some("task") && subject(item) == name
    })
}

fn done_letter(items: &[Item], of: &str) -> Item {
    let subject_of = format!("Re: {of}");
    pick(items, &format!("done of {of}"), |item| {
        event(item).as_deref() == Some("done") && subject(item) == subject_of
    })
}

/// The hub writes a whole run the way the flow engine does, then pushes it.
/// Returns the handles and the letters as they sit on the wire.
fn hub_run(hub: &Checkout, to: &str) -> Run {
    let port = MailspacePort {
        mailspace: hub.mailspace.clone(),
        pump_identity: "operator".into(),
    };
    let need = hub.send_need("mind", to, "run the flow");
    port.open_pump_record(&need, "kind: build").unwrap();
    // The run-start post the engine writes: addressed to the need's own
    // recipient set so it reaches every member, the earlier post other
    // members need before they take the pump's close.
    port.post_to_need_audience(&need, "pump: run r1 started (build, entry plan)")
        .unwrap();
    let step = |subject: &str, after: &[String]| {
        port.send_step("mind", to, subject, "step\n", after)
            .unwrap()
    };
    let sentinel = step("sentinel", &[]);
    let pred = step("pred", &[]);
    let succ = step("succ", std::slice::from_ref(&pred));
    let unit = step("unit", &[]);
    port.bind(
        &need,
        &[sentinel.clone(), pred.clone(), succ.clone(), unit.clone()],
    )
    .unwrap();
    for task in [&pred, &succ, &unit] {
        port.close_task(task, "ok", None, None).unwrap();
    }
    // The travelling need close is written before the sentinel closes here:
    // the hub's own join closes the need the moment the sentinel does, and a
    // need that is already done refuses the explicit close.
    port.close_need(&need, "flow finished").unwrap();
    port.close_task(&sentinel, "ok", None, None).unwrap();
    port.close_pump_record(&need, "landed").unwrap();

    let server = FakeServer::new();
    hub.push(&hub.client(&server));
    let items = server.items();
    let letters = Letters {
        need: pick(&items, "the need", |item| {
            item.kind.as_deref() == Some("need")
        }),
        post: pick(&items, "the pump post", |item| {
            event(item).is_none() && subject(item) == "Re: run the flow"
        }),
        bound: pick(&items, "the bound letter", |item| {
            event(item).as_deref() == Some("bound")
        }),
        sentinel: task_letter(&items, "sentinel"),
        pred: task_letter(&items, "pred"),
        succ: task_letter(&items, "succ"),
        unit: task_letter(&items, "unit"),
        done_pred: done_letter(&items, "pred"),
        done_succ: done_letter(&items, "succ"),
        done_unit: done_letter(&items, "unit"),
        done_sentinel: done_letter(&items, "sentinel"),
        done_need: done_letter(&items, "run the flow"),
    };
    Run {
        need,
        sentinel,
        pred,
        succ,
        unit,
        letters,
    }
}

/// Put `letters` on a wire in this order and let `member` pull them.
fn deliver(server: &Rc<FakeServer>, hub: &Checkout, member: &Checkout, letters: &[&Item]) {
    for letter in letters {
        hub.client(server)
            .append(
                &letter.message_id,
                &Envelope {
                    raw_eml: &letter.bytes,
                    to: letter.to.as_deref().unwrap_or(""),
                    kind: letter.kind.as_deref().unwrap_or("mail"),
                },
            )
            .unwrap();
    }
    let pulled = member.pull(&member.client(server));
    assert!(pulled.failure().is_none(), "{:?}", pulled.warnings);
}

fn node_state(mailspace: &Mailspace, handle: &str) -> String {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == handle)
        .unwrap_or_else(|| panic!("no node for {handle}"))
        .state
}

/// The unit source ids bound to `need`'s backlog node here, sorted.
fn bound_units(mailspace: &Mailspace, need: &str) -> Vec<String> {
    let storage = mailspace.storage().unwrap();
    let graph = storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .expect("the backlog graph exists");
    let mut sources: Vec<String> = storage
        .work_graph_nodes_by_subgraph(&graph.handle, need)
        .unwrap()
        .into_iter()
        .map(|node| node.source_id)
        .collect();
    sources.sort();
    sources
}

fn sorted(handles: &[&String]) -> Vec<String> {
    let mut sorted: Vec<String> = handles.iter().map(|handle| (*handle).clone()).collect();
    sorted.sort();
    sorted
}

fn fold_state(mailspace: &Mailspace, need: &str) -> Option<String> {
    mailspace
        .item_event_state(need)
        .unwrap()
        .map(|state| state.state)
}

#[test]
fn the_need_stays_open_until_the_sentinel_closes_whatever_the_letter_order() {
    let hub = hub();
    let member = member();
    let run = hub_run(&hub, &member.address("mind"));
    let l = &run.letters;
    // One wire for the member, so its pull cursor keeps moving forward.
    let wire = FakeServer::new();
    let mine = &member.mailspace;

    // A predecessor's done arrives before its successor's task and before any
    // bound letter.
    deliver(
        &wire,
        &hub,
        &member,
        &[&l.need, &l.post, &l.pred, &l.done_pred],
    );
    assert_eq!(node_state(mine, &run.pred), "done");
    assert_eq!(node_state(mine, &run.need), "open");
    assert!(bound_units(mine, &run.need).is_empty(), "nothing bound yet");

    // The bound letter arrives before the sentinel's, the successor's and the
    // unit's task letters. It binds the one unit that is here and holds the
    // join for the three named units never seen.
    deliver(&wire, &hub, &member, &[&l.bound]);
    assert_eq!(bound_units(mine, &run.need), vec![run.pred.clone()]);
    assert_eq!(
        node_state(mine, &run.need),
        "open",
        "unseen named units hold the join"
    );

    // The successor's and the unit's done arrive ahead of their task letters;
    // nothing is closed or bound on a unit that is not here.
    deliver(&wire, &hub, &member, &[&l.done_succ, &l.done_unit]);
    assert_eq!(node_state(mine, &run.need), "open");

    // The late task letters are bound when they arrive (re-apply).
    deliver(&wire, &hub, &member, &[&l.succ, &l.unit]);
    assert_eq!(
        bound_units(mine, &run.need),
        sorted(&[&run.pred, &run.succ, &run.unit]),
        "the late task letters were bound"
    );
    assert_eq!(
        node_state(mine, &run.need),
        "open",
        "the sentinel's task letter has still not arrived"
    );

    // An absorbed named unit does not hold the join.
    {
        let storage = mine.storage().unwrap();
        let id = storage.resolve_message_token(&run.unit).unwrap();
        let row = storage.message_by_id(&id).unwrap().unwrap();
        assert!(
            storage
                .absorb_message(&row.account, &row.message_id, &row.account)
                .unwrap()
        );
    }

    // The sentinel's task letter arrives last; it is bound, and being open it
    // keeps the need open.
    deliver(&wire, &hub, &member, &[&l.sentinel]);
    assert!(bound_units(mine, &run.need).contains(&run.sentinel));
    assert_eq!(node_state(mine, &run.sentinel), "open");
    assert_eq!(
        node_state(mine, &run.need),
        "open",
        "the open sentinel holds the need"
    );
    assert_eq!(fold_state(mine, &run.need), None, "no need close yet");

    // The sentinel closes; the join fires. Then the travelling need close.
    deliver(&wire, &hub, &member, &[&l.done_sentinel]);
    assert_eq!(node_state(mine, &run.sentinel), "done");
    assert_eq!(node_state(mine, &run.need), "done");
    deliver(&wire, &hub, &member, &[&l.done_need]);
    assert_eq!(fold_state(mine, &run.need).as_deref(), Some("done"));
}

/// A need the member authored and delegated to the hub, which runs the pump
/// as `pump` (not the operator role): the second member takes the pump's
/// close only after the pump has posted on the need's thread. The hub opens
/// its record, posts (when `post`) and closes the need; the member receives
/// the letters and returns the need's folded state there.
fn member_authored_close(post: bool) -> Option<String> {
    // Both members share the project's `[pump] from`.
    let pump_config = || {
        Some(crate::pump::config::PumpConfig {
            from: "pump".into(),
            ..crate::pump::config::PumpConfig::default()
        })
    };
    let mut hub = checkout("hub", &["mind", "pump"]);
    hub.mailspace.config.pump = pump_config();
    let mut member = member();
    member.mailspace.config.pump = pump_config();
    let need = member.send_need("mind", &hub.address("mind"), "run the flow");
    member
        .mailspace
        .assign_need("mind", &need, &hub.address("mind"))
        .unwrap();
    let wire = FakeServer::new();
    member.push(&member.client(&wire));
    let pulled = hub.pull(&hub.client(&wire));
    assert!(pulled.failure().is_none(), "{:?}", pulled.warnings);
    let hub_need = hub
        .rows()
        .into_iter()
        .find(|row| row.subject == "run the flow" && row.local_role != "sent")
        .expect("the hub holds the need")
        .handle;

    let port = MailspacePort {
        mailspace: hub.mailspace.clone(),
        pump_identity: "pump".into(),
    };
    port.open_pump_record(&hub_need, "kind: build").unwrap();
    if post {
        port.post_to_need_audience(&hub_need, "pump: run r1 started (build, entry plan)")
            .unwrap();
    }
    port.close_need(&hub_need, "flow finished").unwrap();

    hub.push(&hub.client(&wire));
    let pulled = member.pull(&member.client(&wire));
    assert!(pulled.failure().is_none(), "{:?}", pulled.warnings);
    fold_state(&member.mailspace, &need)
}

#[test]
fn without_the_run_start_post_a_second_member_rejects_the_pump_close() {
    assert_eq!(
        member_authored_close(false),
        None,
        "a close from a pump that never posted is not honored"
    );
}

#[test]
fn the_run_start_post_lets_a_second_member_fold_the_pump_close() {
    assert_eq!(member_authored_close(true).as_deref(), Some("done"));
}
