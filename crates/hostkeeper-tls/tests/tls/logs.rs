//! What a look at the files logs: an operator reading the log learns which listener took new
//! files and which one refused them.

use std::io;
use std::sync::{Arc, Mutex};

use hostkeeper_tls::Outcome;

use crate::support::{Files, ca, put, server_leaf};

/// The log lines written while a test runs.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("lock").clone()).expect("UTF-8")
    }
}

/// A refused reload logs a warning and an applied one an information line, each naming the
/// listener; a look that finds the files as they were logs nothing.
///
/// Catches: a reload whose outcome reaches no log, or a log line that does not say which
/// listener it is about.
#[test]
fn a_reload_that_changes_something_logs_its_outcome_and_its_listener() {
    let ca = ca("log CA");
    let (old, new) = (server_leaf(&ca), server_leaf(&ca));
    let files = Files::write("logs", &old, None);
    let source = files.source();
    let log = Captured::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        put(&files.files.cert, &new.cert);
        assert_eq!(source.reload(), Outcome::Rejected);
        files.install(&new);
        assert_eq!(source.reload(), Outcome::Applied);
        assert_eq!(source.reload(), Outcome::Unchanged);
    });
    let text = log.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains("WARN"), "{text}");
    assert!(
        lines[0].contains("TLS files unusable; previous ones kept"),
        "{text}"
    );
    assert!(lines[0].contains("listener=\"test\""), "{text}");
    assert!(lines[1].contains("INFO"), "{text}");
    assert!(lines[1].contains("reloaded the TLS files"), "{text}");
    assert!(lines[1].contains("listener=\"test\""), "{text}");
}
