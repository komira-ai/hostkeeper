//! The listeners: TLS in front of [`crate::grpc::route`].
//!
//! A listener is a way to accept connections (a function; [`accept_tcp`] makes one of a bound
//! socket) and a [`TlsSource`]; what the connections are is the caller's, so a test serves
//! in-memory ones. Each listener holds a [`TlsSource`], so a certificate the farm CA renewed is picked up
//! without a restart (the files are looked at every interval, and a pair that does not make
//! a working configuration leaves the one in use). With enrollment, the same tick first renews
//! the coordinator's own server certificate when it is due or names other names than the flags
//! ([`reload_every`]). A heartbeat listener's source (the LAN listener's is the same one) requires a
//! client certificate that chains to the farm CA, or, with enrollment, lets a client without one
//! through the handshake and admits one of the coordinator's CA too (each route then decides:
//! only `Enroll` and `Poll` answer a caller without one); the private listener's asks for none. A
//! handshake offers `h2` and `http/1.1`, whatever the source's own protocols are: a connection
//! that negotiates `h2` speaks HTTP/2 (gRPC, and the JSON routes over HTTP/2), any other speaks
//! HTTP/1.1 (the JSON routes). Either has a limit on its handshake, its request body and on any
//! single write of a response that its peer does not read; an HTTP/1.1 connection has a limit on
//! its request head, and serves a WebSocket upgrade (the UI's terminal, [`crate::ssh_terminal_route`]; HTTP/2 does not: no
//! extended CONNECT); an HTTP/2 connection has a limit on its streams, pings its peer, takes new
//! calls for one patience only and is closed one patience after that, or, when it holds a
//! `Releases.WaitConverged` call then, one patience after the last such call ends
//! ([`crate::wait`]); a listener has a limit on its connections, and an upgraded HTTP/1.1
//! connection counts against it until the upgraded socket ends ([`Hold`]). A console stream
//! ([`crate::console`]) also takes its upgraded connection off the patience rule ([`Patience`]): a
//! write the browser does not read is not cut after a patience, and the stream counts against
//! `--console-max-sessions` instead. At the stop ([`crate::graceful`]) the heartbeat, private and
//! LAN listeners accept no more, and each of their connections is told to finish: an HTTP/2 one
//! goes away as it does after its patience, an HTTP/1.1 one closes after the request in flight,
//! and an upgraded one ends with its socket.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Either, select};
use hostkeeper_tls::TlsSource;
use hyper::body::Incoming;
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use rustls::ServerConfig;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio::time::{Instant, Sleep};
use tokio_rustls::TlsAcceptor;

use crate::console::ConsoleFiles;
use crate::enrollment::Enrollment;
use crate::graceful::Graceful;
use crate::grpc::route;
use crate::http::{Peer, PeerAddr, Surface};
use crate::operator_keys::OperatorKeys;
use crate::prerequisites::HostPrerequisites;
use crate::service::Coordinator;
use crate::ssh_terminal_route::Shared;
use crate::tokens::OperatorTokens;
use crate::wait::{ConnectionWaits, unless};

/// How long a client has to finish its TLS handshake, and then to send a request head.
pub const CLIENT_PATIENCE: Duration = Duration::from_secs(10);

/// The protocol a handshake settles on for HTTP/2, as ALPN names it.
const H2: &[u8] = b"h2";

/// The protocols a handshake offers, in the order of preference.
const ALPN: [&[u8]; 2] = [H2, b"http/1.1"];

/// The most requests one HTTP/2 connection serves at once. An updater or a gRPC operator client
/// sends one call at a time; the rest is slack for a client that pipelines.
pub const MAX_STREAMS: u32 = 16;

/// The most connections one listener serves at once. A client with a handshake, a head, a
/// body or an unread response in flight holds one for at most [`CLIENT_PATIENCE`] each, and an
/// HTTP/2 connection is closed two patiences after its handshake (or one after the last held
/// wait on it, of which a coordinator holds at most [`crate::wait::MAX_WAITS`]), so the cap makes
/// a flood wait in the kernel's backlog rather than take the process's descriptors and memory. An
/// upgraded HTTP/1.1 connection (the UI's terminal) holds its slot for as long as the upgraded
/// socket lives, which its own task bounds.
pub const MAX_CONNECTIONS: usize = 1024;

/// A transport that can say where it came from.
pub trait Remote {
    /// The address of the other end, or `None` for a transport with none (an in-memory pipe).
    fn remote(&self) -> Option<SocketAddr> {
        None
    }
}

impl Remote for TcpStream {
    fn remote(&self) -> Option<SocketAddr> {
        self.peer_addr().ok()
    }
}

impl Remote for tokio::io::DuplexStream {}

/// One listener: `accept` yields the next connection, as a function of no argument that
/// returns a future.
pub struct Listener<A> {
    /// Accepts the next connection.
    pub accept: A,
    /// Its TLS configuration.
    pub tls: TlsSource,
    /// What it serves.
    pub surface: Surface,
}

/// The accept function of a bound socket: each call waits for the next connection.
pub fn accept_tcp(
    tcp: TcpListener,
) -> impl FnMut() -> BoxFuture<'static, io::Result<TcpStream>> + Send {
    let tcp = Arc::new(tcp);
    move || {
        let tcp = Arc::clone(&tcp);
        async move { tcp.accept().await.map(|(stream, _)| stream) }.boxed()
    }
}

/// Serves `listener` for `coordinator` for as long as the future is polled, for a listener that
/// is never told to stop. A client gets `patience` for its handshake, again for its request head,
/// again for its request body and again for each write of a response that it does not read, and
/// at most `max_connections` are served at once (an upgraded socket is served until it ends). It
/// never returns.
pub fn serve<A, F, S>(
    listener: Listener<A>,
    coordinator: Arc<Coordinator>,
    patience: Duration,
    max_connections: usize,
) -> impl Future<Output = Infallible>
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    serve_until_stopped(
        listener,
        coordinator,
        patience,
        max_connections,
        Graceful::default(),
    )
}

/// [`serve`] for a listener of `graceful` (the heartbeat, private and LAN ones of a coordinator
/// that stops): each connection is counted as open on it while it is served, and is told to finish
/// when it is told to stop ([`crate::graceful`]). It never returns; the listener's accepting ends
/// by [`crate::graceful::accept_until_stopped`] dropping it.
pub(crate) fn serve_until_stopped<A, F, S>(
    listener: Listener<A>,
    coordinator: Arc<Coordinator>,
    patience: Duration,
    max_connections: usize,
    graceful: Graceful,
) -> impl Future<Output = Infallible>
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    serve_counted(
        listener,
        coordinator,
        patience,
        Arc::new(Semaphore::new(max_connections)),
        graceful,
    )
}

/// [`serve`] with the connection slots given: one permit is held for each connection being
/// served, so a caller can see how many are in use.
pub async fn serve_with_slots<A, F, S>(
    listener: Listener<A>,
    coordinator: Arc<Coordinator>,
    patience: Duration,
    slots: Arc<Semaphore>,
) -> Infallible
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    serve_counted(listener, coordinator, patience, slots, Graceful::default()).await
}

/// [`serve_with_slots`] counting its connections on `graceful`.
async fn serve_counted<A, F, S>(
    listener: Listener<A>,
    coordinator: Arc<Coordinator>,
    patience: Duration,
    slots: Arc<Semaphore>,
    graceful: Graceful,
) -> Infallible
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    let Listener {
        accept,
        tls,
        surface,
    } = listener;
    accept_each(accept, slots, graceful, move |stream, graceful| {
        connection_with(
            stream,
            tls.config(),
            surface,
            Arc::clone(&coordinator),
            patience,
            graceful,
        )
    })
    .await
}

/// Accepts the connections `accept` yields and serves each on a task of its own with
/// `serve_one` (which is given `graceful`, to end the connection by when it is told to stop),
/// holding one of `slots` for each while it is served and counting it as open on `graceful`
/// from the moment it is accepted: with no slot free, clients queue in the kernel's backlog. A
/// failed accept is logged and tried again after a pause. It never returns; the caller drops it
/// to stop accepting, and the connections already accepted go on on their tasks.
pub(crate) async fn accept_each<A, F, S, C>(
    mut accept: A,
    slots: Arc<Semaphore>,
    graceful: Graceful,
    mut serve_one: impl FnMut(S, Graceful) -> C,
) -> Infallible
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    C: Future<Output = ()> + Send + 'static,
{
    loop {
        // A slot first, then the accept: with none free, clients queue in the backlog.
        // (The semaphore is never closed, so this is always `Some`.)
        let slot = Arc::clone(&slots).acquire_owned().await.ok();
        match accept().await {
            Ok(stream) => {
                // Counted before the spawn, with no await between the accept and here: a stop
                // that drops this loop leaves no accepted connection uncounted.
                let open = graceful.open();
                let serving = serve_one(stream, graceful.clone());
                tokio::spawn(async move {
                    serving.await;
                    drop(slot);
                    drop(open);
                });
            }
            Err(why) => {
                tracing::warn!(%why, "accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Held by an upgraded socket: the connection that was upgraded ends, and so frees its slot of the
/// listener, only when every hold on it is dropped.
#[derive(Clone)]
pub(crate) struct Hold {
    _held: mpsc::Sender<()>,
}

/// Handed to each request of a connection: a route that upgrades the connection to a console
/// stream releases it, and from then on no write of the connection is held to the patience (the
/// bridge's own bounds hold a browser that reads nothing).
#[derive(Clone, Default)]
pub(crate) struct Patience(Arc<AtomicBool>);

impl Patience {
    /// Takes the connection off the patience rule.
    pub(crate) fn release(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether the connection was taken off the patience rule.
    fn released(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What a connection hands each of its requests besides the request: its held waits, the
/// coordinator (which the upgrade of a UI terminal outlives the request with) and the address it
/// came from when it has one.
fn carry<B>(
    request: &mut hyper::Request<B>,
    waits: &ConnectionWaits,
    remote: Option<SocketAddr>,
    coordinator: &Arc<Coordinator>,
) {
    request.extensions_mut().insert(waits.clone());
    request
        .extensions_mut()
        .insert(Shared(Arc::clone(coordinator)));
    if let Some(from) = remote {
        request.extensions_mut().insert(PeerAddr(from));
    }
}

/// Two futures that never finish, polled together: it never returns.
pub(crate) async fn together(
    first: impl Future<Output = Infallible>,
    second: impl Future<Output = Infallible>,
) -> Infallible {
    let (Either::Left((never, _)) | Either::Right((never, _))) =
        select(pin!(first), pin!(second)).await;
    match never {}
}

/// A connection's IO whose writes fail with [`io::ErrorKind::TimedOut`] when one makes no
/// progress for the patience, so a peer that sends requests and stops reading the responses
/// does not keep its connection for ever. Reads pass through.
pub(crate) struct WriteDeadline<S> {
    inner: S,
    deadline: Deadline,
    patience: Patience,
}

/// The clock of a [`WriteDeadline`]: started when a write first has to wait, and cleared
/// when a write makes progress.
struct Deadline {
    patience: Duration,
    sleep: Pin<Box<Sleep>>,
    running: bool,
}

impl Deadline {
    fn new(patience: Duration) -> Self {
        Self {
            patience,
            sleep: Box::pin(tokio::time::sleep(patience)),
            running: false,
        }
    }

    /// `polled`, the outcome of one write-side poll, or a timeout error when it has been
    /// waiting for longer than the patience.
    fn limit(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<usize>>,
    ) -> Poll<io::Result<usize>> {
        if polled.is_ready() {
            self.running = false;
            return polled;
        }
        if !self.running {
            self.sleep.as_mut().reset(Instant::now() + self.patience);
            self.running = true;
        }
        match self.sleep.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.running = false;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the peer did not read its response in time",
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> WriteDeadline<S> {
    /// `inner`, whose writes fail once one makes no progress for `patience`.
    pub(crate) fn new(inner: S, patience: Duration) -> Self {
        Self::released_by(inner, patience, Patience::default())
    }

    /// As [`Self::new`], and no write is limited once `patience` is released.
    pub(crate) fn released_by(inner: S, limit: Duration, patience: Patience) -> Self {
        Self {
            inner,
            deadline: Deadline::new(limit),
            patience,
        }
    }

    /// `polled`, limited by the deadline unless the connection was released from it.
    fn limit(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<usize>>,
    ) -> Poll<io::Result<usize>> {
        if self.patience.released() {
            return polled;
        }
        self.deadline.limit(cx, polled)
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for WriteDeadline<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WriteDeadline<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, data);
        self.limit(cx, polled)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_flush(cx).map_ok(|()| 0);
        self.limit(cx, polled).map_ok(|_| ())
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_shutdown(cx).map_ok(|()| 0);
        self.limit(cx, polled).map_ok(|_| ())
    }
}

/// Serves one connection until it ends: the TLS handshake, then HTTP/2 when the handshake
/// settled on `h2` and HTTP/1.1 otherwise, each with its limit. The address the stream came
/// from rides on each request as [`PeerAddr`] (no extension when the stream has none).
pub async fn connection<S>(
    stream: S,
    tls: TlsSource,
    surface: Surface,
    coordinator: Arc<Coordinator>,
    patience: Duration,
) where
    S: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    connection_with(
        stream,
        tls.config(),
        surface,
        coordinator,
        patience,
        Graceful::default(),
    )
    .await;
}

/// [`connection`] over the configuration `config`, which the caller took for this connection,
/// and told to finish once `graceful` is told to stop: an HTTP/2 connection goes away as it does
/// after its patience (no new stream; the calls in flight finish, a held wait keeps it until the
/// wait ends), an HTTP/1.1 connection closes after the request in flight (one that is idle closes
/// at once), and an upgraded one is left to its socket. A connection still in its TLS handshake
/// at the stop finishes it by its patience and is told then.
pub(crate) async fn connection_with<S>(
    stream: S,
    config: Arc<ServerConfig>,
    surface: Surface,
    coordinator: Arc<Coordinator>,
    patience: Duration,
    graceful: Graceful,
) where
    S: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    let remote = stream.remote();
    let mut config = (*config).clone();
    config.alpn_protocols = ALPN.iter().map(|name| name.to_vec()).collect();
    let shaken = tokio::time::timeout(patience, TlsAcceptor::from(Arc::new(config)).accept(stream));
    let tls = match shaken.await {
        Ok(Ok(tls)) => tls,
        Ok(Err(why)) => {
            tracing::debug!(%why, ?remote, "a TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!(?remote, "a TLS handshake did not finish in time");
            return;
        }
    };
    let peer: Peer = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(<[_]>::first)
        .map(|leaf| Arc::from(leaf.as_ref()));
    let speaks_h2 = tls.get_ref().1.alpn_protocol() == Some(H2);
    let waits = ConnectionWaits::default();
    let held = waits.clone();
    let (held_by, mut upgraded) = mpsc::channel::<()>(1);
    let hold = Hold { _held: held_by };
    let patience_of = Patience::default();
    let released = patience_of.clone();
    let service = service_fn(move |mut request: hyper::Request<Incoming>| {
        let coordinator = Arc::clone(&coordinator);
        let peer = peer.clone();
        carry(&mut request, &held, remote, &coordinator);
        request.extensions_mut().insert(hold.clone());
        request.extensions_mut().insert(released.clone());
        async move { Ok::<_, Infallible>(route(&coordinator, surface, &peer, request, patience).await) }
    });
    let io = TokioIo::new(WriteDeadline::released_by(tls, patience, patience_of));
    let served = if speaks_h2 {
        let serving = http2::Builder::new(TokioExecutor::new())
            .timer(TokioTimer::new())
            .max_concurrent_streams(MAX_STREAMS)
            .keep_alive_interval(patience)
            .keep_alive_timeout(patience)
            .serve_connection(io, service);
        let mut serving = pin!(serving);
        // An HTTP/2 connection takes new calls for one patience, or until the stop: then it is
        // told to go away (no new stream; the calls in flight finish), and dropped one patience
        // later, so an idle client that answers pings cannot keep a slot of the listener. A held
        // wait keeps it: it is dropped one patience after the last held wait on it ends.
        let ended = match select(
            pin!(tokio::time::timeout(patience, serving.as_mut())),
            pin!(graceful.stopped()),
        )
        .await
        {
            Either::Left((Ok(ended), _)) => Some(ended),
            Either::Left((Err(_), _)) | Either::Right(((), _)) => None,
        };
        match ended {
            Some(ended) => ended,
            None => {
                serving.as_mut().graceful_shutdown();
                unless(waits.quiet_for(patience), serving)
                    .await
                    .unwrap_or(Ok(()))
            }
        }
    } else {
        let serving = http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(patience)
            .serve_connection(io, service)
            .with_upgrades();
        let mut serving = pin!(serving);
        // At the stop an HTTP/1.1 connection closes after the request in flight; one that is
        // idle closes at once. One upgraded before the stop has ended here already.
        let ended = match select(serving.as_mut(), pin!(graceful.stopped())).await {
            Either::Left((ended, _)) => Some(ended),
            Either::Right(((), _)) => None,
        };
        match ended {
            Some(ended) => ended,
            None => {
                serving.as_mut().graceful_shutdown();
                serving.await
            }
        }
    };
    if let Err(why) = served {
        tracing::debug!(%why, "a connection ended badly");
    }
    // An upgrade ends the connection's own service and leaves the socket to its task: the
    // connection is not over, and its slot is not free, until that task lets go of its hold.
    upgraded.recv().await;
}

/// Looks at everything that is re-read while the coordinator runs, every `interval`, for as long
/// as the future is polled: with `enrollment`, its server certificate file, made again when it
/// is due or names other names than the flags ([`Enrollment::renew_server`]), and, when its
/// `bool` says so, its requests, expired once they waited longer than the request TTL
/// ([`Enrollment::expire_requests`]; over a database the claimed job `request-expiry` does it
/// instead, [`crate::jobs`]); then the TLS files of `sources`, the
/// tokens file of `tokens`, the operator keys file of `operator_keys`, the host prerequisites
/// file of `prerequisites` and the console's files of `console`. It never returns.
pub async fn reload_every(
    enrollment: Option<(Arc<Enrollment>, bool)>,
    sources: Vec<TlsSource>,
    tokens: OperatorTokens,
    operator_keys: OperatorKeys,
    prerequisites: HostPrerequisites,
    console: ConsoleFiles,
    interval: Duration,
) -> Infallible {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticks.tick().await;
    loop {
        ticks.tick().await;
        reload_all(
            enrollment.as_ref().map(|(enrollment, _)| enrollment),
            enrollment.as_ref().is_some_and(|(_, expire)| *expire),
            &sources,
            &tokens,
            &operator_keys,
            &prerequisites,
            &console,
        )
        .await;
    }
}

/// One look of the tick: the server certificate of `enrollment` is renewed when it is due and,
/// with `expire`, its requests older than the request TTL expire, then each TLS source reloads (so a renewed
/// file is served from this look on), then the tokens file, the operator keys file, the host
/// prerequisites file and the console's files are read again.
pub(crate) async fn reload_all(
    enrollment: Option<&Arc<Enrollment>>,
    expire: bool,
    sources: &[TlsSource],
    tokens: &OperatorTokens,
    operator_keys: &OperatorKeys,
    prerequisites: &HostPrerequisites,
    console: &ConsoleFiles,
) {
    if let Some(enrollment) = enrollment {
        let enrollment = Arc::clone(enrollment);
        // File reads, a key, a serial from the store and file writes: off the runtime's threads.
        let _ = tokio::task::spawn_blocking(move || {
            enrollment.renew_server();
            if expire {
                enrollment.expire_requests();
            }
        })
        .await;
    }
    for source in sources {
        let source = source.clone();
        // File reads and a key parse: off the runtime's threads.
        let _ = tokio::task::spawn_blocking(move || source.reload()).await;
    }
    let tokens = tokens.clone();
    let _ = tokio::task::spawn_blocking(move || tokens.reload()).await;
    let operator_keys = operator_keys.clone();
    let _ = tokio::task::spawn_blocking(move || operator_keys.reload()).await;
    let prerequisites = prerequisites.clone();
    let _ = tokio::task::spawn_blocking(move || prerequisites.reload()).await;
    let console = console.clone();
    let _ = tokio::task::spawn_blocking(move || console.reload()).await;
}

#[cfg(test)]
#[path = "listen_tests.rs"]
pub(crate) mod tests;
