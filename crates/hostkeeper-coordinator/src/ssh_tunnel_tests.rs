//! The tunnels of an instance: the relay between a client and a host's sshd (a listener on a port
//! of loopback the test owns), the registry, the poll and the drain. The book is the memory one
//! over the memory ledger of the signing tests. No test asserts a time: the timers run on tokio's
//! paused clock, and a test waits for the events the tunnels tell.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::SshRefusal as Refusal;
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tonic::Status;

use super::{Event, Incoming, Limits, MAX_FRAME, Outgoing, QUEUE, Tunnels, instance_id};
use crate::enrollment::Random;
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::login::Login;
use crate::login_support::{Seconds, counting, settings};
use crate::operator::Caller;
use crate::service::Coordinator;
use crate::ssh_book::{
    self, Book, BookError, Cause, Closing, OpenRefused, Opened, Opening, PinRefused, Polled,
    TUNNEL_CLOSE, TUNNEL_OPEN, close_detail, open_detail,
};
use crate::ssh_ledger::{self, Issue, Ledger};
use crate::ssh_sign::tests::{OPERATORS, Tune, world};
use crate::testutil::Rig as ScratchRig;

/// The id of the instance of the tests.
pub(crate) const INSTANCE: &str = "00112233aabbccdd";

/// The hosts the book has heard from, all at loopback (`nuc` is heard by [`Rig::hear`]).
const HOSTS: [&str; 2] = ["node-07", "node-08"];

/// The book of the tests: the memory book, which a test can make fail, and which tells a test
/// each close it was asked for.
pub(crate) struct TestBook {
    pub(crate) inner: Arc<ssh_book::Memory>,
    failing: Mutex<BTreeSet<&'static str>>,
    closes: Mutex<Option<mpsc::UnboundedSender<Closing>>>,
    polls: Mutex<Vec<(String, Vec<u64>, u64)>>,
    /// While a test holds the sender, `open` and `close` wait here (see [`Self::hold`]).
    gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl TestBook {
    fn new(inner: Arc<ssh_book::Memory>) -> Self {
        Self {
            inner,
            failing: Mutex::new(BTreeSet::new()),
            closes: Mutex::new(None),
            polls: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        }
    }

    /// Makes the calls of `open` and `close` that come next wait until the sender given back is
    /// dropped: a book that is slow, for as long as the test says.
    pub(crate) fn hold(&self) -> std::sync::mpsc::Sender<()> {
        let (hold, gate) = std::sync::mpsc::channel();
        *self.gate.lock().unwrap_or_else(PoisonError::into_inner) = Some(gate);
        hold
    }

    /// Waits at the gate, if a test holds it.
    fn wait_at_gate(&self) {
        let gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(gate) = gate.as_ref() {
            // It returns when the sender is dropped: nothing is ever sent.
            let _ = gate.recv();
        }
    }

    /// Makes the calls named `op` fail until [`Self::mend`]; `panic_<op>` (`open`, `close`,
    /// `poll`, `heard`, `host_keys`, `pin`) makes them panic instead.
    pub(crate) fn fail(&self, op: &'static str) {
        self.failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(op);
    }

    /// Makes the calls named `op` work again.
    pub(crate) fn mend(&self, op: &'static str) {
        self.failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(op);
    }

    fn down(&self, op: &'static str) -> bool {
        self.failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(op)
    }

    /// The closes the book is asked for from now on.
    pub(crate) fn watch_closes(&self) -> mpsc::UnboundedReceiver<Closing> {
        let (told, closes) = mpsc::unbounded_channel();
        *self.closes.lock().unwrap_or_else(PoisonError::into_inner) = Some(told);
        closes
    }

    /// The polls the book was asked for: the instance, the ids and the stale seconds.
    pub(crate) fn polls(&self) -> Vec<(String, Vec<u64>, u64)> {
        self.polls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Book for TestBook {
    fn heard(&self, host_id: &str, address: &str, reported: &[String]) -> Result<(), BookError> {
        assert!(!self.down("panic_heard"), "the book panicked");
        if self.down("heard") {
            return Err(BookError("the book is down".to_owned()));
        }
        self.inner.heard(host_id, address, reported)
    }

    fn host_keys(&self, host_id: &str) -> Result<ssh_book::HostKeys, BookError> {
        assert!(!self.down("panic_host_keys"), "the book panicked");
        if self.down("host_keys") {
            return Err(BookError("the book is down".to_owned()));
        }
        self.inner.host_keys(host_id)
    }

    fn pin(&self, by: &str, host_id: &str, fingerprints: &[String]) -> Result<(), PinRefused> {
        assert!(!self.down("panic_pin"), "the book panicked");
        if self.down("pin") {
            return Err(PinRefused::Unwritten("the book is down".to_owned()));
        }
        self.inner.pin(by, host_id, fingerprints)
    }

    fn open(&self, opening: &Opening) -> Result<Opened, OpenRefused> {
        self.wait_at_gate();
        assert!(!self.down("panic_open"), "the book panicked");
        if self.down("open") {
            return Err(OpenRefused::Unwritten("the book is down".to_owned()));
        }
        self.inner.open(opening)
    }

    fn close(&self, closing: &Closing) -> Result<(), BookError> {
        self.wait_at_gate();
        if self.down("panic_close") {
            if let Some(told) = self
                .closes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                let _ = told.send(closing.clone());
            }
            panic!("the book panicked");
        }
        let done = if self.down("close") {
            Err(BookError("the book is down".to_owned()))
        } else {
            self.inner.close(closing)
        };
        if let Some(told) = self
            .closes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            let _ = told.send(closing.clone());
        }
        done
    }

    fn poll(&self, instance: &str, ids: &[u64], stale_secs: u64) -> Result<Polled, BookError> {
        assert!(!self.down("panic_poll"), "the book panicked");
        self.polls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((instance.to_owned(), ids.to_vec(), stale_secs));
        if self.down("poll") {
            return Err(BookError("the book is down".to_owned()));
        }
        self.inner.poll(instance, ids, stale_secs)
    }
}

/// A coordinator that signs and holds tunnels, over the memory book, with a listener standing for
/// the sshd of every host.
pub(crate) struct Rig {
    pub(crate) coordinator: Arc<Coordinator>,
    pub(crate) tunnels: Arc<Tunnels>,
    pub(crate) book: Arc<TestBook>,
    pub(crate) ledger: Arc<ssh_ledger::Memory>,
    pub(crate) operators: PathBuf,
    pub(crate) records: Arc<MemoryRecords>,
    /// The clock of the coordinator and its sign-in, in seconds.
    pub(crate) clock: Arc<AtomicU64>,
    pub(crate) events: mpsc::UnboundedReceiver<Event>,
    /// Events read while waiting for another one.
    unmatched: Vec<Event>,
    pub(crate) listener: TcpListener,
    _enrollment: EnrollmentRig,
    _rig: ScratchRig,
}

/// The limits of the tests: the flags' defaults, the sshd's port the listener's.
pub(crate) fn limits(port: u16) -> Limits {
    Limits {
        live_secs: 120,
        idle: Duration::from_secs(900),
        max_session: Duration::from_secs(28_800),
        dial_timeout: Duration::from_secs(5),
        max_tunnels: 16,
        per_operator: 4,
        poll: Duration::from_secs(10),
        drain: Duration::from_secs(600),
        port,
    }
}

impl Rig {
    /// A rig named `name` whose limits `tune` changes.
    pub(crate) async fn new(name: &str, tune: impl FnOnce(&mut Limits)) -> Rig {
        Rig::tuned(name, Tune::default(), tune).await
    }

    /// [`Rig::new`] with the signing changed as `signing` says.
    pub(crate) async fn tuned(name: &str, signing: Tune, tune: impl FnOnce(&mut Limits)) -> Rig {
        let world = world(name, Some(OPERATORS), signing);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a listener on a port of loopback");
        let mut limits = limits(listener.local_addr().expect("an address").port());
        tune(&mut limits);
        let book = Arc::new(TestBook::new(Arc::new(ssh_book::Memory::over(Arc::clone(
            &world.ledger,
        )))));
        let tunnels = Tunnels::new(
            Arc::clone(&book) as Arc<dyn Book>,
            INSTANCE.to_owned(),
            limits,
        );
        let events = tunnels.watch();
        let clock = Arc::clone(&world._rig.clock);
        let login = Login::new(
            settings(),
            Arc::new(Seconds(Arc::clone(&clock))),
            counting(),
        )
        .expect("a sign-in");
        let coordinator = Arc::new(world.coordinator.with_tunnels(tunnels).with_login(login));
        let tunnels = Arc::clone(coordinator.tunnels.as_ref().expect("the tunnels"));
        for host in HOSTS {
            book.inner
                .heard(host, "127.0.0.1", &[])
                .expect("the host is heard");
        }
        Rig {
            coordinator,
            tunnels,
            book,
            ledger: world.ledger,
            operators: world.operators,
            records: world.records,
            clock,
            events,
            unmatched: Vec::new(),
            listener,
            _enrollment: world._enrollment,
            _rig: world._rig,
        }
    }

    /// The book hears `host` from loopback.
    pub(crate) fn hear(&self, host: &str) {
        self.book
            .inner
            .heard(host, "127.0.0.1", &[])
            .expect("the host is heard");
    }

    /// A certificate of `operator` for `host`, which the ledger holds: its serial.
    pub(crate) fn cert(&self, operator: &str, host: &str) -> u64 {
        self.ledger
            .issue(&Issue {
                operator,
                host_id: host,
                fingerprint: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                ttl_secs: 3600,
                reason: "",
            })
            .expect("a certificate")
            .serial
    }

    /// The next event `wanted` says yes to.
    pub(crate) async fn event(&mut self, wanted: impl Fn(&Event) -> bool) -> Event {
        if let Some(at) = self.unmatched.iter().position(&wanted) {
            return self.unmatched.remove(at);
        }
        loop {
            let event = self.events.recv().await.expect("the tunnels tell events");
            if wanted(&event) {
                return event;
            }
            self.unmatched.push(event);
        }
    }

    /// Waits until tunnel `id` has ended, and gives how.
    pub(crate) async fn closed(&mut self, id: u64) -> Cause {
        match self
            .event(|event| matches!(event, Event::Closed(closed, _) if *closed == id))
            .await
        {
            Event::Closed(_, cause) => cause,
            other => unreachable!("{other:?}"),
        }
    }

    /// The call that opens a tunnel for `caller`: its answer, and the way to send frames.
    pub(crate) async fn try_open(
        &self,
        caller: &Caller,
        first: proto::TunnelFrame,
    ) -> (Result<Outgoing, Refusal>, Client) {
        let (client, incoming) = Client::new();
        client.send(first).await;
        let answer = self
            .coordinator
            .ssh_tunnel_open(caller, incoming, None)
            .await;
        (answer, client)
    }

    /// A tunnel of `operator` to `host` that is open: the certificate made, the connection
    /// accepted, and the relay running.
    pub(crate) async fn tunnel(&mut self, operator: &str, host: &str) -> Live {
        let serial = self.cert(operator, host);
        let caller = caller(operator);
        let (answer, client) = self.try_open(&caller, open(host, serial)).await;
        let mut out = answer.expect("the tunnel is opened");
        let (socket, _) = self.listener.accept().await.expect("the host is dialled");
        let Event::Opened(id) = self.event(|e| matches!(e, Event::Opened(_))).await else {
            unreachable!("an event that was asked for");
        };
        self.event(|e| *e == Event::Relaying(id)).await;
        assert_eq!(
            frame(&mut out).await,
            Frame::Opened(proto::Opened {
                login_user: "root".to_owned()
            })
        );
        Live {
            id,
            serial,
            client,
            out,
            host: socket,
        }
    }

    /// The audit lines of the tunnels: who, which, about whom, and the detail.
    pub(crate) fn tunnel_lines(&self) -> Vec<(String, &'static str, Option<String>, String)> {
        self.ledger
            .audit()
            .into_iter()
            .filter(|row| [TUNNEL_OPEN, TUNNEL_CLOSE].contains(&row.line.action))
            .map(|row| {
                (
                    row.line.by,
                    row.line.action,
                    row.line.subject,
                    row.line.detail,
                )
            })
            .collect()
    }

    /// One turn of the poll.
    pub(crate) async fn poll(&self) {
        self.tunnels.poll_once(&self.coordinator).await;
    }
}

/// The sending side of a client's stream.
pub(crate) struct Client(mpsc::Sender<Result<proto::TunnelFrame, Status>>);

impl Client {
    pub(crate) fn new() -> (Client, Incoming) {
        let (sender, received) = mpsc::channel(8);
        let incoming = futures::stream::unfold(received, |mut received| async move {
            received.recv().await.map(|item| (item, received))
        })
        .boxed();
        (Client(sender), incoming)
    }

    pub(crate) async fn send(&self, frame: proto::TunnelFrame) {
        self.0.send(Ok(frame)).await.expect("the stream is open");
    }

    pub(crate) async fn fail(&self) {
        self.0
            .send(Err(Status::cancelled("the client went away")))
            .await
            .expect("the stream is open");
    }
}

/// A tunnel that is open, as the test sees it.
pub(crate) struct Live {
    pub(crate) id: u64,
    pub(crate) serial: u64,
    pub(crate) client: Client,
    pub(crate) out: Outgoing,
    pub(crate) host: TcpStream,
}

pub(crate) fn caller(name: &str) -> Caller {
    Caller::Operator(name.parse().expect("a token name"))
}

pub(crate) fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

pub(crate) fn open(host: &str, serial: u64) -> proto::TunnelFrame {
    proto::TunnelFrame {
        frame: Some(Frame::Open(proto::Open {
            host_id: host.to_owned(),
            cert_serial: serial,
        })),
    }
}

pub(crate) fn data(bytes: &[u8]) -> proto::TunnelFrame {
    proto::TunnelFrame {
        frame: Some(Frame::Data(bytes.to_vec())),
    }
}

pub(crate) fn close(cause: &str) -> proto::TunnelFrame {
    proto::TunnelFrame {
        frame: Some(Frame::Close(proto::Close {
            cause: cause.to_owned(),
        })),
    }
}

/// The next frame the client is sent.
pub(crate) async fn frame(out: &mut Outgoing) -> Frame {
    out.next()
        .await
        .expect("a frame")
        .expect("not an error")
        .frame
        .expect("a frame with a case")
}

/// The bytes the client is sent until the stream's `close`, and its cause; the stream ends then.
pub(crate) async fn until_close(out: &mut Outgoing) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    loop {
        match frame(out).await {
            Frame::Data(more) => bytes.extend(more),
            Frame::Close(proto::Close { cause }) => {
                assert!(out.next().await.is_none(), "nothing follows the close");
                return (bytes, cause);
            }
            other => panic!("a frame that is none of the relay's: {other:?}"),
        }
    }
}

/// What `read_exact` reads from `socket`: `n` bytes.
async fn read(socket: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut bytes = vec![0; n];
    socket.read_exact(&mut bytes).await.expect("the bytes");
    bytes
}

/// Proves: the id of an instance is 16 lowercase hex characters made of 8 bytes of the
/// coordinator's random source, and a random source that cannot give them gives no id.
///
/// Catches: an id that is shorter, in upper case or not made of the source's bytes, and a failed
/// source that still gives an id.
#[test]
fn an_instance_id_is_sixteen_lowercase_hex_characters_of_the_random_bytes() {
    let counting: Random = Box::new(|buffer| {
        for (n, byte) in buffer.iter_mut().enumerate() {
            *byte = 0xa0 + u8::try_from(n).expect("a small index");
        }
        true
    });
    assert_eq!(instance_id(&counting).as_deref(), Some("a0a1a2a3a4a5a6a7"));
    let broken: Random = Box::new(|_| false);
    assert_eq!(instance_id(&broken), None);
}

/// Proves: bytes go both ways through an open tunnel in the order they were sent; a `close` from
/// the client ends only its input (the host reads the end of its stream) while the host's answer
/// still reaches the client; the host's end of the connection then closes the tunnel
/// `host_closed`; and the book holds the open line, and the close line with the bytes each way.
///
/// Catches: a relay that ends at the client's `close` and drops the host's last answer (the end of
/// an `scp`), bytes that are lost or reordered, and a close line without the counts or the cause.
#[tokio::test]
async fn bytes_go_both_ways_and_a_client_close_ends_its_input_only() {
    let mut rig = Rig::new("tunnel-bytes", |_| {}).await;
    let mut live = rig.tunnel("operator", "node-07").await;
    live.client.send(data(b"hello")).await;
    assert_eq!(read(&mut live.host, 5).await, b"hello");
    live.host.write_all(b"world").await.expect("written");
    assert_eq!(frame(&mut live.out).await, Frame::Data(b"world".to_vec()));
    live.client.send(close("eof")).await;
    let mut rest = Vec::new();
    live.host
        .read_to_end(&mut rest)
        .await
        .expect("the host reads the end of the client's input");
    assert!(rest.is_empty());
    live.host.write_all(b"bye").await.expect("written");
    drop(live.host);
    let (bytes, cause) = until_close(&mut live.out).await;
    assert_eq!(
        (bytes.as_slice(), cause.as_str()),
        (&b"bye"[..], "host_closed")
    );
    assert_eq!(rig.tunnels.count(), 0);
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
    assert_eq!(
        rig.tunnel_lines(),
        vec![
            (
                "operator".to_owned(),
                TUNNEL_OPEN,
                Some("node-07".to_owned()),
                open_detail(live.id, live.serial, "127.0.0.1", INSTANCE)
            ),
            (
                "operator".to_owned(),
                TUNNEL_CLOSE,
                Some("node-07".to_owned()),
                close_detail(live.id, Cause::HostClosed, Some((5, 8)), 0)
            ),
        ]
    );
}

/// Proves: a client whose stream ends without a `close` ends the tunnel `eof`; a stream that
/// fails, and a frame the client may not send (a second `open`, an `opened`, a frame with no
/// case, `data` over 16 KiB) end it `error`; each is told to the client in `close` and written to
/// the book.
///
/// Catches: a tunnel that outlives its client, a frame that is relayed to the host or ignored
/// instead of ending the stream, and a cause the audit does not hold.
#[tokio::test]
async fn a_client_that_ends_or_breaks_the_protocol_ends_its_tunnel() {
    let mut rig = Rig::new("tunnel-client-ends", |_| {}).await;
    let causes = [
        ("eof", None),
        ("error", Some(proto::TunnelFrame { frame: None })),
        ("error", Some(open("node-07", 1))),
        (
            "error",
            Some(proto::TunnelFrame {
                frame: Some(Frame::Opened(proto::Opened::default())),
            }),
        ),
        ("error", Some(data(&vec![0; MAX_FRAME + 1]))),
    ];
    for (cause, sent) in causes {
        let mut live = rig.tunnel("operator", "node-07").await;
        match sent {
            Some(frame) => live.client.send(frame).await,
            None => drop(live.client),
        }
        let (bytes, said) = until_close(&mut live.out).await;
        assert_eq!((bytes.len(), said.as_str()), (0, cause));
        assert_eq!(
            rig.tunnel_lines().last().map(|line| line.3.clone()),
            Some(close_detail(
                live.id,
                if cause == "eof" {
                    Cause::Eof
                } else {
                    Cause::Error
                },
                Some((0, 0)),
                0
            ))
        );
    }
    let mut live = rig.tunnel("operator", "node-07").await;
    live.client.fail().await;
    let (_, said) = until_close(&mut live.out).await;
    assert_eq!(said, "error");
    assert_eq!(rig.tunnels.count(), 0);
}

/// Proves: a client that went away (its response stream is dropped) ends the tunnel, and a host
/// that resets its connection ends it `error`; both leave the book without the tunnel's row.
///
/// Catches: a relay left running for a client nobody reads, and a reset that is taken for the
/// host closing.
#[tokio::test]
async fn a_client_that_is_gone_and_a_host_that_resets_end_the_tunnel_with_an_error() {
    let mut rig = Rig::new("tunnel-gone", |_| {}).await;
    let live = rig.tunnel("operator", "node-07").await;
    let (id, host) = (live.id, live.host);
    drop(live.out);
    assert_eq!(rig.closed(id).await, Cause::Error);
    drop((host, live.client));

    let live = rig.tunnel("operator", "node-07").await;
    let (id, mut out) = (live.id, live.out);
    live.host.set_zero_linger().expect("the option");
    drop(live.host);
    assert_eq!(rig.closed(id).await, Cause::Error);
    let (_, cause) = until_close(&mut out).await;
    assert_eq!(cause, "error");
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
}

/// Proves: the host is read only while the queue towards the client has a slot: with a client that
/// reads nothing and a host that writes without end, the relay queues `QUEUE` frames and then
/// waits (it tells so), and when the client takes one frame it reads one more.
///
/// Catches: a relay that reads the host without bound into memory for a client that reads nothing.
#[tokio::test]
async fn the_host_is_read_only_as_fast_as_the_client_takes_frames() {
    let mut rig = Rig::new("tunnel-backpressure", |_| {}).await;
    let mut live = rig.tunnel("operator", "node-07").await;
    let writing = tokio::spawn({
        let mut host = live.host;
        async move {
            let chunk = vec![7_u8; MAX_FRAME];
            while host.write_all(&chunk).await.is_ok() {}
        }
    });
    let mut queued = 0;
    loop {
        match rig.events.recv().await.expect("an event") {
            Event::Down(_) => queued += 1,
            Event::Waiting(id) => {
                assert_eq!(id, live.id);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(queued, QUEUE);
    assert!(matches!(frame(&mut live.out).await, Frame::Data(_)));
    rig.event(|event| matches!(event, Event::Down(_))).await;
    writing.abort();
    drop(live.client);
}

/// Proves: a tunnel that moves no byte for the idle time ends `idle`, and a byte either way
/// postpones that: the tunnel is still open after more than the idle time has passed since it
/// opened, because bytes moved in between.
///
/// Catches: an idle timer that is not reset by the bytes of one direction, and one that never fires.
#[tokio::test(start_paused = true)]
async fn a_tunnel_that_moves_no_byte_for_the_idle_time_ends_idle() {
    let mut rig = Rig::new("tunnel-idle", |limits| {
        limits.idle = secs(100);
        limits.max_session = secs(100_000);
    })
    .await;
    let mut live = rig.tunnel("operator", "node-07").await;
    tokio::time::advance(secs(60)).await;
    live.host.write_all(b"x").await.expect("written");
    rig.event(|event| matches!(event, Event::Down(_))).await;
    tokio::time::advance(secs(60)).await;
    live.client.send(data(b"y")).await;
    assert_eq!(read(&mut live.host, 1).await, b"y");
    tokio::time::advance(secs(60)).await;
    assert_eq!(rig.tunnels.count(), 1);
    assert_eq!(rig.closed(live.id).await, Cause::Idle);
    assert_eq!(frame(&mut live.out).await, Frame::Data(b"x".to_vec()));
    let (_, cause) = until_close(&mut live.out).await;
    assert_eq!(cause, "idle");
}

/// Proves: a tunnel that keeps moving bytes still ends `max_session` when it has been open that
/// long.
///
/// Catches: a session limit that only an idle tunnel meets.
#[tokio::test(start_paused = true)]
async fn a_tunnel_that_stays_busy_ends_at_the_session_limit() {
    let mut rig = Rig::new("tunnel-session", |limits| {
        limits.idle = secs(30);
        limits.max_session = secs(60);
    })
    .await;
    let mut live = rig.tunnel("operator", "node-07").await;
    for _ in 0..2 {
        tokio::time::advance(secs(20)).await;
        live.host.write_all(b"x").await.expect("written");
        rig.event(|event| matches!(event, Event::Down(_))).await;
    }
    tokio::time::advance(secs(20)).await;
    assert_eq!(rig.closed(live.id).await, Cause::MaxSession);
    let (bytes, cause) = until_close(&mut live.out).await;
    assert_eq!(
        (bytes.as_slice(), cause.as_str()),
        (&b"xx"[..], "max_session")
    );
}

/// Proves: stopping a tunnel with any cause the poll or the drain gives ends it with that cause
/// (told to the client in `close` and written to the book, but for `instance_lost`, whose line
/// another instance wrote), and when two causes stop it the first stands.
///
/// Catches: a stop that is lost, a cause that is replaced by a later one, and a close line
/// written for a tunnel that another instance's reaper closed.
#[tokio::test]
async fn a_stopped_tunnel_ends_with_the_first_cause_and_the_client_is_told() {
    let mut rig = Rig::new("tunnel-stop", |_| {}).await;
    for cause in [
        Cause::TokenRevoked,
        Cause::NotPermitted,
        Cause::Revoked,
        Cause::CoordinatorStopping,
    ] {
        let mut live = rig.tunnel("operator", "node-07").await;
        rig.tunnels.stop(live.id, cause);
        rig.tunnels.stop(live.id, Cause::Idle);
        let (_, said) = until_close(&mut live.out).await;
        assert_eq!(said, cause.as_str());
        assert_eq!(
            rig.tunnel_lines().last().map(|line| line.3.clone()),
            Some(close_detail(live.id, cause, Some((0, 0)), 0))
        );
    }
    let mut live = rig.tunnel("operator", "node-07").await;
    rig.tunnels.stop(live.id, Cause::InstanceLost);
    let (_, said) = until_close(&mut live.out).await;
    assert_eq!(said, "instance_lost");
    assert_eq!(rig.book.inner.tunnel_ids(), vec![live.id]);
    rig.tunnels.stop(live.id + 100, Cause::Revoked);
    assert_eq!(rig.tunnels.count(), 0);
}

/// Proves: a book that cannot write the close does not keep the tunnel in the registry or from
/// telling its client why it ended; the failure is logged and the row stays for the reaper of
/// another instance.
///
/// Catches: a tunnel that never leaves the registry (and so holds the drain for its whole time)
/// because its close failed.
#[tokio::test]
async fn a_book_that_cannot_write_the_close_does_not_hold_the_tunnel() {
    let mut rig = Rig::new("tunnel-close-fails", |_| {}).await;
    rig.book.fail("close");
    let mut live = rig.tunnel("operator", "node-07").await;
    drop(live.host);
    let (_, said) = until_close(&mut live.out).await;
    assert_eq!(said, "host_closed");
    assert_eq!(rig.tunnels.count(), 0);
    assert_eq!(rig.book.inner.tunnel_ids(), vec![live.id]);
}

/// Proves: a book that panics in the close leaves the tunnel as it leaves one whose close failed:
/// the client is told why it ended and the registry no longer lists it.
///
/// Catches: a relay that dies with the book's thread and leaves its tunnel in the registry for
/// ever.
#[tokio::test]
async fn a_book_that_panics_in_the_close_does_not_hold_the_tunnel() {
    let mut rig = Rig::new("tunnel-close-panics", |_| {}).await;
    rig.book.fail("panic_close");
    let mut live = rig.tunnel("operator", "node-07").await;
    drop(live.host);
    let (_, said) = until_close(&mut live.out).await;
    assert_eq!(said, "host_closed");
    assert_eq!(rig.closed(live.id).await, Cause::HostClosed);
    assert_eq!(rig.tunnels.count(), 0);
}
