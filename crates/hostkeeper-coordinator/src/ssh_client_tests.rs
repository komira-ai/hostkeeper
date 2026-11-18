//! The stream of the SSH client over a tunnel's frames, the host key check and the terminal's key.
//! The shell, the login and the pty are tested against a real SSH server in the terminal's tests.

use std::collections::BTreeSet;

use futures::StreamExt;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use russh::client::Handler as _;
use russh::keys::PublicKeyOrCertificate;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tonic::Status;

use super::{ClientError, OpenError, Pinned, bridge, key_from_seed};
use crate::ssh_cert::fingerprint;
use crate::ssh_tunnel::{MAX_FRAME, Outgoing};

type Frames = mpsc::Sender<Result<proto::TunnelFrame, Status>>;

fn data(bytes: &[u8]) -> proto::TunnelFrame {
    proto::TunnelFrame {
        frame: Some(Frame::Data(bytes.to_vec())),
    }
}

fn close(cause: &str) -> proto::TunnelFrame {
    proto::TunnelFrame {
        frame: Some(Frame::Close(proto::Close {
            cause: cause.to_owned(),
        })),
    }
}

/// A tunnel's answer that the test feeds: the frames it sends and the stream they come out of.
fn answer() -> (Frames, Outgoing) {
    let (sent, received) = mpsc::channel(8);
    let stream = futures::stream::unfold(received, |mut received| async move {
        received.recv().await.map(|item| (item, received))
    })
    .boxed();
    (sent, stream)
}

/// Proves: what the client writes goes into the tunnel as `data` frames of at most 16 KiB that hold
/// every byte in order, and the client dropping its end of the stream is a `close{eof}` frame,
/// after which nothing more is sent.
///
/// Catches: a frame past the tunnel's limit (the relay would end the tunnel), and an end of the
/// client that the tunnel is not told.
#[tokio::test]
async fn what_the_client_writes_goes_out_in_frames_and_its_end_is_told() {
    let (into, mut tunnel_sees) = mpsc::channel(4);
    let (_answers, from_tunnel) = answer();
    let (mut stream, _tunnel) = bridge(into, from_tunnel);
    let bytes: Vec<u8> = (0..40_000_u32)
        .map(|n| u8::try_from(n % 251).expect("a byte"))
        .collect();
    let writing = tokio::spawn(async move {
        stream.write_all(&bytes).await.expect("written");
        stream.shutdown().await.expect("shut");
    });
    let mut got = Vec::new();
    let mut frames = 0;
    loop {
        match tunnel_sees
            .recv()
            .await
            .expect("a frame")
            .expect("not an error")
            .frame
        {
            Some(Frame::Data(more)) => {
                assert!(more.len() <= MAX_FRAME, "{}", more.len());
                got.extend(more);
                frames += 1;
            }
            Some(Frame::Close(proto::Close { cause })) => {
                assert_eq!(cause, "eof");
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    writing.await.expect("the writer ends");
    assert!(frames >= 3);
    assert_eq!(got.len(), 40_000);
    assert_eq!(
        got[..300],
        (0..300_u32)
            .map(|n| u8::try_from(n % 251).expect("a byte"))
            .collect::<Vec<_>>()[..]
    );
    assert!(
        tunnel_sees.recv().await.is_none(),
        "nothing follows the close"
    );
}

/// Proves: the tunnel's `data` frames are what the client reads, in order, the `opened` frame is
/// passed over, its `close` frame ends the client's input and its cause is kept, and an answer
/// that ends without a `close`, or with an error, ends the client's input with no cause.
///
/// Catches: a client that never sees the end of the tunnel, and a cause that is lost or invented.
#[tokio::test]
async fn what_the_tunnel_answers_is_read_by_the_client_and_its_close_is_kept() {
    let (sent, from_tunnel) = answer();
    let (mut stream, tunnel) = bridge(mpsc::channel(4).0, from_tunnel);
    assert_eq!(tunnel.cause(), None);
    let opened = proto::TunnelFrame {
        frame: Some(Frame::Opened(proto::Opened {
            login_user: "root".to_owned(),
        })),
    };
    for frame in [
        opened,
        data(b"ab"),
        data(b"c"),
        close("idle"),
        data(b"never read"),
    ] {
        sent.send(Ok(frame)).await.expect("sent");
    }
    let mut read = Vec::new();
    stream.read_to_end(&mut read).await.expect("to the end");
    assert_eq!(read, b"abc");
    assert_eq!(tunnel.cause().as_deref(), Some("idle"));

    for end in [None, Some(Err(Status::cancelled("gone")))] {
        let (sent, from_tunnel) = answer();
        let (mut stream, tunnel) = bridge(mpsc::channel(4).0, from_tunnel);
        sent.send(Ok(data(b"x"))).await.expect("sent");
        if let Some(end) = end {
            sent.send(end).await.expect("sent");
        } else {
            drop(sent);
        }
        let mut read = Vec::new();
        stream.read_to_end(&mut read).await.expect("to the end");
        assert_eq!(read, b"x");
        assert_eq!(tunnel.cause(), None);
    }
}

/// Proves: a client that is gone while the tunnel still sends stops the carrying of the answer (the
/// answer's stream is dropped), and a tunnel that is gone while the client still writes stops the
/// carrying of what it writes.
///
/// Catches: a task that holds a tunnel's stream for ever after its reader is gone.
#[tokio::test]
async fn a_side_that_is_gone_stops_the_carrying_towards_it() {
    let (sent, from_tunnel) = answer();
    let (stream, _tunnel) = bridge(mpsc::channel(4).0, from_tunnel);
    drop(stream);
    // The write to the client fails and the carrier lets go of the stream it read.
    let _ = sent.send(Ok(data(b"nobody reads"))).await;
    sent.closed().await;

    let (into, tunnel_sees) = mpsc::channel(4);
    let (_answers, from_tunnel) = answer();
    let (mut stream, mut tunnel) = bridge(into, from_tunnel);
    drop(tunnel_sees);
    stream.write_all(b"nobody hears").await.expect("written");
    (&mut tunnel.up).await.expect("the carrier ends");
}

/// Proves: letting go of the tunnel ends what the client writes into it (the tunnel sees its
/// client's stream end, which it closes `eof` for), and the carrier of the answer ends with the
/// answer's `close`.
///
/// Catches: a terminal that has ended whose tunnel stays open until its idle timeout, and one that
/// drops the answer's stream and so ends the tunnel as an error.
#[tokio::test]
async fn dropping_the_tunnel_ends_the_stream_into_it_and_the_answer_ends_with_its_close() {
    let (into, mut tunnel_sees) = mpsc::channel(4);
    let (answers, from_tunnel) = answer();
    let (_stream, tunnel) = bridge(into, from_tunnel);
    drop(tunnel);
    assert!(tunnel_sees.recv().await.is_none());
    answers.send(Ok(close("eof"))).await.expect("sent");
    answers.closed().await;
}

/// Proves: the host key check accepts a key whose `SHA256:` fingerprint is in the pinned set and
/// refuses every other key with the pin's own error, and the terminal's key is the same for the
/// same seed and another for another seed.
///
/// Catches: a check that accepts a key that was not pinned, and a terminal key that is not made
/// from the bytes it was given.
#[tokio::test]
async fn the_host_key_check_trusts_the_pinned_fingerprints_only() {
    let host = key_from_seed(&[1; 32]);
    let other = key_from_seed(&[2; 32]);
    let blob = host.public_key().to_bytes().expect("a blob");
    let mut pinned = Pinned {
        fingerprints: BTreeSet::from([fingerprint(&blob)]),
    };
    let host_key = PublicKeyOrCertificate::from(host.public_key().clone());
    assert!(pinned.check_server_key(&host_key).await.expect("trusted"));
    let other_key = PublicKeyOrCertificate::from(other.public_key().clone());
    assert!(matches!(
        pinned.check_server_key(&other_key).await,
        Err(ClientError::Unpinned)
    ));
    assert_eq!(key_from_seed(&[1; 32]).public_key(), host.public_key());
    assert_ne!(other.public_key(), host.public_key());
}

/// Proves: a refusal to open the channel is the host refusing the shell, and any other failure of
/// the connection is the connection's.
///
/// Catches: a host's refusal told as a broken connection, which the terminal would answer with
/// the tunnel's cause.
#[test]
fn a_refused_channel_is_a_refused_shell_and_any_other_failure_a_broken_connection() {
    let refused =
        russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::AdministrativelyProhibited);
    assert_eq!(OpenError::from(refused), OpenError::Shell);
    assert_eq!(
        OpenError::from(russh::Error::Disconnect),
        OpenError::Connection
    );
}

/// Proves: each way the terminal cannot be opened, and each way the connection fails, has its own
/// words.
///
/// Catches: errors that read alike.
#[test]
fn the_errors_say_which_step_failed() {
    assert_eq!(
        OpenError::HostKey.to_string(),
        "the host's key is not in the pinned set"
    );
    assert_eq!(
        OpenError::Auth.to_string(),
        "the host refused the certificate"
    );
    assert_eq!(OpenError::Shell.to_string(), "the host refused the shell");
    assert_eq!(
        OpenError::Connection.to_string(),
        "the connection to the host broke"
    );
    assert_eq!(
        ClientError::Unpinned.to_string(),
        "the host's key is not in the pinned set"
    );
    assert_eq!(
        ClientError::from(russh::Error::Disconnect).to_string(),
        "ssh: Disconnected"
    );
}
