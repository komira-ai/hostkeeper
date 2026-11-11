//! The book of the alerts: an alert is logged once, when it first appears, and keeps the time it
//! was first observed until it ends.

use super::{Alert, Book, Kind, Observed, Subject};
use crate::testutil::Log;

/// The unix time of the coordinator's clock at 0.
const UNIX: u64 = 1_800_000_000;

fn subject(kind: Kind, service: &str, host: &str, version: &str) -> Subject {
    Subject {
        kind,
        service: service.to_owned(),
        host: host.to_owned(),
        version: version.to_owned(),
        request: String::new(),
        other_host: String::new(),
    }
}

fn halt_alert(host: &str) -> Alert {
    Alert {
        subject: subject(Kind::Halt, "kbf", host, "2.0"),
        message: format!("kbf is halted by {host}"),
    }
}

/// Proves: an alert is logged once, at the first observation that finds it, as one error line
/// with its `alert=` field and what it concerns; later observations keep when it was first
/// observed and log nothing; one that ended is forgotten, and coming back it is logged again
/// and first observed anew.
///
/// Catches: an alert logged at every observation, never, or without its token; a first
/// observation time that moves; an ended alert that keeps its old time.
#[test]
fn an_alert_is_logged_once_when_it_appears_and_keeps_its_first_time() {
    let book = Book::default();
    let log = Log::default();
    let _guard = log.install();
    assert_eq!(
        book.observe_with(|| Ok::<_, ()>((vec![halt_alert("d1")], UNIX)))
            .expect("observed"),
        vec![Observed {
            alert: halt_alert("d1"),
            first_unix: UNIX
        }]
    );
    let lines: Vec<String> = log.text().lines().map(str::to_owned).collect();
    let [line] = lines.as_slice() else {
        panic!("one line: {}", log.text());
    };
    assert!(
        line.ends_with(
            r#"ERROR hostkeeper_coordinator::alerts: kbf is halted by d1 alert="halt" service=kbf node_id=d1 version=2.0 request_id= other_host_id="#
        ),
        "{line}"
    );

    let both = book
        .observe_with(|| Ok::<_, ()>((vec![halt_alert("d1"), halt_alert("d2")], UNIX + 15)))
        .expect("observed");
    assert_eq!(
        both.iter()
            .map(|one| (one.alert.subject.host.as_str(), one.first_unix))
            .collect::<Vec<_>>(),
        vec![("d1", UNIX), ("d2", UNIX + 15)]
    );
    assert_eq!(log.text().lines().count(), 2, "{}", log.text());

    assert_eq!(
        book.observe_with(|| Ok::<_, ()>((vec![halt_alert("d2")], UNIX + 30)))
            .expect("observed")
            .len(),
        1
    );
    let back = book
        .observe_with(|| Ok::<_, ()>((vec![halt_alert("d1"), halt_alert("d2")], UNIX + 45)))
        .expect("observed");
    assert_eq!(
        back.iter().map(|one| one.first_unix).collect::<Vec<_>>(),
        vec![UNIX + 45, UNIX + 15]
    );
    assert_eq!(
        log.text().matches("node_id=d1").count(),
        2,
        "{}",
        log.text()
    );
}
