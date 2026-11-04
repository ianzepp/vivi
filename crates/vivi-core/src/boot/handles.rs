//! Handle inventory: verdicts and the triage manifest.
//!
//! Every open task, need, and want is a handle. Boot reports each with a
//! verdict drawn from a closed vocabulary, so the reader routes without
//! re-deriving anything. The triage manifest then slices the undispaced
//! backlog into seat-sized groups.

use serde::Serialize;

use super::format::age_label;
use super::goals::cap_chars;
use super::probe::ProbeVerdict;
use super::truncate::{CAP_TRIAGE_SUBJECTS, TRIAGE_SLICE};

/// Minimum length of a handle prefix a probe may use instead of the full handle.
const MIN_PREFIX: usize = 4;

/// Closed verdict vocabulary. Every variant has a producer and one
/// deterministic test, so a reader can route on a verdict without re-deriving
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Plain open work. Test: the handle is open and no other verdict applied.
    Open,
    /// A backlog-graph prerequisite is unmet. Test: the graph lists the node blocked.
    Blocked,
    /// The landing is already visible in the tree. Test: a probe said so.
    Stale,
    /// Process confirmed alive on this host. Test: local pid probe reports alive.
    Live,
    /// Active seat with no bound process. Test: no pid and no stored host.
    Unbound,
    /// A subagent harness owns liveness. Test: the role's harness is `subagent`.
    Unverified,
    /// Bound pid is gone. Test: local probe finds no such process.
    Dead,
    /// Terminated but unreaped. Test: local probe reports a zombie.
    Zombie,
    /// Bound to another host. Test: stored host differs from local.
    Remote,
    /// The host could not classify the bound process. Test: local probe
    /// reports an unclassified result.
    Unknown,
}

impl Verdict {
    /// Stable lowercase slug, as it appears in the digest.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Blocked => "blocked",
            Self::Stale => "stale",
            Self::Live => "live",
            Self::Unbound => "unbound",
            Self::Unverified => "unverified",
            Self::Dead => "dead",
            Self::Zombie => "zombie",
            Self::Remote => "remote",
            Self::Unknown => "unknown",
        }
    }
}

/// One open handle with its derived verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandleRow {
    pub handle: String,
    pub kind: String,
    pub age: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl HandleRow {
    /// Build a row from the fields every dumped record carries.
    #[must_use]
    pub fn new(
        handle: &str,
        kind: &str,
        date: &str,
        from: &str,
        to: &str,
        subject: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            handle: handle.to_string(),
            kind: kind.to_string(),
            age: age_label(date, now),
            from: from.to_string(),
            to: to.to_string(),
            subject: cap_chars(subject, 96),
            verdict: Verdict::Open,
            detail: None,
        }
    }
}

/// Apply probe verdicts and graph blockers. Probe handles match the full
/// handle or a unique prefix, so a probe may report either.
pub fn apply_verdicts(rows: &mut [HandleRow], verdicts: &[ProbeVerdict], blocked: &[String]) {
    for row in rows.iter_mut() {
        if let Some(detail) = blocker_detail(row, blocked) {
            row.verdict = Verdict::Blocked;
            row.detail = Some(detail);
            continue;
        }
        if let Some(verdict) = probe_verdict(row, verdicts) {
            row.verdict = parse_verdict(&verdict.verdict).unwrap_or(row.verdict);
            row.detail = verdict.detail.clone();
        }
    }
}

fn blocker_detail(row: &HandleRow, blocked: &[String]) -> Option<String> {
    blocked
        .iter()
        .any(|handle| matches(row, handle))
        .then(|| "graph prerequisite unmet".to_string())
}

fn probe_verdict<'a>(row: &HandleRow, verdicts: &'a [ProbeVerdict]) -> Option<&'a ProbeVerdict> {
    verdicts.iter().find(|v| matches(row, &v.handle))
}

fn matches(row: &HandleRow, candidate: &str) -> bool {
    if row.handle == candidate {
        return true;
    }
    candidate.len() >= MIN_PREFIX && row.handle.starts_with(candidate)
}

/// Map a probe's verdict slug onto the closed vocabulary. An unknown slug
/// leaves the row's existing verdict in place rather than inventing one.
#[must_use]
pub fn parse_verdict(slug: &str) -> Option<Verdict> {
    match slug.trim().to_lowercase().as_str() {
        "open" => Some(Verdict::Open),
        "blocked" => Some(Verdict::Blocked),
        "stale" => Some(Verdict::Stale),
        "live" => Some(Verdict::Live),
        "unbound" => Some(Verdict::Unbound),
        "unverified" => Some(Verdict::Unverified),
        "dead" => Some(Verdict::Dead),
        "zombie" => Some(Verdict::Zombie),
        "remote" => Some(Verdict::Remote),
        "unknown" => Some(Verdict::Unknown),
        _ => None,
    }
}

/// One seat-sized slice of the undispaced backlog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageSlice {
    pub index: usize,
    pub size: usize,
    pub handles: Vec<String>,
    /// First few subjects, so a reader knows the slice without opening it.
    pub subjects: Vec<String>,
}

/// Slice the open needs and wants into seat-sized groups. Tasks are already
/// dispatched work and stay out of the backlog slices; blocked and stale rows
/// are excluded because they need disposition before dispatch.
#[must_use]
pub fn triage_slices(rows: &[HandleRow]) -> Vec<TriageSlice> {
    let mut backlog: Vec<&HandleRow> = rows
        .iter()
        .filter(|row| row.kind == "need" || row.kind == "want")
        .filter(|row| matches!(row.verdict, Verdict::Open))
        .collect();
    backlog.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.handle.cmp(&b.handle)));
    backlog
        .chunks(TRIAGE_SLICE)
        .enumerate()
        .map(|(index, chunk)| TriageSlice {
            index: index + 1,
            size: chunk.len(),
            handles: chunk.iter().map(|row| row.handle.clone()).collect(),
            subjects: chunk
                .iter()
                .take(CAP_TRIAGE_SUBJECTS)
                .map(|row| row.subject.clone())
                .collect(),
        })
        .collect()
}
