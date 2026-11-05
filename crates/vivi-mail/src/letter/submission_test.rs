use super::context::*;
use crate::letter::roles::{RoleKind, SlicerMode};

#[test]
fn contract_round_trips_role_and_supplied_context() {
    let mut contract = ReplyContract::new(RoleKind::HeadCeo, "step-7", 3);
    contract.target = Some("crates/vivi-mail".into());
    contract.scope = vec!["src/a.rs".into()];
    contract.dependencies = vec!["need-1".into()];
    contract.expected_slicer_mode = Some(SlicerMode::Split);
    let encoded = serde_json::to_string(&contract).unwrap();
    assert!(encoded.contains("\"role\":\"head-ceo\""));
    let decoded: ReplyContract = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, contract);
    assert_eq!(decoded.permitted_answers, vec!["reviewed", "no-op"]);
    assert_eq!(decoded.version, REPLY_CONTRACT_VERSION);
}
