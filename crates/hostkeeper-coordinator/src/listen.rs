//! The listeners: TLS in front of [`crate::http::handle`].
//!
//! A listener is a way to accept connections (a function; [`accept_tcp`] makes one of a bound
//! socket) and a [`TlsSource`]; what the connections are is the caller's, so a test serves
//! in-memory ones. Each listener holds a [`TlsSource`], so a certificate the farm CA renewed is picked up
//! without a restart (the files are looked at every interval, and a pair that does not make
//! a working configuration leaves the one in use). A heartbeat listener's source requires a
//! client certificate that chains to the farm CA; a control listener's asks for none. A
//! connection speaks HTTP/1.1 only, and has a limit on its handshake, its request head, its
//! request body and on any single write of a response that its peer does not read; a listener
//! has a limit on its connections.

use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use hostkeeper_tls::TlsSource;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::{Instant, Sleep};
use tokio_rustls::TlsAcceptor;

use crate::http::{Peer, Surface, handle};
use crate::service::Coordinator;

/// How long a client has to finish its TLS handshake, and then to send a request head.
pub const CLIENT_PATIENCE: Duration = Duration::from_secs(10);

/// The most connections one listener serves at once. A client with a handshake, a head, a
/// body or an unread response in flight holds one for at most [`CLIENT_PATIENCE`] each, so the cap makes a flood
/// wait in the kernel's backlog rather than take the process's descriptors and memory.
pub const MAX_CONNECTIONS: usize = 1024;

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

/// Serves `listener` for `coordinator` for as long as the future is polled. A client gets
/// `patience` for its handshake, again for its request head, again for its request body and
/// again for each write of a response that it does not read, and at most `max_connections` are served at once. It never returns.
pub fn serve<A, F, S>(
    listener: Listener<A>,
    coordinator: Arc<Coordinator>,
    patience: Duration,
    max_connections: usize,
) -> impl Future<Output = Infallible>
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve_with_slots(
        listener,
        coordinator,
        patience,
        Arc::new(Semaphore::new(max_connections)),
    )
}

/// [`serve`] with the connection slots given: one permit is held for each connection being
/// served, so a caller can see how many are in use.
pub async fn serve_with_slots<A, F, S>(
    mut listener: Listener<A>,
    coordinator: Arc<Coordinator>,
    patience: Duration,
    slots: Arc<Semaphore>,
) -> Infallible
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    loop {
        // A slot first, then the accept: with none free, clients queue in the backlog.
        // (The semaphore is never closed, so this is always `Some`.)
        let slot = Arc::clone(&slots).acquire_owned().await.ok();
        match (listener.accept)().await {
            Ok(stream) => {
                let serving = connection(
                    stream,
                    listener.tls.clone(),
                    listener.surface,
                    Arc::clone(&coordinator),
                    patience,
                );
                tokio::spawn(async move {
                    serving.await;
                    drop(slot);
                });
            }
            Err(why) => {
                tracing::warn!(%why, "accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// A connection's IO whose writes fail with [`io::ErrorKind::TimedOut`] when one makes no
/// progress for the patience, so a peer that sends requests and stops reading the responses
/// does not keep its connection for ever. Reads pass through.
struct WriteDeadline<S> {
    inner: S,
    deadline: Deadline,
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
    fn new(inner: S, patience: Duration) -> Self {
        Self {
            inner,
            deadline: Deadline::new(patience),
        }
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
        self.deadline.limit(cx, polled)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_flush(cx).map_ok(|()| 0);
        self.deadline.limit(cx, polled).map_ok(|_| ())
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_shutdown(cx).map_ok(|()| 0);
        self.deadline.limit(cx, polled).map_ok(|_| ())
    }
}

/// Serves one connection until it ends: the TLS handshake, then HTTP/1.1, each with its limit.
pub async fn connection<S>(
    stream: S,
    tls: TlsSource,
    surface: Surface,
    coordinator: Arc<Coordinator>,
    patience: Duration,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut config = (*tls.config()).clone();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let shaken = tokio::time::timeout(patience, TlsAcceptor::from(Arc::new(config)).accept(stream));
    let tls = match shaken.await {
        Ok(Ok(tls)) => tls,
        Ok(Err(why)) => {
            tracing::debug!(%why, "a TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!("a TLS handshake did not finish in time");
            return;
        }
    };
    let peer: Peer = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(<[_]>::first)
        .map(|leaf| Arc::from(leaf.as_ref()));
    let service = service_fn(move |request| {
        let coordinator = Arc::clone(&coordinator);
        let peer = peer.clone();
        async move { Ok::<_, Infallible>(handle(&coordinator, surface, &peer, request, patience).await) }
    });
    let served = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(patience)
        .serve_connection(TokioIo::new(WriteDeadline::new(tls, patience)), service)
        .await;
    if let Err(why) = served {
        tracing::debug!(%why, "a connection ended badly");
    }
}

/// Looks at the TLS files of `sources` every `interval`, for as long as the future is polled.
/// It never returns.
pub async fn reload_every(sources: Vec<TlsSource>, interval: Duration) -> Infallible {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticks.tick().await;
    loop {
        ticks.tick().await;
        for source in &sources {
            let source = source.clone();
            // File reads and a key parse: off the runtime's threads.
            let _ = tokio::task::spawn_blocking(move || source.reload()).await;
        }
    }
}

#[cfg(test)]
#[path = "listen_tests.rs"]
mod tests;
