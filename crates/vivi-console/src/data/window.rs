//! How far back a page looks: the one window menu the failures and jobs
//! pages share.

/// How far back a page reaches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Window {
    Hour,
    #[default]
    Day,
    Week,
}

impl Window {
    /// Every window, in menu order.
    pub const ALL: [Window; 3] = [Window::Hour, Window::Day, Window::Week];

    /// The query-string word: `1h`, `24h` or `7d`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    /// The window a query-string word names; anything else is `None`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|window| window.word() == word)
    }

    /// Length in seconds.
    #[must_use]
    pub fn secs(self) -> i64 {
        match self {
            Self::Hour => 3600,
            Self::Day => 24 * 3600,
            Self::Week => 7 * 24 * 3600,
        }
    }
}

#[cfg(test)]
#[path = "window_test.rs"]
mod tests;
