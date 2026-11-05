//! The last line a hook step writes to standard output: one JSON object the updater reads.
//!
//! The line is built by hand (the program has no dependencies) with the keys in the order the
//! contract prints them: `class`, `reversible`, `promotion_pending`, `reason`,
//! `retry_after_secs`, `detail`; a key that is not set is left out.

/// Whom a failure is blamed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The release is wrong (the default).
    Release,
    /// The host or something shared failed; the release may be fine.
    Environment,
}

impl Class {
    /// The word in the JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Class::Release => "release",
            Class::Environment => "environment",
        }
    }
}

/// What a step tells the updater besides its exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub class: Class,
    /// Stated only when the step says whether it can still be undone.
    pub reversible: Option<bool>,
    /// Stated only by `verify`.
    pub promotion_pending: Option<bool>,
    /// A machine-readable token of `[a-z0-9_]`.
    pub reason: Option<&'static str>,
    /// With exit 75: when to run the step again.
    pub retry_after_secs: Option<u64>,
    /// Text for the log.
    pub detail: Option<String>,
}

impl Report {
    /// A report of class `class` with nothing else stated.
    pub const fn new(class: Class) -> Report {
        Report {
            class,
            reversible: None,
            promotion_pending: None,
            reason: None,
            retry_after_secs: None,
            detail: None,
        }
    }

    /// A report of class `release`.
    pub const fn release() -> Report {
        Report::new(Class::Release)
    }

    /// A report of class `environment`.
    pub const fn environment() -> Report {
        Report::new(Class::Environment)
    }

    /// Sets `detail`.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Report {
        self.detail = Some(detail.into());
        self
    }

    /// The JSON object, without a newline.
    pub fn line(&self) -> String {
        let mut line = format!("{{\"class\":\"{}\"", self.class.as_str());
        if let Some(reversible) = self.reversible {
            line.push_str(&format!(",\"reversible\":{reversible}"));
        }
        if let Some(pending) = self.promotion_pending {
            line.push_str(&format!(",\"promotion_pending\":{pending}"));
        }
        if let Some(reason) = self.reason {
            line.push_str(&format!(",\"reason\":{}", quote(reason)));
        }
        if let Some(secs) = self.retry_after_secs {
            line.push_str(&format!(",\"retry_after_secs\":{secs}"));
        }
        if let Some(detail) = &self.detail {
            line.push_str(&format!(",\"detail\":{}", quote(detail)));
        }
        line.push('}');
        line
    }
}

/// A step that ended without success: the exit code and what it reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: i32,
    pub report: Report,
}

impl Failure {
    /// Exit 1, class `release`, with `detail`.
    pub fn release(detail: impl Into<String>) -> Failure {
        Failure {
            code: 1,
            report: Report::release().with_detail(detail),
        }
    }

    /// Exit 1, class `environment`, with `detail`.
    pub fn environment(detail: impl Into<String>) -> Failure {
        Failure {
            code: 1,
            report: Report::environment().with_detail(detail),
        }
    }

    /// The same failure with a `reason` token.
    pub fn with_reason(mut self, reason: &'static str) -> Failure {
        self.report.reason = Some(reason);
        self
    }
}

/// `text` as a JSON string, quotes included.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' || c == '\u{7f}' => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests;
