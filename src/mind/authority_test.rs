//! Tests for [`crate::mind::authority`].

use super::*;

const NOW: u64 = 1_000_000;

fn budgets() -> Budgets {
    Budgets {
        per_need_actions: 2,
        per_need_window_secs: 3600,
        max_wakes_per_hour: 6,
    }
}

fn gate<'a>(stage: AuthorityStage, budgets: &'a Budgets, ledger: &'a Ledger) -> Gate<'a> {
    Gate {
        stage,
        paused: false,
        budgets,
        ledger,
        now: NOW,
    }
}

#[test]
fn observe_allows_reads_and_letters_only() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::Observe, &budgets, &ledger);
    for action in [
        Action::ReadStatus,
        Action::LetterToOperator,
        Action::Escalate,
    ] {
        assert_eq!(permit(&g, action, None), Ok(()), "{action:?}");
    }
    for action in [
        Action::PausePump,
        Action::Requeue,
        Action::Replan,
        Action::Answer,
        Action::Unlock,
        Action::FileFixNeed,
    ] {
        assert!(
            matches!(permit(&g, action, Some("n1")), Err(Denial::Stage { .. })),
            "{action:?}"
        );
    }
}

#[test]
fn unstick_opens_pump_controls_but_not_fix_needs() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::Unstick, &budgets, &ledger);
    for action in [
        Action::Requeue,
        Action::Replan,
        Action::Answer,
        Action::Unlock,
    ] {
        assert_eq!(permit(&g, action, Some("n1")), Ok(()), "{action:?}");
    }
    assert_eq!(permit(&g, Action::PausePump, None), Ok(()));
    assert_eq!(
        permit(&g, Action::FileFixNeed, None),
        Err(Denial::Stage {
            required: AuthorityStage::FileFixes,
            current: AuthorityStage::Unstick,
        })
    );
}

#[test]
fn file_fixes_opens_fix_needs_but_never_upgrades() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::FileFixes, &budgets, &ledger);
    assert_eq!(permit(&g, Action::FileFixNeed, None), Ok(()));
    assert!(matches!(
        permit(&g, Action::Upgrade, None),
        Err(Denial::Stage { .. })
    ));
    assert!(matches!(
        permit(&g, Action::SelfRelease, None),
        Err(Denial::Stage { .. })
    ));
}

#[test]
fn unimplemented_stage_refuses_even_when_reached() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::SelfRelease, &budgets, &ledger);
    assert_eq!(
        permit(&g, Action::Upgrade, None),
        Err(Denial::NotImplemented)
    );
    assert_eq!(permit(&g, Action::Requeue, Some("n1")), Ok(()));
}

#[test]
fn paused_mind_may_still_read_and_write_letters() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let mut g = gate(AuthorityStage::Unstick, &budgets, &ledger);
    g.paused = true;
    assert_eq!(permit(&g, Action::Escalate, None), Ok(()));
    assert_eq!(permit(&g, Action::Requeue, Some("n1")), Err(Denial::Paused));
    assert_eq!(permit(&g, Action::PausePump, None), Err(Denial::Paused));
}

#[test]
fn need_scoped_actions_need_a_need() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::Unstick, &budgets, &ledger);
    assert_eq!(permit(&g, Action::Unlock, None), Err(Denial::MissingNeed));
    assert_eq!(
        permit(&g, Action::Unlock, Some("  ")),
        Err(Denial::MissingNeed)
    );
}

#[test]
fn budget_is_per_need_and_windowed() {
    let budgets = budgets();
    let mut ledger = Ledger::new();
    spend(&mut ledger, "n1", NOW - 10, budgets.per_need_window_secs);
    spend(&mut ledger, "n1", NOW - 5, budgets.per_need_window_secs);
    let g = gate(AuthorityStage::Unstick, &budgets, &ledger);
    assert_eq!(
        permit(&g, Action::Requeue, Some("n1")),
        Err(Denial::Budget {
            need: "n1".into(),
            used: 2,
            limit: 2,
        })
    );
    assert_eq!(permit(&g, Action::Requeue, Some("n2")), Ok(()));
    let later = Gate {
        now: NOW + 3600,
        ..g
    };
    assert_eq!(permit(&later, Action::Requeue, Some("n1")), Ok(()));
}

#[test]
fn spend_prunes_entries_outside_the_window() {
    let mut ledger = Ledger::new();
    spend(&mut ledger, "n1", 100, 50);
    spend(&mut ledger, "n1", 200, 50);
    assert_eq!(ledger["n1"], vec![200]);
}

#[test]
fn stage_table_is_explicit() {
    assert_eq!(Action::Escalate.required_stage(), AuthorityStage::Observe);
    assert_eq!(Action::Unlock.required_stage(), AuthorityStage::Unstick);
    assert_eq!(
        Action::FileFixNeed.required_stage(),
        AuthorityStage::FileFixes
    );
    assert_eq!(
        Action::Upgrade.required_stage(),
        AuthorityStage::UpgradeOnApproval
    );
    assert!(!Action::Escalate.mutates());
    assert!(Action::PausePump.mutates());
    assert!(!Action::PausePump.need_scoped());
}
