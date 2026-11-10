//! The tests of `coordinator_store`: the package layer's fetcher over a fake coordinator.

use std::sync::Mutex;

use hostkeeper_link::LinkError;
use hostkeeper_package::ports::Stop;
use hostkeeper_package::stores::{COORDINATOR_STORE, Stores};
use hostkeeper_steps::Stamp;

use super::*;
use crate::testing::FakeClock;

const LOCATOR: &str = "repositories/acme/pool/group/config.pkg";
const MAX: u64 = 1 << 20;

/// A coordinator that answers every path with one scripted result and records what it was asked.
struct Scripted {
    answer: Result<Option<Vec<u8>>, GetError>,
    asked: Mutex<Vec<(String, Bound)>>,
}

impl Scripted {
    fn new(answer: Result<Option<Vec<u8>>, GetError>) -> Arc<Scripted> {
        Arc::new(Scripted {
            answer,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<(String, Bound)> {
        self.asked.lock().expect("asked").clone()
    }
}

impl Fetch for Scripted {
    fn get(
        &self,
        path: &str,
        limit: Bound,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Answer, GetError> {
        self.asked
            .lock()
            .expect("asked")
            .push((path.to_owned(), limit));
        match &self.answer {
            Ok(None) => Ok(Answer::NotFound),
            Ok(Some(body)) => {
                for chunk in body.chunks(4) {
                    sink(chunk).map_err(|e| GetError::Sink(e.kind()))?;
                }
                Ok(Answer::Body(body.len() as u64))
            }
            Err(error) => Err(error.clone()),
        }
    }
}

/// A sink that keeps what it is given and may stop at its `begin` or at the n-th chunk.
#[derive(Default)]
struct Keep {
    begun: Vec<Option<u64>>,
    body: Vec<u8>,
    stop_begin: bool,
    stop_chunk: bool,
}

impl Sink for Keep {
    fn begin(&mut self, content_length: Option<u64>) -> Result<(), Stop> {
        self.begun.push(content_length);
        if self.stop_begin { Err(Stop) } else { Ok(()) }
    }

    fn chunk(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        if self.stop_chunk {
            return Err(Stop);
        }
        self.body.extend_from_slice(bytes);
        Ok(())
    }
}

/// Asks the fetcher over `fetch` for the locator's path, 30 seconds after the clock's now.
fn ask(fetch: &Arc<Scripted>, sink: &mut Keep) -> Result<Got, NetError> {
    let clock = FakeClock::new();
    let fetcher = CoordinatorFetcher::new(fetch.clone(), clock.clone(), MAX);
    let stores = Stores::default();
    let url = stores
        .get(COORDINATOR_STORE)
        .expect("the coordinator store")
        .blob_url(LOCATOR, &"0".repeat(64));
    let mut deadline = Stamp::now(&*clock);
    deadline.mono_secs += 30;
    let limit = Limit {
        connect_timeout: std::time::Duration::from_secs(1),
        deadline,
    };
    fetcher.get(&url, sink, limit)
}

/// The body reaches the sink whole, after one `begin` with no length, and the coordinator is
/// asked for `/` and the locator within the bytes and the seconds the transfer has left.
///
/// Catches a path other than the locator's, a body cut or reordered, a missing `begin`, and a
/// bound that is not the entry's size or the deadline's remainder.
#[test]
fn a_body_reaches_the_sink_from_the_path_with_the_transfers_bounds() {
    let fetch = Scripted::new(Ok(Some(b"the bytes of a package".to_vec())));
    let mut sink = Keep::default();
    assert_eq!(ask(&fetch, &mut sink), Ok(Got::Complete));
    assert_eq!(sink.body, b"the bytes of a package");
    assert_eq!(sink.begun, [None]);
    assert_eq!(
        fetch.asked(),
        [(
            format!("/{LOCATOR}"),
            Bound {
                max_bytes: MAX,
                deadline_secs: 30
            }
        )]
    );
}

/// An empty body is complete and the sink is still told the transfer began, once.
///
/// Catches an empty body that never reaches `begin`.
#[test]
fn an_empty_body_begins_the_sink_once() {
    let fetch = Scripted::new(Ok(Some(Vec::new())));
    let mut sink = Keep::default();
    assert_eq!(ask(&fetch, &mut sink), Ok(Got::Complete));
    assert_eq!(sink.begun, [None]);
    assert!(sink.body.is_empty());
    let mut sink = Keep {
        stop_begin: true,
        ..Keep::default()
    };
    assert_eq!(ask(&fetch, &mut sink), Ok(Got::Stopped));
}

/// A sink that stops, at `begin` or at a chunk, ends the transfer as stopped, not as a failure.
///
/// Catches a stop reported as a network error, which would be retried at the next source.
#[test]
fn a_sink_that_stops_ends_the_transfer_as_stopped() {
    let fetch = Scripted::new(Ok(Some(b"the bytes of a package".to_vec())));
    for mut sink in [
        Keep {
            stop_begin: true,
            ..Keep::default()
        },
        Keep {
            stop_chunk: true,
            ..Keep::default()
        },
    ] {
        assert_eq!(ask(&fetch, &mut sink), Ok(Got::Stopped));
        assert!(sink.body.is_empty());
    }
}

/// Each way the link gives no body has its package-layer error: the statuses by their code
/// (a 404 included), a short body, the deadline, a sink's own failure; every failure to reach the
/// coordinator, a host with no certificate and a body over the bound as a failed connection.
///
/// Catches a 404 taken for a body, and a failure of the link mapped to another kind.
#[test]
fn what_the_link_cannot_deliver_is_a_net_error() {
    let cases = [
        (Ok(None), NetError::Status(404)),
        (Err(GetError::Status(502)), NetError::Status(502)),
        (Err(GetError::Truncated), NetError::Truncated),
        (Err(GetError::Deadline), NetError::DeadlineExceeded),
        (
            Err(GetError::Sink(io::ErrorKind::StorageFull)),
            NetError::Io,
        ),
        (Err(GetError::NotEnrolled), NetError::Connect),
        (Err(GetError::Path("x".to_owned())), NetError::Connect),
        (
            Err(GetError::Link(LinkError::Transport(
                io::ErrorKind::ConnectionRefused,
            ))),
            NetError::Connect,
        ),
        (
            Err(GetError::TooLong { length: 9, max: 8 }),
            NetError::Connect,
        ),
    ];
    for (answer, want) in cases {
        let fetch = Scripted::new(answer);
        assert_eq!(ask(&fetch, &mut Keep::default()), Err(want));
    }
}
