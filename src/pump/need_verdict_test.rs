//! Focused tests for frozen need-audit verdict routing.

use crate::pump::need::NeedAuditBinding;
use crate::pump::need_verdict::{self, NeedVerdictState};
use crate::pump::state::State;

const NEED: &str = "need1234";
const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn verdict_state_round_trips_without_replacing_other_targets() {
    let mut state = State::default();
    let one = NeedVerdictState {
        original_binding: Some(NeedAuditBinding::new(NEED, "default", 1, BASE, TIP)),
        ..NeedVerdictState::default()
    };
    let two = NeedVerdictState::default();
    need_verdict::set_state(&mut state, "default", NEED, &one).unwrap();
    need_verdict::set_state(&mut state, "gpu", NEED, &two).unwrap();
    assert_eq!(need_verdict::state(&state, "default", NEED).unwrap(), one);
    assert_eq!(need_verdict::state(&state, "gpu", NEED).unwrap(), two);
}
