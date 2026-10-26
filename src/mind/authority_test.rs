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

fn gate(stage: AuthorityStage, budgets: &Budgets) -> Gate<'_> {
    Gate {
        stage,
        paused: false,
        budgets,
        now: NOW,
    }
}

#[test]
fn observe_allows_reads_and_letters_only() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::Observe, &budgets);
    for action in [
        Action::ReadStatus,
        Action::ReadPumpLog,
        Action::ReadPumpHistory,
        Action::ReadNeed,
        Action::ReadNeedThread,
        Action::ReadNeedUnits,
        Action::ReadTaskThread,
        Action::ReadConfig,
        Action::ReadPool,
        Action::LetterToOperator,
        Action::Escalate,
    ] {
        assert_eq!(permit(&g, &ledger, action, None), Ok(()), "{action:?}");
    }
    for action in [
        Action::PausePump,
        Action::Requeue,
        Action::Replan,
        Action::Answer,
        Action::Unlock,
        Action::ReplyNeed,
        Action::DispatchNeed,
        Action::SetPriority,
        Action::WithdrawNeed,
        Action::SetRoleModel,
        Action::SetPoolCap,
        Action::SetPoolBand,
        Action::FileNeed,
        Action::FileFixNeed,
    ] {
        assert!(
            matches!(
                permit(&g, &ledger, action, Some("n1")),
                Err(Denial::Stage { .. })
            ),
            "{action:?}"
        );
    }
}

#[test]
fn unstick_opens_pump_controls_but_not_fix_needs() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::Unstick, &budgets);
    for action in [
        Action::Requeue,
        Action::Replan,
        Action::Answer,
        Action::Unlock,
    ] {
        assert_eq!(
            permit(&g, &ledger, action, Some("n1")),
            Ok(()),
            "{action:?}"
        );
    }
    assert_eq!(permit(&g, &ledger, Action::PausePump, None), Ok(()));
    assert_eq!(
        permit(&g, &ledger, Action::FileFixNeed, None),
        Err(Denial::Stage {
            required: AuthorityStage::FileFixes,
            current: AuthorityStage::Unstick,
        })
    );
}

#[test]
fn file_fixes_opens_fix_needs_but_never_upgrades() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::FileFixes, &budgets);
    assert_eq!(permit(&g, &ledger, Action::FileNeed, Some("n1")), Ok(()));
    assert_eq!(permit(&g, &ledger, Action::FileFixNeed, Some("n1")), Ok(()));
    assert_eq!(
        permit(&g, &ledger, Action::FileFixNeed, None),
        Err(Denial::MissingNeed)
    );
    assert!(matches!(
        permit(&g, &ledger, Action::Upgrade, None),
        Err(Denial::Stage { .. })
    ));
    assert!(matches!(
        permit(&g, &ledger, Action::SelfRelease, None),
        Err(Denial::Stage { .. })
    ));
}

#[test]
fn unimplemented_stage_refuses_even_when_reached() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::SelfRelease, &budgets);
    assert_eq!(
        permit(&g, &ledger, Action::Upgrade, None),
        Err(Denial::NotImplemented)
    );
    assert_eq!(permit(&g, &ledger, Action::Requeue, Some("n1")), Ok(()));
}

#[test]
fn paused_mind_may_still_read_and_write_letters() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let mut g = gate(AuthorityStage::Unstick, &budgets);
    g.paused = true;
    assert_eq!(permit(&g, &ledger, Action::Escalate, None), Ok(()));
    assert_eq!(
        permit(&g, &ledger, Action::Requeue, Some("n1")),
        Err(Denial::Paused)
    );
    assert_eq!(
        permit(&g, &ledger, Action::PausePump, None),
        Err(Denial::Paused)
    );
}

#[test]
fn need_scoped_actions_need_a_need() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::Unstick, &budgets);
    assert_eq!(
        permit(&g, &ledger, Action::Unlock, None),
        Err(Denial::MissingNeed)
    );
    assert_eq!(
        permit(&g, &ledger, Action::Unlock, Some("  ")),
        Err(Denial::MissingNeed)
    );
}

#[test]
fn budget_is_per_need_and_windowed() {
    let budgets = budgets();
    let mut ledger = Ledger::new();
    spend(&mut ledger, "n1", NOW - 10, budgets.per_need_window_secs);
    spend(&mut ledger, "n1", NOW - 5, budgets.per_need_window_secs);
    let g = gate(AuthorityStage::Unstick, &budgets);
    assert_eq!(
        permit(&g, &ledger, Action::Requeue, Some("n1")),
        Err(Denial::Budget {
            need: "n1".into(),
            used: 2,
            limit: 2,
        })
    );
    assert_eq!(permit(&g, &ledger, Action::Requeue, Some("n2")), Ok(()));
    let later = Gate {
        now: NOW + 3600,
        ..g
    };
    assert_eq!(permit(&later, &ledger, Action::Requeue, Some("n1")), Ok(()));
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

#[test]
fn operator_action_stage_matrix() {
    // (action, lowest stage, mutates, need-scoped, kebab-case kind)
    let matrix = [
        (
            Action::ReadStatus,
            AuthorityStage::Observe,
            false,
            false,
            "read-status",
        ),
        (
            Action::ReadPumpLog,
            AuthorityStage::Observe,
            false,
            false,
            "read-pump-log",
        ),
        (
            Action::ReadPumpHistory,
            AuthorityStage::Observe,
            false,
            false,
            "read-pump-history",
        ),
        (
            Action::ReadNeed,
            AuthorityStage::Observe,
            false,
            false,
            "read-need",
        ),
        (
            Action::ReadNeedThread,
            AuthorityStage::Observe,
            false,
            false,
            "read-need-thread",
        ),
        (
            Action::ReadNeedUnits,
            AuthorityStage::Observe,
            false,
            false,
            "read-need-units",
        ),
        (
            Action::ReadTaskThread,
            AuthorityStage::Observe,
            false,
            false,
            "read-task-thread",
        ),
        (
            Action::ReadConfig,
            AuthorityStage::Observe,
            false,
            false,
            "read-config",
        ),
        (
            Action::ReadPool,
            AuthorityStage::Observe,
            false,
            false,
            "read-pool",
        ),
        (
            Action::LetterToOperator,
            AuthorityStage::Observe,
            false,
            false,
            "letter-to-operator",
        ),
        (
            Action::Escalate,
            AuthorityStage::Observe,
            false,
            false,
            "escalate",
        ),
        (
            Action::PausePump,
            AuthorityStage::Unstick,
            true,
            false,
            "pause-pump",
        ),
        (
            Action::ResumePump,
            AuthorityStage::Unstick,
            true,
            false,
            "resume-pump",
        ),
        (
            Action::Requeue,
            AuthorityStage::Unstick,
            true,
            true,
            "requeue",
        ),
        (
            Action::Replan,
            AuthorityStage::Unstick,
            true,
            true,
            "replan",
        ),
        (
            Action::Answer,
            AuthorityStage::Unstick,
            true,
            true,
            "answer",
        ),
        (
            Action::Unlock,
            AuthorityStage::Unstick,
            true,
            true,
            "unlock",
        ),
        (
            Action::ReplyNeed,
            AuthorityStage::Unstick,
            true,
            true,
            "reply-need",
        ),
        (
            Action::DispatchNeed,
            AuthorityStage::Unstick,
            true,
            true,
            "dispatch-need",
        ),
        (
            Action::SetPriority,
            AuthorityStage::Unstick,
            true,
            true,
            "set-priority",
        ),
        (
            Action::WithdrawNeed,
            AuthorityStage::Unstick,
            true,
            true,
            "withdraw-need",
        ),
        (
            Action::SetRoleModel,
            AuthorityStage::Unstick,
            true,
            true,
            "set-role-model",
        ),
        (
            Action::SetPoolCap,
            AuthorityStage::Unstick,
            true,
            true,
            "set-pool-cap",
        ),
        (
            Action::SetPoolBand,
            AuthorityStage::Unstick,
            true,
            true,
            "set-pool-band",
        ),
        (
            Action::FileNeed,
            AuthorityStage::FileFixes,
            true,
            true,
            "file-need",
        ),
        (
            Action::FileFixNeed,
            AuthorityStage::FileFixes,
            true,
            true,
            "file-fix-need",
        ),
        (
            Action::Upgrade,
            AuthorityStage::UpgradeOnApproval,
            true,
            false,
            "upgrade",
        ),
        (
            Action::SelfRelease,
            AuthorityStage::SelfRelease,
            true,
            false,
            "self-release",
        ),
    ];
    for (action, stage, mutates, need_scoped, kind) in matrix {
        assert_eq!(action.required_stage(), stage, "{action:?}");
        assert_eq!(action.mutates(), mutates, "{action:?}");
        assert_eq!(action.need_scoped(), need_scoped, "{action:?}");
        assert_eq!(action.as_str(), kind, "{action:?}");
        let value = serde_json::to_value(action).unwrap();
        assert_eq!(value, serde_json::Value::from(kind), "{action:?}");
        let back: Action = serde_json::from_value(value).unwrap();
        assert_eq!(back, action, "{action:?}");
    }
}

#[test]
fn operator_action_denials_cover_the_new_writes() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let observe = gate(AuthorityStage::Observe, &budgets);
    assert!(matches!(
        permit(&observe, &ledger, Action::ReplyNeed, Some("n1")),
        Err(Denial::Stage {
            required: AuthorityStage::Unstick,
            current: AuthorityStage::Observe,
        })
    ));
    assert!(matches!(
        permit(&observe, &ledger, Action::FileNeed, Some("n1")),
        Err(Denial::Stage {
            required: AuthorityStage::FileFixes,
            current: AuthorityStage::Observe,
        })
    ));
    let unstick = gate(AuthorityStage::Unstick, &budgets);
    for action in [
        Action::ReplyNeed,
        Action::DispatchNeed,
        Action::SetPriority,
        Action::WithdrawNeed,
        Action::SetRoleModel,
        Action::SetPoolCap,
        Action::SetPoolBand,
    ] {
        assert_eq!(
            permit(&unstick, &ledger, action, Some("n1")),
            Ok(()),
            "{action:?}"
        );
        assert_eq!(
            permit(&unstick, &ledger, action, None),
            Err(Denial::MissingNeed),
            "{action:?}"
        );
    }
}

#[test]
fn new_writes_spend_the_need_budget_and_respect_pause() {
    let budgets = budgets();
    let mut ledger = Ledger::new();
    spend(&mut ledger, "n1", NOW - 10, budgets.per_need_window_secs);
    spend(&mut ledger, "n1", NOW - 5, budgets.per_need_window_secs);
    let g = gate(AuthorityStage::Unstick, &budgets);
    assert_eq!(
        permit(&g, &ledger, Action::SetPriority, Some("n1")),
        Err(Denial::Budget {
            need: "n1".into(),
            used: 2,
            limit: 2,
        })
    );
    let mut paused = g;
    paused.paused = true;
    assert_eq!(
        permit(&paused, &Ledger::new(), Action::ReplyNeed, Some("n1")),
        Err(Denial::Paused)
    );
    assert_eq!(
        permit(&paused, &Ledger::new(), Action::ReadConfig, None),
        Ok(())
    );
}

#[test]
fn as_str_is_the_envelope_kind() {
    for action in [
        Action::ReadStatus,
        Action::LetterToOperator,
        Action::Escalate,
        Action::PausePump,
        Action::ResumePump,
        Action::Requeue,
        Action::Replan,
        Action::Answer,
        Action::Unlock,
        Action::FileFixNeed,
        Action::Upgrade,
        Action::SelfRelease,
    ] {
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            serde_json::Value::from(action.as_str())
        );
    }
}
