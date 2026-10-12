use super::*;

#[test]
fn kinds_file_into_work_folders_and_unknown_kinds_degrade_to_inbox() {
    assert_eq!(role_for(Some("need")), "needs");
    assert_eq!(role_for(Some("TASK")), "tasks");
    assert_eq!(role_for(Some("want")), "wants");
    assert_eq!(role_for(None), "inbox");
    assert_eq!(role_for(Some("mail")), "inbox");
    assert_eq!(role_for(Some("proposal")), "inbox");
    assert_eq!(work_kind(Some("proposal")), None);
}
