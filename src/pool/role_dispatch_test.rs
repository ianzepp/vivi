use super::*;
use chrono::TimeZone;

use crate::pool::slot::PoolId;

/// A pool that serves `P1` with the flash model at `xhigh` and `P2` with the
/// other model at `low`, and offers both models to its harness.
fn pool() -> PoolConfig {
    toml::from_str(
        r#"
provider = "local"
slots = 4
[harness]
name = "pi"
run = "pi --model {model}"
models = ["zai/glm-5.3-flash", "other/model"]
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "xhigh"
[[bands.P2]]
model = "other/model"
reasoning = "low"
[local]
root = "~/lanes"
"#,
    )
    .unwrap()
}

fn pool_config(toml_text: &str) -> PoolConfig {
    toml::from_str(toml_text).unwrap()
}

fn role(name: &str, kind: Option<&str>, band: Option<&str>) -> RoleFacts {
    RoleFacts {
        name: name.to_string(),
        kind: kind.map(str::to_string),
        band: band.map(str::to_string),
        thinking: None,
        charter: String::new(),
        read_only: None,
    }
}

fn input<'a>(
    role: Option<&'a RoleFacts>,
    body: &'a str,
    model: Option<&'a str>,
    read_only: bool,
) -> Input<'a> {
    Input {
        model_flag: model,
        read_only_flag: read_only,
        role,
        body,
    }
}

fn plain(body: &str) -> Input<'_> {
    input(None, body, None, false)
}

#[test]
fn an_auditor_role_is_read_only_and_takes_its_band_model_and_reasoning() {
    let auditor = role("reviewer", Some("auditor"), Some("P1-S0"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&auditor), "review it", None, false),
    )
    .unwrap();
    assert_eq!(
        resolved,
        Resolved {
            model: "zai/glm-5.3-flash".into(),
            reasoning: Some("xhigh".into()),
            band: Some(1),
            read_only: true,
            role: Some("reviewer".into()),
        }
    );
}

#[test]
fn builtin_auditor_uses_the_shipped_charter_and_stays_read_only_with_a_model_override() {
    let auditor = RoleFacts::builtin_auditor();
    assert!(auditor.charter.contains("# Auditor"));
    assert_eq!(auditor.kind.as_deref(), Some("auditor"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(
            Some(&auditor),
            "audit this range",
            Some("other/model"),
            false,
        ),
    )
    .unwrap();
    assert!(resolved.read_only);
    assert_eq!(resolved.role.as_deref(), Some("auditor"));
    assert_eq!(resolved.model, "other/model");
}

#[test]
fn privileged_publication_is_limited_to_merge_and_guarded_release_roles() {
    let merge = role("merge", Some("merge"), None);
    let release = role("release", Some("release"), None);
    let hand = role("hand", Some("hand"), None);
    let sha = "a".repeat(40);
    let mains = vec!["main".to_owned()];
    assert!(validate_publish(Some(&merge), Some("refs/heads/next"), None, &mains).is_err());
    assert!(validate_publish(Some(&merge), Some("refs/heads/main"), Some(&sha), &mains).is_ok());
    assert!(validate_publish(Some(&release), Some("refs/heads/main"), Some(&sha), &mains).is_ok());
    assert!(validate_publish(Some(&release), Some("refs/heads/main"), None, &mains).is_err());
    assert!(validate_publish(Some(&hand), Some("refs/heads/main"), None, &mains).is_err());
    assert!(
        validate_publish(
            Some(&hand),
            Some("refs/heads/vivi/unit/u1-a1"),
            None,
            &mains
        )
        .is_ok()
    );
    assert!(validate_publish(Some(&hand), Some("refs/heads/vivi/need/n1"), None, &mains).is_err());
    assert!(validate_publish(None, Some("refs/heads/main"), None, &mains).is_err());
}

#[test]
fn a_repairer_publishes_only_one_repair_segment() {
    let repairer = role("fixer", Some("Repairer"), None);
    assert!(validate_publish(Some(&repairer), Some("refs/heads/vivi/repair/x"), None, &[]).is_ok());
    for bad in [
        "refs/heads/vivi/repair/x/y",
        "refs/heads/vivi/repair/",
        "refs/heads/main",
        "refs/heads/vivi/unit/x",
    ] {
        assert!(
            validate_publish(Some(&repairer), Some(bad), None, &["main".to_owned()]).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn triage_and_cadence_kinds_are_read_only_too() {
    for kind in [
        "triage", "cadence", "AUDITOR", "Triage", "steward", "head", "gate",
    ] {
        let worker = role("gatekeeper", Some(kind), Some("P1"));
        let resolved = resolve(
            &pool(),
            "bridge",
            &input(Some(&worker), "sort it", None, false),
        )
        .unwrap();
        assert!(resolved.read_only, "kind {kind} must be read-only");
    }
}

#[test]
fn an_explicit_read_only_flag_on_a_hand_role_wins() {
    let mut hand = role("lane-3", Some("hand"), Some("P1"));
    hand.read_only = Some(true);
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", None, false),
    )
    .unwrap();
    assert!(resolved.read_only);
}

#[test]
fn a_hand_role_and_a_merge_named_hand_write() {
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
    let merge = role("merge", Some("hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&merge), "merge it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
    let capitalised = role("lane-4", Some("Hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&capitalised), "build it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
    let lint = role("lint", Some("lint"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&lint), "lint it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
}

#[test]
fn a_read_only_flag_makes_a_hand_role_read_only() {
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", None, true),
    )
    .unwrap();
    assert!(resolved.read_only);
}

#[test]
fn a_body_band_line_beats_the_role_band() {
    let worker = role("lane-3", Some("hand"), Some("P2-S0"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it\nband: P1", None, false),
    )
    .unwrap();
    assert_eq!(resolved.band, Some(1));
    assert_eq!(resolved.model, "zai/glm-5.3-flash");
    assert_eq!(resolved.reasoning.as_deref(), Some("xhigh"));
}

#[test]
fn a_role_band_alone_resolves() {
    let worker = role("lane-3", Some("hand"), Some("P2-S0"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap();
    assert_eq!(resolved.band, Some(2));
    assert_eq!(resolved.model, "other/model");
    assert_eq!(resolved.reasoning.as_deref(), Some("low"));
}

#[test]
fn an_unparseable_role_band_is_an_error_naming_the_role_and_the_value() {
    let worker = role("scribe", Some("hand"), Some("S1"));
    let err = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "write it", None, false),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("role scribe"), "got: {message}");
    assert!(message.contains("S1"), "got: {message}");
}

#[test]
fn no_band_and_no_model_is_an_error_naming_the_role() {
    let worker = role("scribe", Some("hand"), None);
    let err = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "write it", None, false),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "configuration error: no model: pass --model, give the task a `band:` line, \
         or give role scribe a band (`vivi role set scribe --model P2`)"
    );
}

#[test]
fn no_band_and_no_model_without_a_role_blames_the_role_slot() {
    let err = resolve(&pool(), "bridge", &plain("write it")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "configuration error: no model: pass --model, give the task a `band:` line, \
         or give the role a band (`vivi role set <name> --model P2`)"
    );
}

#[test]
fn a_band_the_pool_does_not_serve_is_refused_naming_the_band_and_pool() {
    let err = resolve(&pool(), "bridge", &plain("build it\nband: P3")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("band P3"), "got: {message}");
    assert!(message.contains("bridge"), "got: {message}");
}

#[test]
fn a_model_override_keeps_the_role_read_only_and_takes_reasoning_only_for_a_match() {
    let auditor = role("reviewer", Some("auditor"), Some("P1-S0"));
    let same = resolve(
        &pool(),
        "bridge",
        &input(
            Some(&auditor),
            "review it",
            Some("zai/glm-5.3-flash"),
            false,
        ),
    )
    .unwrap();
    assert!(same.read_only);
    assert_eq!(same.model, "zai/glm-5.3-flash");
    assert_eq!(same.reasoning.as_deref(), Some("xhigh"));

    let other = resolve(
        &pool(),
        "bridge",
        &input(Some(&auditor), "review it", Some("other/model"), false),
    )
    .unwrap();
    assert!(other.read_only);
    assert_eq!(other.model, "other/model");
    assert_eq!(other.reasoning, None);
}

#[test]
fn a_model_override_needs_no_band_and_ignores_a_band_the_pool_does_not_serve() {
    let hand = role("lane-3", Some("hand"), None);
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", Some("other/model"), false),
    )
    .unwrap();
    assert_eq!(resolved.band, None);
    assert_eq!(resolved.reasoning, None);
    let unserved = role("lane-3", Some("hand"), Some("P4"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&unserved), "build it", Some("other/model"), false),
    )
    .unwrap();
    assert_eq!(resolved.band, Some(4));
    assert_eq!(resolved.reasoning, None);
}

#[test]
fn a_model_the_harness_does_not_offer_is_refused_naming_the_model() {
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let err = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", Some("mystery/model"), false),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("mystery/model"), "got: {message}");
    assert!(message.contains("harness.models"), "got: {message}");
}

#[test]
fn a_band_choice_outside_the_harness_models_is_refused_too() {
    let config = pool_config(
        r#"
provider = "local"
slots = 4
[harness]
name = "pi"
run = "pi --model {model}"
models = ["zai/glm-5.3-flash"]
[[bands.P1]]
model = "mystery/model"
reasoning = "low"
[local]
root = "~/lanes"
"#,
    );
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let err = resolve(
        &config,
        "bridge",
        &input(Some(&hand), "build it", None, false),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("mystery/model"), "got: {err}");
}

const RULES: &str = "Work the brief. Never weaken a test.\n";

#[test]
fn a_blank_charter_gives_the_pool_rules_then_the_project_rules() {
    for charter in ["", "  ", "\n  \n"] {
        assert_eq!(standing_prompt("auditor", charter, RULES, "", true), RULES);
        assert_eq!(
            standing_prompt("auditor", charter, RULES, "## r\n\nbody\n", false),
            format!("{RULES}\n\n# Project rules\n\n## r\n\nbody")
        );
    }
}

#[test]
fn project_rules_follow_the_base_when_there_is_no_role() {
    assert_eq!(with_project_rules(RULES, "  \n"), RULES);
    assert_eq!(
        with_project_rules(RULES, "## r\n\nbody\n"),
        format!("{RULES}\n\n# Project rules\n\n## r\n\nbody")
    );
}

#[test]
fn a_charter_gives_the_headings_in_order_over_the_pool_rules() {
    let prompt = standing_prompt("scribe", "Write the letters.", RULES, "", false);
    assert_eq!(
        prompt,
        "# Standing charter (role scribe)\n\nWrite the letters.\n\n---\n\n\
         You run in a pool slot with no `vivi` command and no mailspace. Where the charter tells you to read or settle your handle with `vivi`, read your brief instead and end with your receipt on standard output; the runner records your result and the Mind settles the handle.\n\n\
         # Pool rules (these bound the charter)\n\nWork the brief. Never weaken a test.\n"
    );
    let charter = prompt.find("# Standing charter (role scribe)").unwrap();
    let rule = prompt
        .find("# Pool rules (these bound the charter)")
        .unwrap();
    let separator = prompt.find("---").unwrap();
    assert!(charter < separator && separator < rule);
}

#[test]
fn the_two_pool_notes_differ() {
    let bridged = standing_prompt("scribe", "Write the letters.", RULES, "", true);
    let unbridged = standing_prompt("scribe", "Write the letters.", RULES, "", false);
    assert_ne!(bridged, unbridged);
    assert!(bridged.contains("MCP server `vivi`"), "got: {bridged}");
    assert!(
        unbridged.contains("no `vivi` command and no mailspace"),
        "got: {unbridged}"
    );
}

#[test]
fn the_bridge_note_does_not_tell_the_worker_to_settle_its_handle() {
    let bridged = standing_prompt("scribe", "Write the letters.", RULES, "", true);
    assert!(
        bridged.contains(
            "You run in a pool slot with the vivi tools of the project server \
             (MCP server `vivi`). Your task handle is in your brief: read it \
             with `show` and `thread`, report with `reply`; the runner records \
             your result and the pump or Mind settles the handle."
        ),
        "got: {bridged}"
    );
    assert_eq!(bridged.matches("report with `reply`").count(), 1);
    assert!(!bridged.contains("`done`"), "got: {bridged}");
}

#[test]
fn project_rules_follow_the_pool_rules_under_their_own_heading() {
    let prompt = standing_prompt(
        "scribe",
        "Write the letters.",
        RULES,
        "## r\n\nbody\n",
        true,
    );
    assert!(
        prompt.ends_with("# Project rules\n\n## r\n\nbody"),
        "got: {prompt}"
    );
    let rule = prompt.find("# Pool rules").unwrap();
    let project = prompt.find("# Project rules").unwrap();
    assert!(rule < project);
    let none = standing_prompt("scribe", "Write the letters.", RULES, " \n", true);
    assert!(!none.contains("Project rules"), "got: {none}");
}

#[test]
fn the_snapshot_holds_project_rules_and_no_local_rules() {
    let text = project_rules_text(&[
        ("project", "Never force push", "Do not force push.\n"),
        ("local", "My secret habit", "Use tabs."),
        ("project", "Test first", "Write the test."),
    ]);
    assert_eq!(
        text,
        "## Never force push\n\nDo not force push.\n\n## Test first\n\nWrite the test.\n\n"
    );
    assert!(!text.contains("secret"));
    assert_eq!(project_rules_text(&[("local", "x", "y")]), "");
    assert_eq!(project_rules_text(&[]), "");
}

#[test]
fn the_snapshot_is_capped_with_a_cut_marker_on_a_char_boundary() {
    let big = "é".repeat(PROJECT_RULES_CAP);
    let text = project_rules_text(&[("project", "Big", &big)]);
    assert!(text.ends_with("\n[project rules cut at 8192 bytes]\n"));
    assert!(text.len() <= PROJECT_RULES_CAP + 64, "len {}", text.len());
    let small = project_rules_text(&[("project", "Small", "x")]);
    assert!(!small.contains("cut at"));
}

#[test]
fn the_cap_cuts_inside_a_multibyte_character_back_to_a_boundary() {
    // An odd-length subject followed by three-byte characters puts the byte
    // at the cap inside a character.
    let subject = "\u{e9}\u{e9}\u{e9}x";
    assert_eq!(subject.len() % 2, 1);
    let big = "\u{20ac}".repeat(PROJECT_RULES_CAP);
    let full = format!("## {subject}\n\n{big}\n\n");
    assert!(!full.is_char_boundary(PROJECT_RULES_CAP), "cut must split");
    let text = project_rules_text(&[("project", subject, &big)]);
    let marker = "\n[project rules cut at 8192 bytes]\n";
    assert!(
        text.ends_with(marker),
        "got tail: {:?}",
        &text[text.len() - 40..]
    );
    let kept = &text[..text.len() - marker.len()];
    assert!(kept.len() <= PROJECT_RULES_CAP, "kept {}", kept.len());
    assert!(full.starts_with(kept));
    assert!(std::str::from_utf8(text.as_bytes()).is_ok());
}

#[test]
fn resolve_leaves_the_pool_config_alone() {
    let config = pool();
    let worker = role("lane-3", Some("hand"), Some("P1"));
    resolve(
        &config,
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap();
    config
        .validate(&PoolId::new("space", "", "bridge"))
        .unwrap();
}

#[test]
fn digest_brief_first_run_uses_the_default_window() {
    let now = chrono::Utc
        .with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
        .unwrap();
    let brief = digest_brief(now, None, 86_400);
    assert_eq!(brief.subject, "digest 2026-10-19 (1d)");
    assert_eq!(brief.window_label, "1d");
    assert_eq!(brief.start, now - chrono::Duration::seconds(86_400));
    assert_eq!(brief.end, now);
}

#[test]
fn digest_brief_subsequent_run_covers_since_last() {
    let now = chrono::Utc
        .with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
        .unwrap();
    let last = now - chrono::Duration::seconds(3 * 86_400);
    let brief = digest_brief(now, Some(last), 86_400);
    assert_eq!(brief.window_label, "3d");
    assert_eq!(brief.start, last);
}

#[test]
fn digest_run_text_names_contract() {
    let now = chrono::Utc
        .with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
        .unwrap();
    let brief = digest_brief(now, None, 86_400);
    let root = std::path::Path::new("/tmp/demo");
    let text = digest_run_text(&brief, root, "digest");
    assert!(text.contains("## Digest run"), "got: {text}");
    assert!(text.contains(&brief.subject), "got: {text}");
    assert!(text.contains(&brief.start.to_rfc3339()), "got: {text}");
    assert!(text.contains(&brief.end.to_rfc3339()), "got: {text}");
    assert!(text.contains("--tag digest"), "got: {text}");
    assert!(text.contains("--body-file"), "got: {text}");
    assert!(text.contains("/tmp/demo"), "got: {text}");
    assert!(text.contains("status.txt"), "got: {text}");
    assert!(text.contains("only write"), "got: {text}");
}

#[test]
fn a_merge_role_publishes_only_its_four_ref_families() {
    let merge = role("merge", Some("merge"), None);
    let mains = vec!["trunk".to_owned(), "main".to_owned()];
    for good in [
        "refs/heads/trunk",
        "refs/heads/main",
        "refs/heads/vivi/need/n1",
        "refs/heads/vivi/need-prior/n1-r2",
        "refs/heads/pump/merge-main/default-job-1",
    ] {
        assert!(
            validate_publish(Some(&merge), Some(good), None, &mains).is_ok(),
            "{good}"
        );
    }
    for bad in [
        "refs/heads/next",
        "refs/heads/vivi/need/a/b",
        "refs/heads/vivi/need/",
        "refs/heads/vivi/need-prior/a/b",
        "refs/heads/pump/merge-main/a/b",
        "refs/heads/vivi/unit/x",
        "refs/heads/vivi/repair/x",
    ] {
        assert!(
            validate_publish(Some(&merge), Some(bad), None, &mains).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn a_merge_role_may_not_publish_main_when_the_configured_main_is_another_name() {
    let merge = role("merge", Some("merge"), None);
    let mains = vec!["trunk".to_owned()];
    assert!(validate_publish(Some(&merge), Some("refs/heads/trunk"), None, &mains).is_ok());
    assert!(validate_publish(Some(&merge), Some("refs/heads/main"), None, &mains).is_err());
    assert!(validate_publish(Some(&merge), Some("refs/heads/main"), None, &[]).is_err());
}

fn slug_role(slug: &str, thinking: Option<&str>) -> RoleFacts {
    RoleFacts {
        thinking: thinking.map(str::to_string),
        ..role("slugger", Some("hand"), Some(slug))
    }
}

#[test]
fn a_slug_role_resolves_to_that_model_with_its_thinking() {
    let worker = slug_role("other/model", Some("high"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap();
    assert_eq!(resolved.model, "other/model");
    assert_eq!(resolved.reasoning.as_deref(), Some("high"));
    assert_eq!(resolved.band, None);
    assert_eq!(resolved.role.as_deref(), Some("slugger"));
}

#[test]
fn a_slug_role_without_thinking_takes_the_reasoning_of_a_band_entry_naming_it() {
    let worker = slug_role("other/model", None);
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap();
    assert_eq!(resolved.model, "other/model");
    assert_eq!(resolved.reasoning.as_deref(), Some("low"));
}

#[test]
fn a_slug_role_with_no_thinking_and_no_band_entry_has_no_reasoning() {
    let config = pool_config(
        r#"
provider = "local"
slots = 4
[harness]
name = "pi"
run = "pi --model {model}"
models = ["zai/glm-5.3-flash", "bare/model"]
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "xhigh"
[local]
root = "~/lanes"
"#,
    );
    let worker = slug_role("bare/model", None);
    let resolved = resolve(
        &config,
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap();
    assert_eq!(resolved.model, "bare/model");
    assert_eq!(resolved.reasoning, None);
}

#[test]
fn a_model_flag_equal_to_a_slug_role_takes_the_role_thinking() {
    let worker = slug_role("other/model", Some("high"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it", Some("other/model"), false),
    )
    .unwrap();
    assert_eq!(resolved.model, "other/model");
    assert_eq!(resolved.reasoning.as_deref(), Some("high"));
}

#[test]
fn a_body_band_beats_a_slug_role() {
    let worker = slug_role("other/model", Some("high"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it\nband: P1", None, false),
    )
    .unwrap();
    assert_eq!(resolved.model, "zai/glm-5.3-flash");
    assert_eq!(resolved.band, Some(1));
}

#[test]
fn a_role_value_that_is_neither_band_nor_harness_model_is_refused() {
    let worker = slug_role("nope/model", None);
    let err = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("role slugger"), "got: {message}");
    assert!(message.contains("nope/model"), "got: {message}");
    assert!(message.contains("harness.models"), "got: {message}");
}
