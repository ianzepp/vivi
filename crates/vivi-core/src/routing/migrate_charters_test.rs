//! `vivi config migrate` replaces 19.x pump-role charters with the shipped ones.

use crate::mailspace::MailspaceConfig;
use crate::mailspace::shipped::{MERGE_CHARTER, PUMP_PLANNER_CHARTER};
use crate::routing::{CharterWrite, MigrateInputs, MigratePlan, plan_migrate, render_plan};

const BASE: &str = r#"
name = "t"
id = "11111111-2222-3333-4444-555555555555"

[[identities]]
name = "hand"
kind = "hand"
status = "active"
pool = "glm"

[[identities]]
name = "merge"
kind = "merge"
status = "active"
pool = "glm"

[[identities]]
name = "pump-planner"
kind = "planner"
status = "active"
pool = "glm"

[pools.glm]
provider = "local"
slots = 4

[pump]
branch = "next"
to = "hand"
"#;

const OLD: &str = "Plan the work.\nBetween PUMP-PLAN-BEGIN and PUMP-PLAN-END.\n";
const V20: &str = "Reply with:\n+++\nanswer = \"done\"\n+++\n";

fn plan_for(charters: &[(&str, &str)]) -> MigratePlan {
    let config: MailspaceConfig = toml::from_str(BASE).expect("toml");
    let mut inputs = MigrateInputs::default();
    for (role, text) in charters {
        inputs.charters.insert((*role).into(), (*text).into());
    }
    plan_migrate(&config, &BASE.parse().expect("document"), &inputs)
}

fn writes<'a>(plan: &'a MigratePlan, role: &str) -> Vec<&'a CharterWrite> {
    plan.charters.iter().filter(|w| w.role == role).collect()
}

#[test]
fn old_charters_of_pump_roles_get_the_shipped_text() {
    let plan = plan_for(&[("pump-planner", OLD), ("merge", "Merge it.\n")]);
    let planner = writes(&plan, "pump-planner");
    assert_eq!(planner.len(), 1, "{plan:?}");
    assert_eq!(planner[0].text, PUMP_PLANNER_CHARTER);
    let merge = writes(&plan, "merge");
    assert_eq!(merge.len(), 1, "{plan:?}");
    assert_eq!(merge[0].text, MERGE_CHARTER);
    assert!(merge[0].origin.contains("no v20 reply envelope"));
}

#[test]
fn v20_and_blank_charters_get_no_write_from_this_rule() {
    let plan = plan_for(&[("pump-planner", V20), ("merge", "  \n")]);
    assert!(writes(&plan, "pump-planner").is_empty(), "{plan:?}");
    assert!(writes(&plan, "merge").is_empty(), "{plan:?}");
}

#[test]
fn render_plan_names_the_stale_charter_write() {
    let plan = plan_for(&[("merge", "Merge it.\n")]);
    let text = render_plan(&plan);
    assert!(
        text.contains(
            "set role merge charter from the shipped merge charter \
             (the current one has no v20 reply envelope)"
        ),
        "{text}"
    );
}
