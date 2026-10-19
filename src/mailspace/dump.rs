use std::collections::HashSet;

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use super::kind::{effective_kind, events_kind, matches_kind, matches_kind_events};
use super::{MailAbsorbFilter, Mailspace, canonical_local_role};
use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, StoredMessageView};

#[derive(Debug, Clone, Default)]
pub struct DumpFilters {
    pub for_identity: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub participant: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub since: Option<String>,
    pub before: Option<String>,
    pub absorb_status: MailAbsorbFilter,
    pub absorbed_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MailDumpRequest {
    pub folder: String,
    pub kind: Option<String>,
    pub filters: DumpFilters,
}

#[derive(Debug, Clone)]
pub struct TaskDumpRequest {
    pub status: TaskDumpStatus,
    pub open_role: String,
    pub kind: String,
    /// Also list the sender's own open copies of work addressed to another
    /// mailspace (role `sent`) for an open or all-status dump.
    pub include_outbound: bool,
    pub filters: DumpFilters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskDumpStatus {
    Open,
    Done,
    All,
}

#[derive(Debug, Clone, Serialize)]
pub struct DumpRecord {
    pub handle: String,
    pub message_id: String,
    pub account: String,
    pub role: String,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub date: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    pub parent_content_id: Option<String>,
    pub link_source: Option<String>,
    pub events: Vec<MailspaceEvent>,
}

struct PreparedFilters {
    account: Option<HashSet<String>>,
    from: Option<String>,
    to: Option<String>,
    participant: Option<ParticipantFilter>,
    subject: Option<String>,
    body: Option<String>,
    absorb_status: MailAbsorbFilter,
    absorbed_by: Option<String>,
    window: DumpWindow,
}

struct ParticipantFilter {
    identity: Option<HashSet<String>>,
    text: String,
}

#[derive(Default)]
struct DumpWindow {
    since: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
}

impl Mailspace {
    /// Dump mail records matching the given folder and filters.
    ///
    /// # Errors
    /// Returns an error if the folder is invalid, filter resolution fails, or a
    /// storage operation fails.
    pub fn dump_mail(&self, request: MailDumpRequest) -> Result<Vec<DumpRecord>, VivariumError> {
        let roles = mail_roles(&request.folder)?;
        self.dump_records(&roles, None, request.kind.as_deref(), request.filters)
    }

    /// Dump task records matching the given status and filters.
    ///
    /// # Errors
    /// Returns an error if the role is invalid, filter resolution fails, or a
    /// storage operation fails.
    pub fn dump_tasks(&self, request: TaskDumpRequest) -> Result<Vec<DumpRecord>, VivariumError> {
        let mut roles = status_roles(&request.open_role, request.status);
        let outbound = request.include_outbound && request.status != TaskDumpStatus::Done;
        if outbound {
            roles.push("sent".into());
        }
        let mut records = self.dump_records(
            &roles,
            Some((request.status, request.open_role.as_str())),
            Some(&request.kind),
            request.filters,
        )?;
        if outbound {
            records.retain(|record| {
                record.role != "sent" || self.addresses_include_remote(&[&record.to, &record.cc])
            });
        }
        Ok(records)
    }

    fn dump_records(
        &self,
        roles: &[String],
        status: Option<(TaskDumpStatus, &str)>,
        kind: Option<&str>,
        filters: DumpFilters,
    ) -> Result<Vec<DumpRecord>, VivariumError> {
        let prepared = self.prepare_filters(filters)?;
        let storage = self.storage()?;
        let views = if let Some(account_names) = &prepared.account {
            let account_list: Vec<String> = account_names.iter().cloned().collect();
            storage.list_messages_by_account_roles(&account_list, roles)?
        } else if roles.len() <= 4 {
            let mut all = Vec::new();
            for role in roles {
                all.extend(storage.list_messages_by_role(role)?);
            }
            all
        } else {
            storage.list_messages()?
        };
        if views.is_empty() {
            return Ok(Vec::new());
        }

        let message_ids: Vec<String> = views.iter().map(|v| v.message_id.clone()).collect();
        let content_ids: Vec<String> = views.iter().map(|v| v.content_id.clone()).collect();
        let events_by_msg = storage.list_mailspace_events_for_messages(&message_ids)?;
        let links_by_content = storage.list_mailspace_links_for_children(&content_ids)?;

        let mut records = Vec::new();
        for view in views {
            let events = events_by_msg
                .get(&view.message_id)
                .cloned()
                .unwrap_or_default();
            let Some(record) = Self::view_to_record(
                &storage,
                view,
                status,
                kind,
                &events,
                &links_by_content,
                &prepared,
            )?
            else {
                continue;
            };
            records.push(record);
        }
        Ok(records)
    }

    fn view_to_record(
        storage: &crate::storage::Storage,
        view: StoredMessageView,
        status: Option<(TaskDumpStatus, &str)>,
        kind: Option<&str>,
        events: &[MailspaceEvent],
        links_by_content: &std::collections::HashMap<String, crate::storage::MailspaceLink>,
        filters: &PreparedFilters,
    ) -> Result<Option<DumpRecord>, VivariumError> {
        let role_determined = matches!(
            view.local_role.as_str(),
            "tasks" | "needs" | "wants" | "memos"
        );
        let kind_needs_blob = kind.is_some_and(|k| !(role_determined && k != "mail"));
        let body_needed = filters.body.is_some();
        let needs_blob = kind_needs_blob || body_needed;

        // Apply non-blob filters first to avoid unnecessary reads
        if matches!(status, Some((TaskDumpStatus::Open, _))) && view.absorbed_at.is_some() {
            return Ok(None);
        }
        if !matches_filters_header(&view, events, filters) {
            return Ok(None);
        }

        let Some((message_kind, body)) = Self::record_blob_fields(
            storage,
            &view,
            kind,
            events,
            filters,
            needs_blob,
            body_needed,
        )?
        else {
            return Ok(None);
        };

        let link = links_by_content.get(&view.content_id);
        Ok(Some(DumpRecord {
            events: events.to_vec(),
            handle: view.handle,
            message_id: view.message_id,
            account: view.account,
            role: view.local_role.clone(),
            kind: message_kind,
            status: status.and_then(|(_, open_role)| status_for_role(&view.local_role, open_role)),
            date: view.date,
            from: view.from_addr,
            to: view.to_addr,
            cc: view.cc_addr,
            subject: view.subject,
            body,
            parent_content_id: link.map(|l| l.parent_content_id.clone()),
            link_source: link.map(|l| l.source.clone()),
        }))
    }

    fn record_blob_fields(
        storage: &crate::storage::Storage,
        view: &StoredMessageView,
        kind: Option<&str>,
        events: &[MailspaceEvent],
        filters: &PreparedFilters,
        needs_blob: bool,
        body_needed: bool,
    ) -> Result<Option<(Option<String>, String)>, VivariumError> {
        // Decide the kind filter from the role and event log before any blob
        // read: filed work items carry their send/done events, so `done` and
        // `sent` rows no longer open and parse every body just to learn a
        // kind the log already names. Only an undecided filter or a body
        // filter falls through to the blob.
        if let Some(kind) = kind {
            if !body_needed {
                if matches_kind_events(&view.local_role, events, kind) {
                    return Ok(Some((events_kind(&view.local_role, events), String::new())));
                }
                if events_kind(&view.local_role, events).is_some() || events.is_empty() {
                    // The log names a different kind, or there is no log at
                    // all (only vivi-filed items carry headers, and they log).
                    return Ok(None);
                }
            }
        }
        if !needs_blob {
            return Ok(Some((
                effective_kind(&view.local_role, &[], events),
                String::new(),
            )));
        }
        let data = storage.read_message(&view.message_id)?;
        if kind.is_some_and(|k| !matches_kind(&view.local_role, &data, events, k)) {
            return Ok(None);
        }
        let body = if body_needed {
            text_body(&data)
        } else {
            String::new()
        };
        if body_needed && !matches_text(&body, filters.body.as_deref()) {
            return Ok(None);
        }
        Ok(Some((
            effective_kind(&view.local_role, &data, events),
            body,
        )))
    }

    fn prepare_filters(&self, filters: DumpFilters) -> Result<PreparedFilters, VivariumError> {
        // Resolve --for and --participant to account name sets for SQL filtering
        let account_from_for = filters
            .for_identity
            .as_deref()
            .map(|identity| self.resolve_identity(identity))
            .transpose()?
            .map(|identity| self.identity_names(&identity));

        // If --participant resolves to a known identity, use it for SQL filtering too
        let participant_filter = self.participant_filter(filters.participant.as_deref());
        let account_from_participant = participant_filter
            .as_ref()
            .and_then(|pf| pf.identity.clone());

        let account = account_from_for.or(account_from_participant);

        Ok(PreparedFilters {
            account,
            from: filters.from.map(normalize_filter),
            to: filters.to.map(normalize_filter),
            participant: participant_filter,
            subject: filters.subject.map(normalize_filter),
            body: filters.body.map(normalize_filter),
            absorb_status: filters.absorb_status,
            absorbed_by: filters.absorbed_by,
            window: DumpWindow::parse(filters.since.as_deref(), filters.before.as_deref())?,
        })
    }

    fn participant_filter(&self, participant: Option<&str>) -> Option<ParticipantFilter> {
        let value = participant?;
        let resolved = match self.resolve_identity(value) {
            Ok(identity) => Some(identity),
            Err(_) => None,
        };
        let text = resolved
            .as_deref()
            .map_or_else(|| value.to_string(), |identity| self.address_for(identity));
        let identity = resolved.map(|identity| self.identity_names(&identity));
        Some(ParticipantFilter {
            identity,
            text: normalize_filter(text),
        })
    }
}

/// Match filters that do not require the blob body.
fn matches_filters_header(
    view: &StoredMessageView,
    events: &[MailspaceEvent],
    filters: &PreparedFilters,
) -> bool {
    filters
        .account
        .as_ref()
        .is_none_or(|names| names.contains(&view.account))
        && filters.window.contains(&view.date)
        && matches_text(&view.from_addr, filters.from.as_deref())
        && matches_recipients(view, filters.to.as_deref())
        && matches_text(&view.subject, filters.subject.as_deref())
        && matches_participant(view, filters.participant.as_ref())
        && matches_absorb(view, events, filters)
}

fn matches_recipients(view: &StoredMessageView, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    matches_text(&view.to_addr, Some(filter)) || matches_text(&view.cc_addr, Some(filter))
}

fn matches_participant(view: &StoredMessageView, filter: Option<&ParticipantFilter>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    filter
        .identity
        .as_ref()
        .is_some_and(|names| names.contains(&view.account))
        || matches_text(&view.from_addr, Some(&filter.text))
        || matches_text(&view.to_addr, Some(&filter.text))
        || matches_text(&view.cc_addr, Some(&filter.text))
        || matches_text(&view.bcc_addr, Some(&filter.text))
}

fn matches_text(value: &str, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| value.to_ascii_lowercase().contains(filter))
}

fn matches_absorb(
    view: &StoredMessageView,
    events: &[MailspaceEvent],
    filters: &PreparedFilters,
) -> bool {
    let absorbed = view.absorbed_at.is_some()
        || events
            .iter()
            .any(|event| event.event_type == "absorbed" || event.command.ends_with(" absorb"));
    let status_matches = match filters.absorb_status {
        MailAbsorbFilter::All => true,
        MailAbsorbFilter::Absorbed => absorbed,
        MailAbsorbFilter::Unabsorbed => !absorbed,
    };
    status_matches
        && filters.absorbed_by.as_ref().is_none_or(|identity| {
            view.absorbed_by.as_ref() == Some(identity)
                || events.iter().any(|event| {
                    (event.event_type == "absorbed" || event.command.ends_with(" absorb"))
                        && event.actor_identity.as_ref() == Some(identity)
                })
        })
}

fn mail_roles(folder: &str) -> Result<Vec<String>, VivariumError> {
    if folder.eq_ignore_ascii_case("all") {
        return Ok(["inbox", "sent", "archive", "trash", "drafts"]
            .into_iter()
            .map(str::to_string)
            .collect());
    }
    canonical_local_role(folder).map(|role| vec![role])
}

fn status_roles(open_role: &str, status: TaskDumpStatus) -> Vec<String> {
    match status {
        TaskDumpStatus::Open => vec![open_role.into()],
        TaskDumpStatus::Done => vec!["done".into()],
        TaskDumpStatus::All => vec![open_role.into(), "done".into()],
    }
}

fn status_for_role(role: &str, open_role: &str) -> Option<String> {
    if role == open_role || role == "sent" {
        Some("open".into())
    } else if role == "done" {
        Some("done".into())
    } else {
        None
    }
}

fn text_body(data: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(data)
        .and_then(|parsed| parsed.body_text(0).map(|body| body.to_string()))
        .unwrap_or_default()
}

fn normalize_filter(value: impl AsRef<str>) -> String {
    value.as_ref().to_ascii_lowercase()
}

impl DumpWindow {
    fn parse(since: Option<&str>, before: Option<&str>) -> Result<Self, VivariumError> {
        Ok(Self {
            since: since.map(parse_time_bound).transpose()?,
            before: before.map(parse_time_bound).transpose()?,
        })
    }

    fn contains(&self, raw_date: &str) -> bool {
        let Some(date) = parse_message_date(raw_date) else {
            return self.since.is_none() && self.before.is_none();
        };
        self.since.is_none_or(|since| date >= since)
            && self.before.is_none_or(|before| date < before)
    }
}

/// Parse a time bound string into a `DateTime<Utc>`. Accepts RFC3339,
/// `YYYY-MM-DD`, or relative forms (`Nh`, `Nd`, `Nw`).
///
/// # Errors
/// Returns an error if the value cannot be parsed as a recognised time format.
///
/// # Panics
/// Never panics. The `0, 0, 0` arguments to `and_hms_opt` are always valid.
pub fn parse_time_bound(value: &str) -> Result<DateTime<Utc>, VivariumError> {
    if let Some(date) = relative_time_bound(value)? {
        return Ok(date);
    }
    if let Ok(date) = DateTime::parse_from_rfc3339(value) {
        return Ok(date.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
        VivariumError::Config(format!(
            "invalid time '{value}', expected RFC3339, YYYY-MM-DD, Nh, Nd, or Nw"
        ))
    })?;
    // 0, 0, 0 is always a valid time for and_hms_opt.
    let Some(dt) = date.and_hms_opt(0, 0, 0) else {
        return Err(VivariumError::Config(format!("invalid date '{value}'")));
    };
    Local
        .from_local_datetime(&dt)
        .single()
        .map(|date| date.with_timezone(&Utc))
        .ok_or_else(|| VivariumError::Config(format!("ambiguous local date '{value}'")))
}

fn relative_time_bound(value: &str) -> Result<Option<DateTime<Utc>>, VivariumError> {
    let now = Utc::now();
    if let Some(count) = parse_suffix(value, "h")? {
        return Ok(Some(now - Duration::hours(count)));
    }
    if let Some(count) = parse_suffix(value, "d")? {
        return Ok(Some(now - Duration::days(count)));
    }
    if let Some(count) = parse_suffix(value, "w")? {
        return Ok(Some(now - Duration::weeks(count)));
    }
    Ok(None)
}

fn parse_suffix(value: &str, suffix: &str) -> Result<Option<i64>, VivariumError> {
    let Some(number) = value.strip_suffix(suffix) else {
        return Ok(None);
    };
    number.parse::<i64>().map(Some).map_err(|_| {
        VivariumError::Config(format!(
            "invalid relative time '{value}', expected Nh, Nd, or Nw"
        ))
    })
}

fn parse_message_date(raw_date: &str) -> Option<DateTime<Utc>> {
    match DateTime::parse_from_rfc3339(raw_date) {
        Ok(date) => Some(date.with_timezone(&Utc)),
        Err(_) => None,
    }
}
