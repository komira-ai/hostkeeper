//! The write deadline of a listener's connections.

use std::future::poll_fn;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::task::noop_waker_ref;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use super::WriteDeadline;

const PATIENCE: Duration = Duration::from_millis(200);

/// Catches: a write that waits on a peer that does not read never failing: it keeps waiting
/// while the patience has not passed (however often it is polled), then fails with `TimedOut`.
#[tokio::test(start_paused = true)]
async fn a_write_the_peer_does_not_take_times_out_after_the_patience() {
    let (near, _far) = tokio::io::duplex(4);
    let mut io = WriteDeadline::new(near, PATIENCE);
    let mut cx = Context::from_waker(noop_waker_ref());
    let first = Pin::new(&mut io).poll_write(&mut cx, b"abcdefgh");
    let first = first.map(|written| written.map_err(|why| why.kind()));
    assert_eq!(first, Poll::Ready(Ok(4)));
    let waiting = Pin::new(&mut io).poll_write(&mut cx, b"efgh");
    assert!(waiting.is_pending());
    tokio::time::advance(PATIENCE / 2).await;
    let still = Pin::new(&mut io).poll_write(&mut cx, b"efgh");
    assert!(still.is_pending());
    tokio::time::advance(PATIENCE).await;
    let late = Pin::new(&mut io).poll_write(&mut cx, b"efgh");
    let late = late.map(|written| written.map_err(|why| why.kind()));
    assert_eq!(late, Poll::Ready(Err(io::ErrorKind::TimedOut)));
}

/// Catches: the deadline running from the first byte of a long response instead of from the
/// last write that made progress: a peer that keeps reading, slowly, is never cut off.
#[tokio::test(start_paused = true)]
async fn a_peer_that_keeps_reading_is_not_cut_off() {
    let (near, mut far) = tokio::io::duplex(4);
    let mut io = WriteDeadline::new(near, PATIENCE);
    let reader = tokio::spawn(async move {
        let mut got = Vec::new();
        let mut chunk = [0u8; 4];
        for _ in 0..5 {
            tokio::time::sleep(PATIENCE * 3 / 4).await;
            let n = far.read(&mut chunk).await.expect("read");
            got.extend_from_slice(&chunk[..n]);
        }
        got
    });
    let sent = io.write_all(&[7u8; 20]).await;
    assert_eq!(sent.map_err(|e| e.kind()), Ok(()));
    let got = reader.await.expect("joined");
    assert_eq!(got, vec![7u8; 20]);
}

/// Catches: flush, shutdown and read not passing through to the connection: they behave as
/// the inner stream's do.
#[tokio::test(start_paused = true)]
async fn flush_shutdown_and_read_reach_the_connection() {
    let (near, mut far) = tokio::io::duplex(64);
    let mut io = WriteDeadline::new(near, PATIENCE);
    far.write_all(b"ping").await.expect("sent");
    let mut got = [0u8; 4];
    let mut buf = ReadBuf::new(&mut got);
    poll_fn(|cx| Pin::new(&mut io).poll_read(cx, &mut buf))
        .await
        .expect("read");
    assert_eq!(buf.filled(), b"ping");
    io.write_all(b"pong").await.expect("written");
    io.flush().await.expect("flushed");
    io.shutdown().await.expect("shut down");
    let mut rest = Vec::new();
    far.read_to_end(&mut rest).await.expect("read to the end");
    assert_eq!(rest, b"pong");
}
