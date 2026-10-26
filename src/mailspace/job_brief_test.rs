//! Round trips and rejection cases for the multipart assignment codec.

use super::*;

const JOB: &str = "slice-test-job";

/// A sample prose that exercises Unicode, quotes, backslashes, control
/// characters, awkward whitespace and full envelope fences.
fn hard_prose() -> String {
    let fence = format!("{FENCE_LINE}\n[step]\nneed = \"f540ea4e\"\n{FENCE_LINE}\n");
    format!(
        "lead \"quoted\" and back\\slash\ttab\n\r\n \
         unicode: héllo, 日本語, 🛠️️, z\u{1F9F0}z, combining e\u{301}\n \
         {fence}+++ not at line start alone, and a \"third\\\" quote\n \
         trailing spaces and a tab:   \t\n\
         control \u{7}\u{1b} and a form feed \u{c}"
    )
}

const FENCE_LINE: &str = "+++";

/// Encode, decode, and require the exact prose back.
fn assert_round_trip(prose: &str) {
    let assignment = encode(JOB, prose).expect("encodes");
    let decoded = decode(JOB, &assignment.manifest, &assignment.parts).expect("decodes");
    assert_eq!(decoded, prose);
}

#[test]
fn short_prose_round_trips_as_one_part() {
    let assignment = encode(JOB, "one plain body\nsecond line\n").expect("encodes");
    assert_eq!(assignment.parts.len(), 1);
    assert_round_trip("one plain body\nsecond line\n");
}

#[test]
fn unicode_quotes_backslashes_and_whitespace_round_trip() {
    assert_round_trip(&hard_prose());
}

#[test]
fn whitespace_is_preserved_exactly() {
    assert_round_trip("   leading\n\n\ttabs\tand \r\n crlf\n\n   ");
}

#[test]
fn prose_with_envelope_fences_round_trips_and_stays_one_line() {
    let prose = format!(
        "{FENCE_LINE}\ndepends_on = [\"0508a290\"]\n{FENCE_LINE}\n\nBody with\n{FENCE_LINE} alone\n"
    );
    let assignment = encode(JOB, &prose).expect("encodes");
    for part in &assignment.parts {
        assert!(
            !part.contains('\n'),
            "encoded part must be one line: {part}"
        );
        assert!(
            !part.contains('\r'),
            "encoded part must be one line: {part}"
        );
    }
    let decoded = decode(JOB, &assignment.manifest, &assignment.parts).expect("decodes");
    assert_eq!(decoded, prose);
}

#[test]
fn splits_on_char_boundaries_and_reassembles_losslessly() {
    let prose = "ascii é 🛠 日本\n".repeat(40);
    for budget in [1, 2, 3, 4, 5, 7, 11] {
        let chunks = split_with_budget(&prose, budget);
        assert_eq!(chunks.concat(), prose, "budget {budget} must be lossless");
        for chunk in &chunks {
            let escaped = serde_json::to_string(chunk).expect("escapes").len() - 2;
            assert!(
                escaped <= budget || chunk.chars().count() == 1,
                "budget {budget}: chunk of {} encoded bytes is over with {} chars",
                escaped,
                chunk.chars().count()
            );
        }
    }
}

#[test]
fn encoded_length_accounts_for_escaping_not_source_length() {
    for chunk in [
        "plain".to_owned(),
        "a\"quote".to_owned(),
        "back\\slash".to_owned(),
        "tab\tnewline\n".to_owned(),
        "unicode ünicode 🛠".to_owned(),
        "\u{1}\u{1f}".to_owned(),
    ] {
        let via_serde = serde_json::to_string(&chunk).expect("escapes").len() - 2;
        let counted: usize = chunk.chars().map(encoded_char_len).sum();
        assert_eq!(counted, via_serde, "escaping cost drift for {chunk:?}");
    }
}

#[test]
fn public_encode_fits_its_budgets() {
    let prose = hard_prose().repeat(200);
    let assignment = encode(JOB, &prose).expect("encodes");
    assert!(assignment.parts.len() > 1, "a large prose must split");
    for part in &assignment.parts {
        let part: PartBody = serde_json::from_str(part).expect("part parses");
        assert!(part.body.len() <= PART_BODY_BUDGET, "part body budget");
    }
    assert!(assignment.manifest.len() <= MANIFEST_BUDGET);
    assert_round_trip(&prose);
}

#[test]
fn empty_prose_round_trips_as_one_empty_part() {
    let assignment = encode(JOB, "").expect("encodes");
    assert_eq!(assignment.parts.len(), 1);
    assert_round_trip("");
}

#[test]
fn manifest_lists_ordered_part_identities() {
    let prose = hard_prose().repeat(2);
    let assignment = encode_with_budgets(JOB, &prose, 96, MANIFEST_BUDGET).expect("encodes");
    assert!(assignment.parts.len() > 3);
    let manifest: Manifest = serde_json::from_str(&assignment.manifest).expect("manifest parses");
    assert_eq!(manifest.job, JOB);
    assert_eq!(manifest.parts as usize, assignment.parts.len());
    assert_eq!(manifest.bytes, prose.len());
    assert_eq!(manifest.sha256, sha256_hex(prose.as_bytes()));
    for (at, identity) in manifest.part.iter().enumerate() {
        assert_eq!(identity.index, at as u32 + 1, "identities must be ordered");
        let part: PartBody = serde_json::from_str(&assignment.parts[at]).expect("part parses");
        assert_eq!(identity.bytes, part.bytes);
        assert_eq!(identity.sha256, part.sha256);
    }
}

#[test]
fn a_missing_part_is_incomplete_not_corrupt() {
    let assignment = encode(JOB, &hard_prose().repeat(50)).expect("encodes");
    let err = decode(JOB, &assignment.manifest, &assignment.parts[1..]).expect_err("refuses");
    assert!(err.is_job_brief_incomplete(), "{err}");
    assert!(!err.is_job_brief_corrupt(), "{err}");
    assert_eq!(err.job_brief_class(), Some("job-brief-incomplete"));
    let err = decode(JOB, &assignment.manifest, &[]).expect_err("refuses");
    assert!(err.is_job_brief_incomplete(), "{err}");
}

#[test]
fn a_tampered_part_body_is_corrupt() {
    let assignment = encode(JOB, &hard_prose().repeat(50)).expect("encodes");
    let mut parts = assignment.parts.clone();
    parts[0] = rewrite(&parts[0], "body", serde_json::json!("tampered body"));
    let err = decode(JOB, &assignment.manifest, &parts).expect_err("refuses");
    assert_corrupt(&err);
}

#[test]
fn a_tampered_part_hash_is_corrupt() {
    let assignment = encode(JOB, "small").expect("encodes");
    let parts = vec![rewrite(
        &assignment.parts[0],
        "sha256",
        serde_json::json!(hex::encode([7_u8; 32])),
    )];
    let err = decode(JOB, &assignment.manifest, &parts).expect_err("refuses");
    assert_corrupt(&err);
}

#[test]
fn a_tampered_manifest_hash_or_length_is_corrupt() {
    let assignment = encode(JOB, &hard_prose()).expect("encodes");
    for (key, value) in [
        ("sha256", serde_json::json!(hex::encode([9_u8; 32]))),
        ("bytes", serde_json::json!(hard_prose().len() + 1)),
    ] {
        let manifest = rewrite(&assignment.manifest, key, value.clone());
        let err = decode(JOB, &manifest, &assignment.parts).expect_err("refuses");
        assert_corrupt(&err);
    }
}

#[test]
fn a_foreign_job_identity_is_corrupt() {
    let assignment = encode(JOB, &hard_prose()).expect("encodes");
    let err = decode("other-job", &assignment.manifest, &assignment.parts).expect_err("refuses");
    assert_corrupt(&err);
}

#[test]
fn an_unsupported_version_is_rejected_everywhere() {
    let assignment = encode(JOB, &hard_prose().repeat(50)).expect("encodes");
    let manifest = rewrite(&assignment.manifest, "version", serde_json::json!(2));
    let err = decode(JOB, &manifest, &assignment.parts).expect_err("refuses");
    assert_corrupt(&err);
    let mut parts = assignment.parts.clone();
    parts[0] = rewrite(&parts[0], "version", serde_json::json!(2));
    let err = decode(JOB, &assignment.manifest, &parts).expect_err("refuses");
    assert_corrupt(&err);
}

#[test]
fn malformed_bodies_are_rejected() {
    let assignment = encode(JOB, &hard_prose()).expect("encodes");
    let err = decode(JOB, "not json at all", &assignment.parts).expect_err("refuses");
    assert_corrupt(&err);
    let err =
        decode(JOB, &assignment.manifest, &["also not json".to_owned()]).expect_err("refuses");
    assert_corrupt(&err);
    let manifest = rewrite(&assignment.manifest, "extra", serde_json::json!(true));
    let err = decode(JOB, &manifest, &assignment.parts).expect_err("refuses");
    assert_corrupt(&err);
}

#[test]
fn an_out_of_range_or_duplicate_part_index_is_corrupt() {
    let assignment = encode_with_budgets(JOB, &hard_prose(), 96, MANIFEST_BUDGET).expect("encodes");
    let mut parts = assignment.parts.clone();
    parts[0] = rewrite(
        &parts[0],
        "part",
        serde_json::json!(assignment.parts.len() + 1),
    );
    let err = decode(JOB, &assignment.manifest, &parts).expect_err("refuses");
    assert_corrupt(&err);
    parts[0] = assignment.parts[1].clone();
    let err = decode(JOB, &assignment.manifest, &parts).expect_err("refuses");
    assert_corrupt(&err);
}

#[test]
fn an_unfit_manifest_is_refused_at_encode() {
    let err = encode_with_budgets(JOB, &"x".repeat(10_000), 64, 128).expect_err("refuses");
    assert!(!err.is_job_brief_corrupt(), "{err}");
    assert!(!err.is_job_brief_incomplete(), "{err}");
    assert!(err.to_string().contains("manifest"), "{err}");
}

/// Rewrite one top-level JSON field, keeping the frame otherwise intact.
fn rewrite(raw: &str, key: &str, value: serde_json::Value) -> String {
    let mut frame: serde_json::Value = serde_json::from_str(raw).expect("parses");
    frame[key] = value;
    frame.to_string()
}

/// Require the corrupt classification and none other.
fn assert_corrupt(err: &VivariumError) {
    assert!(err.is_job_brief_corrupt(), "{err}");
    assert!(!err.is_job_brief_incomplete(), "{err}");
    assert_eq!(err.job_brief_class(), Some("job-brief-corrupt"));
}
