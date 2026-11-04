//! Tests of the adapter: [`NetFetcher`] over an in-memory transport, and [`UreqTransport`].

use std::cell::RefCell;
use std::io::{Cursor, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::ports::Stop;
use crate::testing::FakeClock;

const URL: &str = "https://releases.example/pkg/1.tar";

/// What a scripted body does when it is read.
struct Scripted {
    status: u16,
    length: Option<u64>,
    body: Vec<u8>,
    error: Option<io::ErrorKind>,
    hook: Box<dyn Fn()>,
}

fn scripted(status: u16, length: Option<u64>, body: &[u8]) -> Scripted {
    Scripted {
        status,
        length,
        body: body.to_vec(),
        error: None,
        hook: Box::new(|| {}),
    }
}

impl Scripted {
    fn into_response(self, reads: Arc<AtomicUsize>) -> Response {
        Response {
            status: self.status,
            content_length: self.length,
            body: Box::new(ScriptedBody {
                data: Cursor::new(self.body),
                error: self.error,
                reads,
                hook: self.hook,
            }),
        }
    }
}

/// Gives its bytes, then the error if there is one, else the end; counts every read.
struct ScriptedBody {
    data: Cursor<Vec<u8>>,
    error: Option<io::ErrorKind>,
    reads: Arc<AtomicUsize>,
    hook: Box<dyn Fn()>,
}

impl Read for ScriptedBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        (self.hook)();
        let n = self.data.read(buf).expect("a read of memory");
        self.error
            .filter(|_| n == 0)
            .map_or(Ok(n), |kind| Err(io::Error::from(kind)))
    }
}

/// Answers its one scripted response and records what it was opened with.
struct FakeTransport {
    response: RefCell<Option<Result<Response, NetError>>>,
    opened: Arc<Mutex<Vec<(String, Timeouts)>>>,
}

impl Transport for FakeTransport {
    fn open(&self, url: &str, timeouts: Timeouts) -> Result<Response, NetError> {
        self.opened
            .lock()
            .expect("the log")
            .push((url.to_owned(), timeouts));
        self.response.borrow_mut().take().expect("one request")
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Begin(Option<u64>),
    Chunk(Vec<u8>),
}

/// Records what the adapter gives it; can refuse at `begin` or at one chunk.
#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
    chunks: usize,
    stop_at_begin: bool,
    stop_at_chunk: Option<usize>,
}

impl Sink for Recorder {
    fn begin(&mut self, content_length: Option<u64>) -> Result<(), Stop> {
        self.events.push(Event::Begin(content_length));
        if self.stop_at_begin {
            Err(Stop)
        } else {
            Ok(())
        }
    }

    fn chunk(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        let index = self.chunks;
        self.chunks += 1;
        self.events.push(Event::Chunk(bytes.to_vec()));
        if self.stop_at_chunk == Some(index) {
            Err(Stop)
        } else {
            Ok(())
        }
    }
}

/// Everything one `get` did.
struct Run {
    result: Result<Got, NetError>,
    events: Vec<Event>,
    reads: usize,
    opened: Vec<(String, Timeouts)>,
}

/// One `get` on `clock` against `script`, with the deadline at `deadline` seconds.
fn drive(
    clock: &Arc<FakeClock>,
    script: Result<Scripted, NetError>,
    mut sink: Recorder,
    deadline: u64,
) -> Run {
    let reads = Arc::new(AtomicUsize::new(0));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let transport = FakeTransport {
        response: RefCell::new(Some(script.map(|s| s.into_response(Arc::clone(&reads))))),
        opened: Arc::clone(&opened),
    };
    let shared: Arc<dyn Clock> = clock.clone();
    let fetcher = NetFetcher::new(transport, shared);
    let limit = Limit {
        connect_timeout: Duration::from_secs(7),
        deadline: clock.stamp(deadline),
    };
    let result = fetcher.get(&Url::new(URL.to_owned()), &mut sink, limit);
    let opened = opened.lock().expect("the log").clone();
    Run {
        result,
        events: sink.events,
        reads: reads.load(Ordering::SeqCst),
        opened,
    }
}

/// `drive` at clock 10 with the deadline at 100.
fn standard(script: Result<Scripted, NetError>, sink: Recorder) -> Run {
    drive(&Arc::new(FakeClock::at(10)), script, sink, 100)
}

/// Every redirect status is an error, whatever the body names, and nothing is followed.
///
/// Catches a transport or adapter that follows a redirect to another host or scheme, or that
/// treats a 3xx as a body to deliver.
#[test]
fn net_adapter_refuses_a_cross_host_or_cross_scheme_redirect() {
    let bodies = [
        "Location: https://other.example/pkg/1.tar",
        "Location: http://releases.example/pkg/1.tar",
        "Location: https://releases.example/pkg/2.tar",
    ];
    for code in [301_u16, 302, 303, 307, 308] {
        for body in bodies {
            let run = standard(
                Ok(scripted(code, Some(body.len() as u64), body.as_bytes())),
                Recorder::default(),
            );
            assert_eq!(run.result, Err(NetError::Status(code)));
            assert_eq!(run.events, Vec::<Event>::new());
            assert_eq!(run.reads, 0);
            assert_eq!(run.opened.len(), 1);
            assert_eq!(run.opened[0].0, URL);
        }
    }
}

/// The announced length reaches the sink before any byte, and `None` is passed on as `None`.
///
/// Catches an adapter that calls `begin` after the first chunk, or drops the length.
#[test]
fn net_adapter_reports_content_length_before_the_first_byte() {
    let run = standard(Ok(scripted(200, Some(3), b"abc")), Recorder::default());
    assert_eq!(run.result, Ok(Got::Complete));
    assert_eq!(
        run.events,
        vec![Event::Begin(Some(3)), Event::Chunk(b"abc".to_vec())]
    );

    let run = standard(Ok(scripted(200, None, b"abc")), Recorder::default());
    assert_eq!(run.result, Ok(Got::Complete));
    assert_eq!(
        run.events,
        vec![Event::Begin(None), Event::Chunk(b"abc".to_vec())]
    );
}

/// Statuses, short bodies, read errors and transport errors each map to one `NetError`.
///
/// Catches a 2xx range that is too wide or too narrow, a short body taken as complete, a read
/// error of one kind reported as another, and a transport error that is rewritten.
#[test]
fn net_adapter_maps_status_and_short_reads_to_net_errors() {
    for (status, expected) in [
        (404_u16, Err(NetError::Status(404))),
        (500, Err(NetError::Status(500))),
        (199, Err(NetError::Status(199))),
        (200, Ok(Got::Complete)),
        (299, Ok(Got::Complete)),
    ] {
        let run = standard(Ok(scripted(status, Some(3), b"abc")), Recorder::default());
        assert_eq!(run.result, expected, "status {status}");
    }

    let run = standard(Ok(scripted(200, Some(10), b"abcd")), Recorder::default());
    assert_eq!(run.result, Err(NetError::Truncated));
    assert_eq!(
        run.events,
        vec![Event::Begin(Some(10)), Event::Chunk(b"abcd".to_vec())]
    );

    for (kind, expected) in [
        (io::ErrorKind::UnexpectedEof, NetError::Truncated),
        (io::ErrorKind::TimedOut, NetError::DeadlineExceeded),
        (io::ErrorKind::ConnectionReset, NetError::Io),
        (io::ErrorKind::Other, NetError::Io),
    ] {
        let mut script = scripted(200, Some(10), b"abcd");
        script.error = Some(kind);
        let run = standard(Ok(script), Recorder::default());
        assert_eq!(run.result, Err(expected), "{kind:?}");
    }

    let run = standard(Err(NetError::Connect), Recorder::default());
    assert_eq!(run.result, Err(NetError::Connect));
    assert_eq!(run.events, Vec::<Event>::new());
}

/// A deadline that has passed, or that was set on another boot, ends the call before any request.
///
/// Catches a request sent with a zero total timeout, and a deadline compared across a reboot.
#[test]
fn net_adapter_gives_up_before_the_request_when_the_deadline_has_passed() {
    let clock = Arc::new(FakeClock::at(100));
    let run = drive(
        &clock,
        Ok(scripted(200, None, b"abc")),
        Recorder::default(),
        100,
    );
    assert_eq!(run.result, Err(NetError::DeadlineExceeded));
    assert_eq!(run.opened.len(), 0);
    assert_eq!(run.events, Vec::<Event>::new());

    let clock = Arc::new(FakeClock::at(10));
    let deadline = clock.stamp(1000);
    clock.reboot("boot-2");
    let reads = Arc::new(AtomicUsize::new(0));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let transport = FakeTransport {
        response: RefCell::new(Some(Ok(scripted(200, None, b"abc").into_response(reads)))),
        opened: Arc::clone(&opened),
    };
    let shared: Arc<dyn Clock> = clock.clone();
    let limit = Limit {
        connect_timeout: Duration::from_secs(7),
        deadline,
    };
    let mut sink = Recorder::default();
    let result =
        NetFetcher::new(transport, shared).get(&Url::new(URL.to_owned()), &mut sink, limit);
    assert_eq!(result, Err(NetError::DeadlineExceeded));
    assert_eq!(opened.lock().expect("the log").len(), 0);
}

/// The transport is opened with the connect timeout of the limit and the seconds left as total.
///
/// Catches a total timeout that is the whole window instead of what is left, and a connect
/// timeout that is replaced.
#[test]
fn net_adapter_opens_the_transport_with_the_limit_and_the_time_left() {
    let run = standard(Ok(scripted(200, Some(0), b"")), Recorder::default());
    assert_eq!(run.result, Ok(Got::Complete));
    assert_eq!(
        run.opened,
        vec![(
            URL.to_owned(),
            Timeouts {
                connect: Duration::from_secs(7),
                total: Duration::from_secs(90),
            }
        )]
    );
}

/// A deadline that passes while the body is being read ends the transfer at the next read.
///
/// Catches a deadline checked only before the request, which lets a stalled body run on.
#[test]
fn net_adapter_stops_when_the_deadline_passes_between_reads() {
    let clock = Arc::new(FakeClock::at(10));
    let ticking = Arc::clone(&clock);
    let mut script = scripted(200, None, b"abc");
    script.hook = Box::new(move || ticking.advance(100));
    let run = drive(&clock, Ok(script), Recorder::default(), 100);
    assert_eq!(run.result, Err(NetError::DeadlineExceeded));
    assert_eq!(
        run.events,
        vec![Event::Begin(None), Event::Chunk(b"abc".to_vec())]
    );
    assert_eq!(run.reads, 1);
}

/// A sink that stops at `begin` reads nothing; one that stops at a chunk gets no further read.
///
/// Catches an adapter that goes on reading after the sink said stop, or reports it as complete.
#[test]
fn net_adapter_stops_reading_when_the_sink_stops() {
    let sink = Recorder {
        stop_at_begin: true,
        ..Recorder::default()
    };
    let run = standard(Ok(scripted(200, Some(3), b"abc")), sink);
    assert_eq!(run.result, Ok(Got::Stopped));
    assert_eq!(run.events, vec![Event::Begin(Some(3))]);
    assert_eq!(run.reads, 0);

    let sink = Recorder {
        stop_at_chunk: Some(0),
        ..Recorder::default()
    };
    let body = vec![1_u8; CHUNK_BYTES + 10];
    let run = standard(Ok(scripted(200, None, &body)), sink);
    assert_eq!(run.result, Ok(Got::Stopped));
    assert_eq!(
        run.events,
        vec![Event::Begin(None), Event::Chunk(vec![1_u8; CHUNK_BYTES])]
    );
    assert_eq!(run.reads, 1);
}

/// Bytes past the announced length are neither read nor delivered.
///
/// Catches a body read to its end whatever the server announced.
#[test]
fn net_adapter_does_not_read_past_the_announced_length() {
    let run = standard(
        Ok(scripted(200, Some(4), b"abcdefghij")),
        Recorder::default(),
    );
    assert_eq!(run.result, Ok(Got::Complete));
    assert_eq!(
        run.events,
        vec![Event::Begin(Some(4)), Event::Chunk(b"abcd".to_vec())]
    );
    assert_eq!(run.reads, 1);
}

/// A body larger than one chunk arrives in chunks of at most `CHUNK_BYTES`, in order.
///
/// Catches a buffer that grows with the body, and chunks delivered out of order or cut short.
#[test]
fn net_adapter_delivers_a_large_body_in_bounded_chunks() {
    let body: Vec<u8> = (0..(2 * CHUNK_BYTES + 1))
        .map(|i| (i % 251) as u8)
        .collect();
    let run = standard(
        Ok(scripted(200, Some(body.len() as u64), &body)),
        Recorder::default(),
    );
    assert_eq!(run.result, Ok(Got::Complete));
    let lengths: Vec<usize> = run
        .events
        .iter()
        .map(|event| match event {
            Event::Chunk(bytes) => bytes.len(),
            Event::Begin(_) => 0,
        })
        .collect();
    assert_eq!(lengths, vec![0, CHUNK_BYTES, CHUNK_BYTES, 1]);
    let joined: Vec<u8> = run
        .events
        .into_iter()
        .flat_map(|event| match event {
            Event::Chunk(bytes) => bytes,
            Event::Begin(_) => Vec::new(),
        })
        .collect();
    assert_eq!(joined, body);
}

/// A body with no announced length is delivered whole and is complete at its end.
///
/// Catches an adapter that needs a length to call a transfer complete.
#[test]
fn net_adapter_completes_a_body_with_no_length() {
    let run = standard(Ok(scripted(200, None, b"hello")), Recorder::default());
    assert_eq!(run.result, Ok(Got::Complete));
    assert_eq!(
        run.events,
        vec![Event::Begin(None), Event::Chunk(b"hello".to_vec())]
    );
}

/// The parts of a `ureq` response are read as they are: status, announced length, body bytes.
///
/// Catches a status or length taken from the wrong place, and a body that is not passed on.
#[test]
fn ureq_response_is_read_into_status_length_and_body() {
    for status in [200_u16, 404] {
        let response = ureq::http::Response::builder()
            .status(status)
            .body(ureq::Body::builder().data(b"hello".to_vec()))
            .expect("a response");
        let mut parts = response_of(response);
        assert_eq!(parts.status, status);
        assert_eq!(parts.content_length, Some(5));
        let mut bytes = Vec::new();
        parts.body.read_to_end(&mut bytes).expect("the body");
        assert_eq!(bytes, b"hello".to_vec());
    }
}

/// Timeouts of the body or the whole request are the deadline; resolving and connecting ones, and
/// every other failure, are no connection.
///
/// Catches a slow body reported as a refused connection, and a refused connection reported as
/// a missed deadline.
#[test]
fn ureq_errors_map_to_net_errors() {
    let cases = [
        (
            ureq::Error::Timeout(ureq::Timeout::Global),
            NetError::DeadlineExceeded,
        ),
        (
            ureq::Error::Timeout(ureq::Timeout::RecvBody),
            NetError::DeadlineExceeded,
        ),
        (
            ureq::Error::Timeout(ureq::Timeout::Connect),
            NetError::Connect,
        ),
        (
            ureq::Error::Timeout(ureq::Timeout::Resolve),
            NetError::Connect,
        ),
        (ureq::Error::HostNotFound, NetError::Connect),
        (ureq::Error::ConnectionFailed, NetError::Connect),
        (ureq::Error::StatusCode(500), NetError::Connect),
    ];
    for (error, expected) in cases {
        let text = error.to_string();
        assert_eq!(call_error(error), expected, "{text}");
    }
}

/// A loopback listener on a free port whose one connection gets bytes that are not TLS.
fn not_tls_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("one connection");
        drop(stream.write_all(b"not tls at all"));
    });
    (port, handle)
}

fn generous() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(30),
        total: Duration::from_secs(60),
    }
}

/// A server that does not speak TLS is no connection, with no roots and with a root that is not
/// a certificate.
///
/// Catches a handshake failure that is reported as a status or an I/O error of the body.
#[test]
fn ureq_transport_reports_a_server_that_is_not_tls_as_no_connection() {
    for roots in [Vec::new(), vec![vec![1_u8, 2, 3]]] {
        let (port, handle) = not_tls_server();
        let transport = UreqTransport::new(&roots);
        let result = transport.open(&format!("https://127.0.0.1:{port}/x"), generous());
        assert_eq!(result.err(), Some(NetError::Connect));
        handle.join().expect("the server thread");
    }
}

/// A plain `http` URL is refused: the client is HTTPS only.
///
/// Catches a client that would fetch a package over an unencrypted connection.
#[test]
fn ureq_transport_refuses_a_plain_http_url() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let transport = UreqTransport::new(&[]);
    let result = transport.open(&format!("http://127.0.0.1:{port}/x"), generous());
    assert_eq!(result.err(), Some(NetError::Connect));
}
