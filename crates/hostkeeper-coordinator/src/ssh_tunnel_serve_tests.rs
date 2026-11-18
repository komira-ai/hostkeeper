//! A tunnel carried by the private listener as a client carries it: a real HTTP/2 client over TLS
//! (in memory) sends a gRPC stream whose request body stays open, and the host's sshd is a
//! listener on a port of loopback. What the router tests cannot show (they hand the request body
//! whole) is shown here: the answer arrives while the client's stream is still open, bytes go
//! both ways as they come, and the connection outlives the listener's patience for as long as
//! the tunnel holds it.

use std::io;
use std::sync::Arc;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::StreamExt;
use futures::stream::BoxStream;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame as BodyFrame, Incoming};
use hyper::{HeaderMap, Request};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tonic::{Code, Status};

use super::Event;
use crate::http::Surface;
use crate::listen::tests::{connector, tls_material};
use crate::listen::{CLIENT_PATIENCE, connection};
use crate::ssh_book::Cause;
use crate::ssh_tunnel::tests::{Rig, close, data, open};
use crate::testutil::OPERATOR;

/// The request body of a client whose stream the test keeps open: what it sends, frame by frame.
type Upload = StreamBody<BoxStream<'static, Result<BodyFrame<Bytes>, io::Error>>>;

/// One gRPC message of `frame`: the compression flag, the length, the encoded frame.
fn message(frame: &proto::TunnelFrame) -> BodyFrame<Bytes> {
    let encoded = frame.encode_to_vec();
    let mut bytes = BytesMut::new();
    bytes.put_u8(0);
    bytes.put_u32(u32::try_from(encoded.len()).expect("a short frame"));
    bytes.put_slice(&encoded);
    BodyFrame::data(bytes.freeze())
}

/// The frames of a response as they arrive, and the trailers that end it.
struct Replies {
    body: Incoming,
    buffer: BytesMut,
    trailers: Option<HeaderMap>,
}

impl Replies {
    /// The next frame, or `None` once the body has ended.
    async fn next(&mut self) -> Option<Frame> {
        loop {
            if self.buffer.len() >= 5 {
                let length = u32::from_be_bytes([
                    self.buffer[1],
                    self.buffer[2],
                    self.buffer[3],
                    self.buffer[4],
                ]) as usize;
                if self.buffer.len() >= 5 + length {
                    self.buffer.advance(5);
                    let bytes = self.buffer.split_to(length);
                    let frame = proto::TunnelFrame::decode(bytes.freeze()).expect("a frame");
                    return Some(frame.frame.expect("a frame with a case"));
                }
            }
            match self.body.frame().await {
                Some(Ok(piece)) => match piece.into_data() {
                    Ok(data) => self.buffer.extend_from_slice(&data),
                    Err(piece) => self.trailers = piece.into_trailers().ok(),
                },
                Some(Err(why)) => panic!("the response failed: {why}"),
                None => return None,
            }
        }
    }

    /// The bytes of `Data` frames until there are `wanted`.
    async fn bytes(&mut self, wanted: usize) -> Vec<u8> {
        let mut got = Vec::new();
        while got.len() < wanted {
            match self.next().await {
                Some(Frame::Data(more)) => got.extend(more),
                other => panic!("not the host's bytes: {other:?}"),
            }
        }
        got
    }
}

/// Proves: over a real HTTP/2 connection to the private listener, `Ssh/Tunnel` answers `opened`
/// while the client's request stream is still open; bytes the client sends reach the host and
/// the host's come back as `data`, each as it is written; the connection is not closed after the
/// listener's patience while the tunnel is open; the client's `close` ends only its input (the
/// host reads end of file and still answers); and when the host closes the stream ends with
/// `close{host_closed}` and the status `OK` in the trailers, the book holding the close.
///
/// Catches: a handler that answers only once the client's stream has ended, a relay that waits for
/// a whole message in either direction, a connection the patience closes under an open tunnel, a
/// client `close` that ends the host's answer too, and a stream that ends without its status.
#[tokio::test]
async fn a_tunnel_is_carried_over_http2_both_ways_while_its_stream_stays_open() {
    let mut rig = Rig::new("serve-tunnel", |_| {}).await;
    let serial = rig.cert("operator", "node-07");
    let (source, ca) = tls_material();
    let (client, server) = tokio::io::duplex(64 * 1024);
    tokio::spawn(connection(
        server,
        source,
        Surface::Control,
        Arc::clone(&rig.coordinator),
        CLIENT_PATIENCE,
    ));
    let tls = connector(&ca, &[b"h2"])
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake");
    let (mut send, driver) = hyper::client::conn::http2::handshake::<_, _, Upload>(
        TokioExecutor::new(),
        TokioIo::new(tls),
    )
    .await
    .expect("an HTTP/2 handshake");
    tokio::spawn(driver);

    let (upload, mut uploads) = mpsc::channel::<proto::TunnelFrame>(8);
    let body: Upload = StreamBody::new(
        futures::stream::poll_fn(move |cx| uploads.poll_recv(cx))
            .map(|frame| Ok(message(&frame)))
            .boxed(),
    );
    let request = Request::builder()
        .method("POST")
        .uri("https://localhost/hostkeeper.operator.v1.Ssh/Tunnel")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(body)
        .expect("a request");
    upload.send(open("node-07", serial)).await.expect("sent");
    let response = send.send_request(request).await.expect("an answer");
    assert_eq!(response.status(), 200);
    let mut replies = Replies {
        body: response.into_body(),
        buffer: BytesMut::new(),
        trailers: None,
    };
    assert_eq!(
        replies.next().await,
        Some(Frame::Opened(proto::Opened {
            login_user: "root".to_owned()
        }))
    );
    let (mut host, _) = rig.listener.accept().await.expect("the host is dialled");
    let Event::Opened(id) = rig.event(|e| matches!(e, Event::Opened(_))).await else {
        unreachable!("an event that was asked for");
    };

    upload.send(data(b"hello")).await.expect("sent");
    let mut heard = [0_u8; 5];
    host.read_exact(&mut heard).await.expect("the host hears");
    assert_eq!(&heard, b"hello");
    host.write_all(b"world").await.expect("written");
    assert_eq!(replies.bytes(5).await, b"world");

    // Quiet for three patiences, the tunnel is still carried: its stream holds the connection.
    tokio::time::pause();
    tokio::time::sleep(CLIENT_PATIENCE * 3).await;
    upload.send(data(b"again")).await.expect("sent");
    host.read_exact(&mut heard).await.expect("the host hears");
    assert_eq!(&heard, b"again");
    host.write_all(b"still").await.expect("written");
    assert_eq!(replies.bytes(5).await, b"still");

    upload.send(close("eof")).await.expect("sent");
    let mut rest = Vec::new();
    host.read_to_end(&mut rest)
        .await
        .expect("the end of the client's input");
    assert!(rest.is_empty());
    host.write_all(b"bye")
        .await
        .expect("the host still answers");
    drop(host);
    assert_eq!(replies.bytes(3).await, b"bye");
    assert_eq!(
        replies.next().await,
        Some(Frame::Close(proto::Close {
            cause: "host_closed".to_owned()
        }))
    );
    assert_eq!(replies.next().await, None);
    let trailers = replies.trailers.expect("trailers");
    let status = Status::from_header_map(&trailers).expect("a grpc-status");
    assert_eq!(status.code(), Code::Ok);
    assert_eq!(rig.closed(id).await, Cause::HostClosed);
}
