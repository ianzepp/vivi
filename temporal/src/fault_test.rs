use super::*;

#[test]
fn parses_each_kind() {
    let got = Fault::parse_all(&["quota@1", "lost@2", "timeout@3", "red@1", "changes@2"]).unwrap();
    let kinds: Vec<FaultKind> = got.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        vec![
            FaultKind::Quota,
            FaultKind::Lost,
            FaultKind::Timeout,
            FaultKind::Red,
            FaultKind::Changes
        ]
    );
    assert_eq!(fault_at(&got, 2), Some(FaultKind::Lost));
    assert_eq!(fault_at(&got, 9), None);
}

#[test]
fn rejects_malformed_entries() {
    for entry in ["quota", "flood@1", "red@x", "red@0"] {
        assert!(Fault::parse(entry).is_err(), "{entry} should not parse");
    }
}
