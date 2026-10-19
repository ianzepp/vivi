//! Scope evidence and no-attempt handoff to a need revision.

use std::path::{Component, Path};

use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};

/// Park a defective unit for a need revision without recording another hand
/// attempt. The dispatch counter is retained so its branch number cannot be
/// reused when the revised unit is dispatched.
pub(crate) fn needs_replan(ctx: &Ctx, st: &mut State, uid: &str, reason: &str, detail: &str) {
    let need = match st.units.get_mut(uid) {
        Some(unit) => {
            unit.status = Status::NeedsReplan;
            unit.since = ctx.now();
            unit.task = None;
            unit.need.clone()
        }
        None => return,
    };
    let text = crate::pump::collect::tail(detail, 300);
    ctx.exception("needs-replan", uid, &text);
    ctx.log(&format!("needs-replan {uid}: {reason}"));
    if st.needs.contains_key(&need) {
        crate::pump::amend::request(
            ctx,
            st,
            &need,
            &format!("unit {uid}: {reason}; revise its brief, write scope, or proof"),
        );
    }
}

/// The one structured scope diagnosis a hand may include in its final
/// message. A marker only counts when its path is relative and outside scope.
pub(crate) fn out_of_scope_marker(message: &str, write_scope: &[String]) -> Option<String> {
    message.lines().find_map(|line| {
        let path = line.trim().strip_prefix("OUT-OF-SCOPE: ")?.trim();
        let candidate = Path::new(path);
        if path.is_empty()
            || path.contains('\\')
            || !candidate
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return None;
        }
        let path = candidate.to_string_lossy().into_owned();
        (!crate::pump::plan::overlaps(&[path.clone()], write_scope)).then_some(path)
    })
}
