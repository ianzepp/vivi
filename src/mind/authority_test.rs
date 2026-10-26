//! Tests for [`crate::mind::authority`].

use std::path::Path;

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

/// One row of the authority matrix: the spelling the envelope writes, the
/// stage that opens the action, whether it changes state, and the budget it
/// draws down.
struct Row {
    action: Action,
    kind: &'static str,
    stage: AuthorityStage,
    mutates: bool,
    budget: BudgetClass,
}

const TABLE: &[Row] = &[
    Row {
        action: Action::ReadStatus,
        kind: "read-status",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ReadLog,
        kind: "read-log",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ReadNeed,
        kind: "read-need",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ReadNeedThread,
        kind: "read-need-thread",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ReadTaskThread,
        kind: "read-task-thread",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ReadFlow,
        kind: "read-flow",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ReadConfig,
        kind: "read-config",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ReadPool,
        kind: "read-pool",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::LetterToOperator,
        kind: "letter-to-operator",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::Escalate,
        kind: "escalate",
        stage: AuthorityStage::Observe,
        mutates: false,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::PausePump,
        kind: "pause-pump",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::ResumePump,
        kind: "resume-pump",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::Requeue,
        kind: "requeue",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::Replan,
        kind: "replan",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::Answer,
        kind: "answer",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::Unlock,
        kind: "unlock",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::ReplyNeed,
        kind: "reply-need",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::SetPriority,
        kind: "set-priority",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::WithdrawNeed,
        kind: "withdraw-need",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::SetRoleModel,
        kind: "set-role-model",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerProject,
    },
    Row {
        action: Action::SetPoolCap,
        kind: "set-pool-cap",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerProject,
    },
    Row {
        action: Action::SetPoolBand,
        kind: "set-pool-band",
        stage: AuthorityStage::Unstick,
        mutates: true,
        budget: BudgetClass::PerProject,
    },
    Row {
        action: Action::FileNeed,
        kind: "file-need",
        stage: AuthorityStage::FileFixes,
        mutates: true,
        budget: BudgetClass::PerProject,
    },
    Row {
        action: Action::FileFixNeed,
        kind: "file-fix-need",
        stage: AuthorityStage::FileFixes,
        mutates: true,
        budget: BudgetClass::PerProject,
    },
    Row {
        action: Action::DispatchNeed,
        kind: "dispatch-need",
        stage: AuthorityStage::FileFixes,
        mutates: true,
        budget: BudgetClass::PerNeed,
    },
    Row {
        action: Action::Upgrade,
        kind: "upgrade",
        stage: AuthorityStage::UpgradeOnApproval,
        mutates: true,
        budget: BudgetClass::Unbudgeted,
    },
    Row {
        action: Action::SelfRelease,
        kind: "self-release",
        stage: AuthorityStage::SelfRelease,
        mutates: true,
        budget: BudgetClass::Unbudgeted,
    },
];

/// The number of `Action` variants. `as_str` matches on every variant, so a
/// new variant only compiles once it is named there; bump this count and add
/// the matching row in the same change.
const ACTION_COUNT: usize = 27;

#[test]
fn table_covers_every_action_exactly_once() {
    assert_eq!(
        TABLE.len(),
        ACTION_COUNT,
        "every Action variant needs exactly one row"
    );
    let mut kinds: Vec<&str> = TABLE.iter().map(|row| row.kind).collect();
    kinds.sort_unstable();
    let before = kinds.len();
    kinds.dedup();
    assert_eq!(kinds.len(), before, "no duplicate spellings in the table");
    let mut actions: Vec<Action> = TABLE.iter().map(|row| row.action).collect();
    actions.sort_unstable();
    actions.dedup();
    assert_eq!(actions.len(), TABLE.len(), "no duplicate rows in the table");
}

#[test]
fn action_table_agrees_on_spelling_stage_mutation_and_budget() {
    for row in TABLE {
        assert_eq!(row.action.as_str(), row.kind, "{}", row.kind);
        assert_eq!(
            serde_json::to_value(row.action).unwrap(),
            serde_json::Value::from(row.kind),
            "{}",
            row.kind
        );
        assert_eq!(row.action.required_stage(), row.stage, "{}", row.kind);
        assert_eq!(row.action.mutates(), row.mutates, "{}", row.kind);
        assert_eq!(row.action.budget(), row.budget, "{}", row.kind);
    }
}

#[test]
fn need_scope_covers_unstick_and_dispatch_interventions() {
    for row in TABLE {
        assert_eq!(
            row.action.need_scoped(),
            row.budget == BudgetClass::PerNeed,
            "{}",
            row.kind
        );
    }
}

#[test]
fn every_action_is_refused_below_its_stage() {
    let stages = [
        AuthorityStage::Observe,
        AuthorityStage::Unstick,
        AuthorityStage::FileFixes,
        AuthorityStage::UpgradeOnApproval,
        AuthorityStage::SelfRelease,
    ];
    let budgets = budgets();
    for row in TABLE {
        for stage in stages.iter().filter(|stage| **stage < row.stage) {
            let g = gate(*stage, &budgets);
            assert!(
                matches!(
                    permit(&g, &Ledger::new(), row.action, Some("n1")),
                    Err(Denial::Stage { .. })
                ),
                "{} refused at {}",
                row.kind,
                stage.as_str()
            );
        }
    }
}

#[test]
fn each_action_passes_at_its_own_stage() {
    let budgets = budgets();
    for row in TABLE {
        let g = gate(row.stage, &budgets);
        let result = permit(&g, &Ledger::new(), row.action, Some("n1"));
        if row.stage.implemented() {
            assert_eq!(result, Ok(()), "{}", row.kind);
        } else {
            assert_eq!(result, Err(Denial::NotImplemented), "{}", row.kind);
        }
    }
}

#[test]
fn higher_implemented_stages_retain_lower_powers() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let file_fixes = gate(AuthorityStage::FileFixes, &budgets);
    for action in [
        Action::Requeue,
        Action::Unlock,
        Action::ReplyNeed,
        Action::SetPoolCap,
    ] {
        assert_eq!(
            permit(&file_fixes, &ledger, action, Some("n1")),
            Ok(()),
            "{action:?}"
        );
    }
    for action in [Action::ReadLog, Action::ReadPool, Action::Escalate] {
        assert_eq!(
            permit(&file_fixes, &ledger, action, None),
            Ok(()),
            "{action:?}"
        );
    }
}

#[test]
fn paused_state_table_only_lets_reads_and_letters_through() {
    let budgets = budgets();
    for row in TABLE {
        let mut g = gate(row.stage, &budgets);
        g.paused = true;
        let result = permit(&g, &Ledger::new(), row.action, Some("n1"));
        if !row.mutates {
            assert_eq!(result, Ok(()), "{}", row.kind);
        } else if row.stage.implemented() {
            assert!(matches!(result, Err(Denial::Paused)), "{}", row.kind);
        } else {
            // The unimplemented-stage refusal comes before the pause check.
            assert_eq!(result, Err(Denial::NotImplemented), "{}", row.kind);
        }
    }
}

#[test]
fn observe_allows_reads_and_letters_only() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::Observe, &budgets);
    for action in [
        Action::ReadStatus,
        Action::ReadLog,
        Action::ReadNeed,
        Action::ReadNeedThread,
        Action::ReadTaskThread,
        Action::ReadFlow,
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
        Action::SetPriority,
        Action::WithdrawNeed,
        Action::SetRoleModel,
        Action::SetPoolCap,
        Action::SetPoolBand,
        Action::FileNeed,
        Action::FileFixNeed,
        Action::DispatchNeed,
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
        Action::ReplyNeed,
        Action::SetPriority,
        Action::WithdrawNeed,
        Action::SetRoleModel,
        Action::SetPoolCap,
        Action::SetPoolBand,
    ] {
        assert_eq!(
            permit(&g, &ledger, action, Some("n1")),
            Ok(()),
            "{action:?}"
        );
    }
    assert_eq!(permit(&g, &ledger, Action::PausePump, None), Ok(()));
    for action in [Action::FileNeed, Action::FileFixNeed, Action::DispatchNeed] {
        assert_eq!(
            permit(&g, &ledger, action, None),
            Err(Denial::Stage {
                required: AuthorityStage::FileFixes,
                current: AuthorityStage::Unstick,
            }),
            "{action:?}"
        );
    }
}

#[test]
fn file_fixes_opens_fix_needs_but_never_upgrades() {
    let (budgets, ledger) = (budgets(), Ledger::new());
    let g = gate(AuthorityStage::FileFixes, &budgets);
    for action in [Action::FileNeed, Action::FileFixNeed] {
        assert_eq!(permit(&g, &ledger, action, None), Ok(()), "{action:?}");
    }
    assert_eq!(
        permit(&g, &ledger, Action::DispatchNeed, Some("n1")),
        Ok(())
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
    assert_eq!(Action::ReadLog.required_stage(), AuthorityStage::Observe);
    assert_eq!(Action::Unlock.required_stage(), AuthorityStage::Unstick);
    assert_eq!(Action::ReplyNeed.required_stage(), AuthorityStage::Unstick);
    assert_eq!(
        Action::SetPoolBand.required_stage(),
        AuthorityStage::Unstick
    );
    assert_eq!(
        Action::FileFixNeed.required_stage(),
        AuthorityStage::FileFixes
    );
    assert_eq!(
        Action::DispatchNeed.required_stage(),
        AuthorityStage::FileFixes
    );
    assert_eq!(
        Action::Upgrade.required_stage(),
        AuthorityStage::UpgradeOnApproval
    );
    assert!(!Action::Escalate.mutates());
    assert!(!Action::ReadTaskThread.mutates());
    assert!(Action::PausePump.mutates());
    assert!(Action::WithdrawNeed.mutates());
    assert!(Action::FileNeed.mutates());
    assert!(!Action::PausePump.need_scoped());
    assert!(Action::SetPriority.need_scoped());
    assert!(Action::DispatchNeed.need_scoped());
    assert!(!Action::FileFixNeed.need_scoped());
}

#[test]
fn as_str_is_the_envelope_kind() {
    for action in TABLE.iter().map(|row| row.action) {
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            serde_json::Value::from(action.as_str())
        );
    }
}

#[test]
fn project_keys_live_in_a_reserved_namespace() {
    // The `project:` prefix carries `:` and path separators, characters the
    // handle validator refuses, so a project key can never collide with a
    // bare need handle.
    let key = project_key(Path::new("/w/app"));
    assert!(key.starts_with("project:"), "{key}");
    assert!(key.contains('/'), "{key}");
    // Bare-need entries keep their spelling: spend writes no prefix.
    let mut ledger = Ledger::new();
    spend(&mut ledger, "n1", NOW, 60);
    assert_eq!(ledger["n1"], vec![NOW]);
    assert!(ledger.get(&key).is_none());
}

#[test]
fn project_budget_is_shared_windowed_and_per_project() {
    let budgets = budgets();
    let mut ledger = Ledger::new();
    let g = gate(AuthorityStage::FileFixes, &budgets);
    let app = project_key(Path::new("/w/app"));
    spend(&mut ledger, &app, NOW - 10, budgets.per_need_window_secs);
    spend(&mut ledger, &app, NOW - 5, budgets.per_need_window_secs);
    assert_eq!(
        permit_project(&g, &ledger, Path::new("/w/app")),
        Err(Denial::ProjectBudget {
            project: app.clone(),
            used: 2,
            limit: 2,
        })
    );
    // Another watched project draws its own window.
    assert_eq!(permit_project(&g, &ledger, Path::new("/w/vivi")), Ok(()));
    // Outside the window the entries expire.
    let later = Gate {
        now: NOW + 3600,
        ..g
    };
    assert_eq!(permit_project(&later, &ledger, Path::new("/w/app")), Ok(()));
}

#[test]
fn project_budget_denial_is_distinct_from_the_need_denial() {
    let project = Denial::ProjectBudget {
        project: "project:/w/app".into(),
        used: 3,
        limit: 3,
    };
    assert_ne!(
        project,
        Denial::Budget {
            need: "project:/w/app".into(),
            used: 3,
            limit: 3,
        }
    );
    assert_eq!(project.to_string(), "project:/w/app budget spent (3/3)");
}

#[test]
fn legacy_bare_need_entries_still_gate_their_need() {
    let budgets = budgets();
    let mut ledger = Ledger::new();
    // Entries written before the project namespace existed are bare need
    // handles; they keep counting against the need's own window.
    ledger.insert(String::from("n1"), vec![NOW - 30, NOW - 20]);
    let g = gate(AuthorityStage::Unstick, &budgets);
    assert_eq!(
        permit(&g, &ledger, Action::Requeue, Some("n1")),
        Err(Denial::Budget {
            need: "n1".into(),
            used: 2,
            limit: 2,
        })
    );
    // The same legacy entries never touch a project's window.
    assert_eq!(permit_project(&g, &ledger, Path::new("/w/app")), Ok(()));
}
