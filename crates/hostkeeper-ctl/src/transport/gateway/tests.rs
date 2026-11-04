//! The gateway client: the request it writes and the responses it reads, on a fake stream; and a
//! call over TLS to a loopback server (127.0.0.1, port 0) with the test PKI.

use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc;

use bytes::Bytes;
use prost::Message as _;
use prost_reflect::DynamicMessage;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tonic::Code;
use tonic::metadata::{MetadataMap, MetadataValue};

use super::{MAX_RESPONSE_BYTES, ReadError, error_status, exchange, method, read_response};
use crate::codes;
use crate::testutil::{Pki, file, from_json, loopback_target, pki, pool, scratch};
use crate::transport::{Connect as _, Network};
use crate::{CtlTransport, TransportKind};

/// A connection whose peer has already written `reply`; what is written to it is kept.
struct FakeStream {
    reply: Cursor<Vec<u8>>,
    written: Vec<u8>,
    fail_write: bool,
    fail_flush: bool,
}

impl FakeStream {
    fn replying(reply: &[u8]) -> FakeStream {
        FakeStream {
            reply: Cursor::new(reply.to_vec()),
            written: Vec::new(),
            fail_write: false,
            fail_flush: false,
        }
    }
}

impl Read for FakeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reply.read(buf)
    }
}

impl Write for FakeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            return Err(io::Error::other("reset"));
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            return Err(io::Error::other("reset"));
        }
        Ok(())
    }
}

/// An HTTP/1.1 response with a Content-Length.
fn response(status: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

const MAKE: &str = "/hostkeeper.fixture.v1.Widgets/Make";

/// A call of Widgets.Make with `request` on a stream that replies `reply`.
fn call(
    stream: &mut FakeStream,
    request: &DynamicMessage,
    metadata: &MetadataMap,
) -> Result<Bytes, tonic::Status> {
    let make = method(&pool(), MAKE).expect("the method");
    exchange(
        &mut *stream,
        "coordinator.example",
        &make,
        MAKE,
        Bytes::from(request.encode_to_vec()),
        metadata,
    )
}

/// Catches the gateway request on another method or path, without the bearer, with a body that
/// is not the request's canonical JSON, or a response not read back into protobuf; and an
/// unknown field of a newer server refused.
#[test]
fn gateway_maps_the_rpc_to_post_package_service_method() {
    let mut metadata = MetadataMap::new();
    metadata.insert("authorization", MetadataValue::from_static("Bearer t0k"));
    let mut stream = FakeStream::replying(&response(
        "200 OK",
        r#"{"name": "x", "colour": "COLOUR_RED", "fieldOfANewerServer": 1}"#,
    ));
    let reply = call(
        &mut stream,
        &from_json("Widget", r#"{"name": "w", "i64": "5"}"#),
        &metadata,
    )
    .expect("a reply");
    assert_eq!(
        String::from_utf8(stream.written).expect("UTF-8"),
        "POST /hostkeeper.fixture.v1.Widgets/Make HTTP/1.1\r\nHost: coordinator.example\r\n\
         Content-Type: application/json\r\nAccept: application/json\r\nContent-Length: 22\r\n\
         Connection: close\r\nAuthorization: Bearer t0k\r\n\r\n{\"name\":\"w\",\"i64\":\"5\"}"
    );
    let got = DynamicMessage::decode(crate::testutil::message("Widget"), reply).expect("decode");
    assert_eq!(
        got,
        from_json("Widget", r#"{"name": "x", "colour": "COLOUR_RED"}"#)
    );

    let mut anonymous = FakeStream::replying(&response("200 OK", "{}"));
    call(
        &mut anonymous,
        &from_json("Widget", "{}"),
        &MetadataMap::new(),
    )
    .expect("a reply");
    let written = String::from_utf8(anonymous.written).expect("UTF-8");
    assert!(!written.contains("Authorization"), "{written}");
    assert!(
        written.ends_with("Connection: close\r\n\r\n{}"),
        "{written}"
    );
}

/// Catches a gRPC path that is not a method of the descriptors called anyway.
#[test]
fn only_a_method_of_the_descriptors_is_called() {
    let pool = pool();
    assert_eq!(
        method(&pool, MAKE).expect("make").full_name(),
        "hostkeeper.fixture.v1.Widgets.Make"
    );
    for path in [
        "hostkeeper.fixture.v1.Widgets/Make",
        "/hostkeeper.fixture.v1.Widgets",
        "/hostkeeper.fixture.v1.Nothing/Make",
        "/hostkeeper.fixture.v1.Widgets/Nothing",
    ] {
        let status = method(&pool, path).expect_err(path);
        assert_eq!(status.code(), Code::Internal);
        assert_eq!(
            status.message(),
            format!("the descriptors have no method at {path:?}")
        );
    }
}

/// Catches an error body read as another code or reason, for every code, and a body that is not
/// the gateway's read as a refusal.
#[test]
fn gateway_error_body_maps_every_grpc_code() {
    for (code, name) in codes::ALL.iter().skip(1) {
        let status = error_status(
            400,
            format!(r#"{{"code": "{name}", "reason": "not_halted"}}"#).as_bytes(),
        );
        assert_eq!(
            (status.code(), status.message()),
            (*code, "not_halted"),
            "{name}"
        );
    }
    for body in [
        &b"<html>"[..],
        br#"{"code": "OK", "reason": "x"}"#,
        br#"{"code": "invalid_argument", "reason": "x"}"#,
        br#"{"code": "INTERNAL"}"#,
        br#"{"code": 3, "reason": "x"}"#,
        br#"{"reason": "x"}"#,
        br#"{"code": "INTERNAL", "reason": 1}"#,
    ] {
        let status = error_status(502, body);
        assert_eq!(status.code(), Code::Unknown, "{body:?}");
        assert_eq!(
            status.message(),
            "malformed reply: HTTP 502 without a gateway error body"
        );
    }
    let mut stream = FakeStream::replying(&response(
        "409 Conflict",
        r#"{"code": "FAILED_PRECONDITION", "reason": "below_min_version"}"#,
    ));
    let status =
        call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new()).expect_err("refused");
    assert_eq!(
        (status.code(), status.message()),
        (Code::FailedPrecondition, "below_min_version")
    );
}

/// Catches a request that does not decode as its method's request sent anyway, a failed write
/// or flush reported as anything but UNAVAILABLE, and a 200 whose body is not the response
/// accepted.
#[test]
fn a_bad_request_a_lost_connection_and_a_bad_body() {
    let make = method(&pool(), MAKE).expect("make");
    let mut stream = FakeStream::replying(b"");
    let status = exchange(
        &mut stream,
        "h",
        &make,
        MAKE,
        Bytes::from_static(b"\xff\xff"),
        &MetadataMap::new(),
    )
    .expect_err("does not decode");
    assert_eq!(status.code(), Code::Internal);
    assert!(stream.written.is_empty());

    let request = from_json("Widget", "{}");
    for (fail_write, fail_flush) in [(true, false), (false, true)] {
        let mut stream = FakeStream::replying(&response("200 OK", "{}"));
        stream.fail_write = fail_write;
        stream.fail_flush = fail_flush;
        let status = call(&mut stream, &request, &MetadataMap::new()).expect_err("lost");
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(status.message(), "the gateway connection failed: reset");
    }
    for body in ["not json", r#"{"name": 5}"#] {
        let mut stream = FakeStream::replying(&response("200 OK", body));
        let status = call(&mut stream, &request, &MetadataMap::new()).expect_err(body);
        assert_eq!(status.code(), Code::Internal, "{body}");
        assert!(
            status.message().starts_with("malformed reply: "),
            "{status:?}"
        );
    }
    let mut truncated = FakeStream::replying(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{}");
    let status = call(&mut truncated, &request, &MetadataMap::new()).expect_err("truncated");
    assert_eq!(status.code(), Code::Unavailable);
    let mut garbled = FakeStream::replying(b"SMTP ready\r\n\r\n");
    let status = call(&mut garbled, &request, &MetadataMap::new()).expect_err("not HTTP");
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(
        status.message(),
        "malformed reply: not an HTTP/1.1 status line: \"SMTP ready\""
    );
}

/// The status and body of a response, or why it was refused.
fn read(bytes: &[u8]) -> Result<(u16, Vec<u8>), String> {
    read_response(BufReader::new(bytes)).map_err(|e| match e {
        ReadError::Io(e) => format!("io: {e}"),
        ReadError::Malformed(why) => why,
    })
}

/// Catches a response framed by Content-Length, by chunks or by the end of the connection read
/// wrongly, and a malformed or oversized one accepted.
#[test]
fn responses_are_framed_and_bounded() {
    assert_eq!(
        read(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}"),
        Ok((200, b"{}".to_vec()))
    );
    assert_eq!(
        read(b"HTTP/1.0 404 Not Found\n\nbody to the end"),
        Ok((404, b"body to the end".to_vec()))
    );
    assert_eq!(
        read(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n4;x=y\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\nTrailer: t\r\n\r\n"),
        Ok((200, b"{\"a\":1}".to_vec()))
    );
    assert_eq!(
        read(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"),
        Ok((204, Vec::new()))
    );
    let over = MAX_RESPONSE_BYTES + 1;
    let cases: Vec<(Vec<u8>, String)> = vec![
        (
            b"HTTP/2 200\r\n\r\n".to_vec(),
            "not an HTTP/1.1 status line: \"HTTP/2 200\"".to_owned(),
        ),
        (
            b"HTTP/1.1 2x0 OK\r\n\r\n".to_vec(),
            "not an HTTP/1.1 status line: \"HTTP/1.1 2x0 OK\"".to_owned(),
        ),
        (
            b"HTTP/1.1 20".to_vec(),
            "the response ended or grew too long inside a line".to_owned(),
        ),
        (
            b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n".to_vec(),
            "not a header: \"no colon\"".to_owned(),
        ),
        (
            b"HTTP/1.1 200 OK\r\n\xff: x\r\n\r\n".to_vec(),
            "a line of the head is not UTF-8".to_owned(),
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: two\r\n\r\n".to_vec(),
            "Content-Length \"two\"".to_owned(),
        ),
        (
            format!("HTTP/1.1 200 OK\r\nContent-Length: {over}\r\n\r\n").into_bytes(),
            "the body is over the limit".to_owned(),
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n".to_vec(),
            "Transfer-Encoding \"gzip\"".to_owned(),
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n".to_vec(),
            "a chunk size \"zz\"".to_owned(),
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nabc\r\n0\r\n\r\n".to_vec(),
            "a chunk is longer than its size".to_owned(),
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffff\r\n".to_vec(),
            "the body is over the limit".to_owned(),
        ),
    ];
    for (bytes, why) in cases {
        assert_eq!(read(&bytes), Err(why.clone()), "{why}");
    }
    let mut long_body = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    long_body.resize(long_body.len() + over, b'x');
    assert_eq!(
        read(&long_body),
        Err("the body is over the limit".to_owned())
    );
    let mut long_line = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
    long_line.resize(long_line.len() + MAX_RESPONSE_BYTES, b'x');
    assert_eq!(
        read(&long_line),
        Err("the response ended or grew too long inside a line".to_owned())
    );
    let truncated = read(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab");
    assert!(
        truncated.as_ref().is_err_and(|e| e.starts_with("io: ")),
        "{truncated:?}"
    );
}

/// A loopback TLS server that answers one connection with `reply` and sends what it read.
fn serve_once(pki: &Pki, reply: Vec<u8>) -> (u16, mpsc::Receiver<Vec<u8>>) {
    let certs: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(pki.server_cert.as_bytes())
            .collect::<Result<_, _>>()
            .expect("the server certificate");
    let key = PrivateKeyDer::from_pem_slice(pki.server_key.as_bytes()).expect("the server key");
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .expect("the server config");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let (sent, received) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((tcp, _)) = listener.accept() else {
            return;
        };
        let Ok(connection) = rustls::ServerConnection::new(config) else {
            return;
        };
        let mut tls = rustls::StreamOwned::new(connection, tcp);
        let mut request = Vec::new();
        let mut reader = BufReader::new(&mut tls);
        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            request.extend_from_slice(line.as_bytes());
            if let Some(n) = line.strip_prefix("Content-Length: ") {
                length = n.trim().parse().unwrap_or(0);
            }
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        request.extend_from_slice(&body);
        drop(reader);
        let _ = sent.send(request);
        let _ = tls.write_all(&reply);
        tls.conn.send_close_notify();
        let _ = tls.flush();
    });
    (port, received)
}

/// Catches the gateway not reaching a server its CA signed, writing other than the request, or
/// misreading the reply; and a server of another CA trusted.
#[test]
fn the_gateway_calls_over_tls() {
    let pki = pki();
    let dir = scratch("gateway-tls");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let (port, received) = serve_once(&pki, response("200 OK", r#"{"hosts": [{"nodeId": "h1"}]}"#));
    let transport = Network
        .connect(&loopback_target(port, TransportKind::Gateway, &ca, None))
        .expect("a transport");
    let mut metadata = MetadataMap::new();
    metadata.insert("authorization", MetadataValue::from_static("Bearer t"));
    // A real path: the gateway reads the real descriptors.
    let reply = transport
        .call(
            "/hostkeeper.operator.v1.Hosts/List",
            Bytes::new(),
            &metadata,
        )
        .expect("a reply");
    let hosts = hostkeeper_proto::hostkeeper::operator::v1::ListHostsResponse::decode(reply)
        .expect("a ListHostsResponse");
    assert_eq!(hosts.hosts.len(), 1);
    assert_eq!(hosts.hosts[0].node_id, "h1");
    let request = String::from_utf8(received.recv().expect("the request")).expect("UTF-8");
    assert!(
        request.starts_with("POST /hostkeeper.operator.v1.Hosts/List HTTP/1.1\r\nHost: 127.0.0.1:"),
        "{request}"
    );
    assert!(
        request.ends_with("Authorization: Bearer t\r\n\r\n{}"),
        "{request}"
    );

    let other = crate::testutil::pki();
    let (port, _received) = serve_once(&other, response("200 OK", "{}"));
    let transport = Network
        .connect(&loopback_target(port, TransportKind::Gateway, &ca, None))
        .expect("a transport");
    let status = transport
        .call(
            "/hostkeeper.operator.v1.Hosts/List",
            Bytes::new(),
            &MetadataMap::new(),
        )
        .expect_err("another CA");
    assert_eq!(status.code(), Code::Unavailable, "{status:?}");

    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        listener.local_addr().expect("its address").port()
    };
    let transport = Network
        .connect(&loopback_target(closed, TransportKind::Gateway, &ca, None))
        .expect("a transport");
    let status = transport
        .call(
            "/hostkeeper.operator.v1.Hosts/List",
            Bytes::new(),
            &MetadataMap::new(),
        )
        .expect_err("closed");
    assert_eq!(status.code(), Code::Unavailable);
    assert!(
        status.message().starts_with("cannot connect to 127.0.0.1:"),
        "{status:?}"
    );
    let status = transport
        .call(
            "/hostkeeper.operator.v1.Nothing/List",
            Bytes::new(),
            &MetadataMap::new(),
        )
        .expect_err("no method");
    assert_eq!(status.code(), Code::Internal);
}
