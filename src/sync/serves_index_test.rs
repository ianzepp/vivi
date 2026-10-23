//! A pulled need's `serves` is indexed in its row, as a sent one's is.

use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::sync::fake_server::{LETTER_DATE, World, wire_id};

const HEADERS_WANT: &str = "X-Vivi-Format: 1\r\nX-Vivi-Kind: want\r\n";
const HEADERS_NEED: &str = "X-Vivi-Format: 1\r\nX-Vivi-Kind: need\r\n";

#[test]
fn a_pulled_need_row_carries_the_serves_index() {
    let w = World::new();
    let hand = w.bob.address("hand");
    let want_wire = wire_id(0xa1, 0);
    let want_mid = format!("{want_wire}@x.test");
    w.put_full(
        &want_wire,
        "want",
        &hand,
        "the want",
        &want_body("q", "prose"),
        LETTER_DATE,
        HEADERS_WANT,
    );
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(format!("<{want_mid}>"));
    w.put_full(
        &wire_id(0xb2, 0),
        "need",
        &hand,
        "the need",
        &need_body_with(&front, "prose"),
        LETTER_DATE,
        HEADERS_NEED,
    );

    w.bob.pull(&w.bob_client);

    let row = w
        .bob
        .rows()
        .into_iter()
        .find(|row| row.subject == "the need")
        .expect("the need was pulled");
    let meta = w
        .bob
        .mailspace
        .storage()
        .unwrap()
        .item_metadata(&row.message_id)
        .unwrap();
    assert_eq!(meta.get("serves"), Some(&want_mid), "{meta:?}");
}
