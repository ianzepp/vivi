//! Tests for the step transient classifier: table-driven diagnostics over
//! the machine evidence the pump actually collects.

use super::{Eligibility, Evidence, Verdict, classify};

/// A task receipt as the pool writes it: summary lines, then sections.
fn receipt(exit: i32, message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\npool fly4  slot s1  model zai/glm\n\
         agent exit {exit} in 0s, job 12s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

/// One classification case: the machine evidence and the wanted verdict.
struct Case {
    name: &'static str,
    state: &'static str,
    detail: &'static str,
    receipt: String,
    eligibility: Eligibility,
    want: Verdict,
}

/// A role step: the owner expects a model reply; lost and timeout jobs of
/// this kind are retry-eligible.
fn role() -> Eligibility {
    Eligibility {
        model_reply: true,
        lost: true,
        timeout: true,
    }
}

/// A plain unit failure the classifier must leave alone.
fn work(name: &'static str, exit: i32, stderr: &'static str) -> Case {
    Case {
        name,
        state: "finished",
        detail: "unit ended",
        receipt: receipt(exit, "the hand reported the real result", stderr),
        eligibility: role(),
        want: Verdict::Work,
    }
}

/// A retryable transient of `class`, evidenced by one stderr line.
fn retryable_stderr(name: &'static str, stderr: &'static str, class: &'static str) -> Case {
    Case {
        name,
        state: "finished",
        detail: "unit ended",
        receipt: receipt(1, "", stderr),
        eligibility: role(),
        want: Verdict::Retryable(super::Transient {
            class,
            evidence: stderr.to_owned(),
        }),
    }
}

fn cases() -> Vec<Case> {
    vec![
        retryable_stderr(
            "429 throttled in stderr",
            "upstream error: 429 rate limited, retry later",
            "throttled",
        ),
        retryable_stderr(
            "overloaded_error from the gateway",
            "api error: overloaded_error, please try again",
            "overload",
        ),
        Case {
            name: "503 Service Unavailable in the runner log tail",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(1, "", "build ok").replace(
                "--- runner log tail\nlog line",
                "--- runner log tail\nproxy: 503 Service Unavailable",
            ),
            eligibility: role(),
            want: Verdict::Retryable(super::Transient {
                class: "provider",
                evidence: "proxy: 503 Service Unavailable".to_owned(),
            }),
        },
        retryable_stderr(
            "connection reset by peer mid-stream",
            "stream error: connection reset by peer",
            "provider",
        ),
        Case {
            name: "auth refusal is permanent, not transient",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(1, "", "authentication error: invalid_api_key supplied"),
            eligibility: role(),
            want: Verdict::Permanent(super::Fault {
                class: "auth",
                evidence: "authentication error: invalid_api_key supplied".to_owned(),
            }),
        },
        Case {
            name: "503 gateway is transient where 401 auth is permanent",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(1, "", "proxy: 503 Service Unavailable"),
            eligibility: role(),
            want: Verdict::Retryable(super::Transient {
                class: "provider",
                evidence: "proxy: 503 Service Unavailable".to_owned(),
            }),
        },
        Case {
            name: "runner-established fetch_failed is retryable",
            state: "finished",
            detail: "fetch_failed git.example/x/y exit 128",
            receipt: receipt(1, "", ""),
            eligibility: role(),
            want: Verdict::Retryable(super::Transient {
                class: "runner-fetch",
                evidence: "fetch_failed git.example/x/y exit 128".to_owned(),
            }),
        },
        Case {
            name: "runner-established checkout_failed is retryable",
            state: "finished",
            detail: "checkout_failed git.example/x/y exit 2",
            receipt: receipt(1, "", ""),
            eligibility: role(),
            want: Verdict::Retryable(super::Transient {
                class: "runner-checkout",
                evidence: "checkout_failed git.example/x/y exit 2".to_owned(),
            }),
        },
        Case {
            name: "invalid target push outcome is permanent",
            state: "finished",
            detail: "target_seed_failed git.example/x/y exit 1",
            receipt: receipt(1, "", ""),
            eligibility: role(),
            want: Verdict::Permanent(super::Fault {
                class: "target",
                evidence: "target_seed_failed git.example/x/y exit 1".to_owned(),
            }),
        },
        Case {
            name: "workdir_missing push outcome is permanent",
            state: "finished",
            detail: "workdir_missing /srv/slots/s1",
            receipt: receipt(1, "", ""),
            eligibility: role(),
            want: Verdict::Permanent(super::Fault {
                class: "workdir",
                evidence: "workdir_missing /srv/slots/s1".to_owned(),
            }),
        },
        Case {
            name: "agent exit 127 is a missing tool",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(127, "", ""),
            eligibility: role(),
            want: Verdict::Permanent(super::Fault {
                class: "missing-tool",
                evidence: "agent exit 127".to_owned(),
            }),
        },
        Case {
            name: "command not found in stderr is a missing tool",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(1, "", "bash: line 1: pi: command not found"),
            eligibility: role(),
            want: Verdict::Permanent(super::Fault {
                class: "missing-tool",
                evidence: "bash: line 1: pi: command not found".to_owned(),
            }),
        },
        work(
            "plain unit failure is work",
            1,
            "test result: FAILED. 1 failed",
        ),
        work("plain success is work", 0, ""),
        Case {
            name: "lost is retried only when the collector made it eligible",
            state: "lost",
            detail: "lost/no receipt returned",
            receipt: receipt(1, "", ""),
            eligibility: Eligibility::default(),
            want: Verdict::Work,
        },
        Case {
            name: "eligible lost is a retryable lost",
            state: "lost",
            detail: "lost/no receipt returned",
            receipt: receipt(1, "", ""),
            eligibility: Eligibility {
                lost: true,
                ..Eligibility::default()
            },
            want: Verdict::Retryable(super::Transient {
                class: "lost",
                evidence: "lost/no receipt returned".to_owned(),
            }),
        },
        Case {
            name: "ineligible timeout falls through untouched",
            state: "timeout",
            detail: "timeout after 900s",
            receipt: receipt(1, "", ""),
            eligibility: Eligibility::default(),
            want: Verdict::Work,
        },
        Case {
            name: "eligible timeout is a retryable timeout",
            state: "timeout",
            detail: "timeout after 900s",
            receipt: receipt(1, "", ""),
            eligibility: Eligibility {
                timeout: true,
                ..Eligibility::default()
            },
            want: Verdict::Retryable(super::Transient {
                class: "timeout",
                evidence: "timeout after 900s".to_owned(),
            }),
        },
        Case {
            name: "absent model reply after a clean exit is retryable",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(0, "", ""),
            eligibility: role(),
            want: Verdict::Retryable(super::Transient {
                class: "empty-reply",
                evidence: "model reply absent".to_owned(),
            }),
        },
        Case {
            name: "whitespace-only model reply reads as absent",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(0, "   \n\t", ""),
            eligibility: role(),
            want: Verdict::Retryable(super::Transient {
                class: "empty-reply",
                evidence: "model reply absent".to_owned(),
            }),
        },
        Case {
            name: "literal null model reply is retryable",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(0, "null", ""),
            eligibility: role(),
            want: Verdict::Retryable(super::Transient {
                class: "empty-reply",
                evidence: "model reply literal null".to_owned(),
            }),
        },
        Case {
            name: "empty reply after an ordinary failure keeps failure precedence",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(1, "", "test result: FAILED. 1 failed"),
            eligibility: role(),
            want: Verdict::Work,
        },
        Case {
            name: "model-free empty output is not an empty reply",
            state: "finished",
            detail: "unit ended",
            receipt: receipt(0, "", ""),
            eligibility: Eligibility::default(),
            want: Verdict::Work,
        },
    ]
}

#[test]
fn table_driven_diagnostics() {
    for case in cases() {
        let got = classify(
            &Evidence {
                state: case.state,
                detail: case.detail,
                receipt: &case.receipt,
            },
            case.eligibility,
        );
        assert_eq!(got, case.want, "case {}", case.name);
    }
}

#[test]
fn provider_markers_in_model_prose_are_not_transient() {
    let prose = "The upstream kept returning 429 rate limit errors, then the \
                 gateway said 503 Service Unavailable and the stream hit \
                 connection reset by peer, so I could not finish the check.";
    let evidence = Evidence {
        state: "finished",
        detail: "unit ended",
        receipt: &receipt(1, prose, "assertion failed: the real unit check"),
    };
    assert_eq!(classify(&evidence, role()), Verdict::Work);
}

#[test]
fn checkout_facts_versus_prose() {
    let runner_fact = Evidence {
        state: "finished",
        detail: "checkout_failed git.example/x/y exit 2",
        receipt: &receipt(1, "", ""),
    };
    assert_eq!(
        classify(&runner_fact, role()),
        Verdict::Retryable(super::Transient {
            class: "runner-checkout",
            evidence: "checkout_failed git.example/x/y exit 2".to_owned(),
        })
    );
    let prose = Evidence {
        state: "finished",
        detail: "unit ended",
        receipt: &receipt(
            1,
            "the checkout failed because the branch was rebased upstream",
            "git exit code 2 in the unit's own log",
        ),
    };
    assert_eq!(classify(&prose, role()), Verdict::Work);
}

#[test]
fn evidence_is_one_capped_line() {
    let long = format!("429 rate limit {}", "x".repeat(400));
    let evidence = Evidence {
        state: "finished",
        detail: "unit ended",
        receipt: &receipt(1, "", &long),
    };
    let got = classify(&evidence, role());
    let Verdict::Retryable(transient) = got else {
        panic!("wanted a retryable transient, got {got:?}");
    };
    assert_eq!(transient.class, "throttled");
    assert_eq!(transient.evidence.chars().count(), 201);
    assert!(transient.evidence.ends_with('…'));
}
