//! [`Live::stop`] on its own, with no stream: the first cause sent is the one the stream closes
//! with, and a later one neither replaces it nor wakes the stream again.

use std::sync::Arc;

use hostkeeper_console::bridge::Moved;
use hostkeeper_console::close::Close;
use hostkeeper_console::session::{SESSION_ID_BYTES, SessionId};

use super::{By, Live, Stop};
use crate::ssh_tunnel::Credential;

/// A stream's entry in the sessions table, and the receiver its stream would read.
fn live() -> (Live, tokio::sync::watch::Receiver<Option<Stop>>) {
    let (stop, stopped) = tokio::sync::watch::channel(None);
    let live = Live {
        id: SessionId::from_bytes([7; SESSION_ID_BYTES]),
        stop,
        moved: Arc::new(Moved::default()),
        credential: Credential::Token,
        used_at: 0,
    };
    (live, stopped)
}

/// Proves: the first stop is sent, with its close and `by`; a second, by another and with
/// another close, leaves the first in place and does not mark the receiver changed.
///
/// Catches: a sweep's or a drain's stop overwriting the operator's close of the same stream, so
/// the close line names the wrong cause or the wrong person.
#[test]
fn the_first_stop_is_kept_and_a_later_one_is_ignored() {
    let (live, mut stopped) = live();
    let first = Stop {
        close: Close::ClosedByOperator,
        by: By::named("alice", None),
    };
    live.stop(Close::ClosedByOperator, "alice");
    assert!(stopped.has_changed().expect("the sender lives"));
    assert_eq!(*stopped.borrow_and_update(), Some(first.clone()));
    live.stop(Close::CoordinatorStopping, "coordinator");
    assert!(!stopped.has_changed().expect("the sender lives"));
    assert_eq!(*stopped.borrow(), Some(first));
}
