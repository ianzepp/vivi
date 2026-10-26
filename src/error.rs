//! Error type for the mailspace crate.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum VivariumError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("message error: {0}")]
    Message(String),

    #[error("{0}")]
    Other(String),

    #[error("job brief incomplete: {0}")]
    JobBriefIncomplete(String),

    #[error("job brief corrupt: {0}")]
    JobBriefCorrupt(String),
}

impl VivariumError {
    /// True for [`VivariumError::JobBriefIncomplete`]: a multipart job brief
    /// whose manifest arrived intact but whose part bodies did not all
    /// arrive. Classify with this predicate, never by matching the message.
    #[must_use]
    pub fn is_job_brief_incomplete(&self) -> bool {
        matches!(self, Self::JobBriefIncomplete(_))
    }

    /// True for [`VivariumError::JobBriefCorrupt`]: a multipart job brief
    /// body that is unreadable, inconsistent or fails a hash or length
    /// check. Classify with this predicate, never by matching the message.
    #[must_use]
    pub fn is_job_brief_corrupt(&self) -> bool {
        matches!(self, Self::JobBriefCorrupt(_))
    }

    /// The stable classification name of a job-brief error,
    /// `job-brief-incomplete` or `job-brief-corrupt`, for reports; `None`
    /// for every other error.
    #[must_use]
    pub fn job_brief_class(&self) -> Option<&'static str> {
        match self {
            Self::JobBriefIncomplete(_) => Some("job-brief-incomplete"),
            Self::JobBriefCorrupt(_) => Some("job-brief-corrupt"),
            _ => None,
        }
    }
}
