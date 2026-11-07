//! The log of `serve`: one line per event, the unix seconds of the injected clock, a space, and
//! the text. Standard error on a host; a test reads the lines as they come.
//!
//! The lines are the record of a run that the design's measurement reads (section 11, item N11):
//! the first DISCOVER or REQUEST of a MAC, the offer sent, the seed fetched.

use std::fmt::Display;
use std::sync::Arc;

/// The wall clock, in unix seconds.
pub type Now = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Where each line goes, without its newline.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// The log of one run.
#[derive(Clone)]
pub struct Log {
    now: Now,
    sink: Sink,
}

impl Log {
    /// A log stamped by `now`, written to `sink`.
    pub fn new(now: Now, sink: Sink) -> Log {
        Log { now, sink }
    }

    /// Writes `text` as one line.
    pub fn line(&self, text: impl Display) {
        (self.sink)(&format!("{} {text}", (self.now)()));
    }
}
