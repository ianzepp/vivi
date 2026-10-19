use chrono::Utc;

use super::Mailspace;
use crate::error::VivariumError;

pub(super) fn prune_retained_memos(
    mailspace: &Mailspace,
    identity: &str,
    new_tags: &[String],
) -> Result<(), VivariumError> {
    let mut windows: Vec<(String, u64)> = Vec::new();
    for tag in new_tags {
        if windows
            .iter()
            .any(|(seen, _)| seen.eq_ignore_ascii_case(tag.trim()))
        {
            continue;
        }
        if let Some(secs) = super::super::memos::retain_secs(&mailspace.config.memo, tag) {
            windows.push((tag.trim().to_string(), secs));
        }
    }
    if windows.is_empty() {
        return Ok(());
    }
    let memos = mailspace.list_kind(Some(identity), "memos", "memo")?;
    let storage = mailspace.storage()?;
    let mut tagged = Vec::with_capacity(memos.len());
    for memo in memos {
        let data = storage.read_message(&memo.message_id)?;
        tagged.push((memo, super::super::memos::parse_tags(&data)));
    }
    drop(storage);
    let now = Utc::now();
    for (tag, secs) in windows {
        for handle in super::super::memos::expired_handles(&tagged, &tag, secs, now) {
            match mailspace.delete_memo(identity, &handle) {
                Ok(_) => {}
                Err(VivariumError::Message(error)) if error.starts_with("message not found:") => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}
