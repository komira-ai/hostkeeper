//! The response parser's strictness: the status line, ambiguous framing, informational
//! responses, the exact byte limit of each framing, and the single JSON document of a 200.

use bytes::Bytes;
use prost_reflect::DynamicMessage;
use tonic::Code;
use tonic::metadata::MetadataMap;

use super::{FakeStream, call, read, response};
use crate::testutil::from_json;
use crate::transport::gateway::MAX_RESPONSE_BYTES;

/// Builds a response of the given total size.
type Build = fn(usize) -> Vec<u8>;

/// The error `read` gives for a refused response.
fn refused(bytes: &[u8]) -> String {
    read(bytes).expect_err("refused")
}

/// A response of exactly `total` bytes, its body framed by `Content-Length`.
fn by_length(total: usize) -> Vec<u8> {
    let (head, tail) = ("HTTP/1.1 200 OK\r\nContent-Length: ", "\r\n\r\n");
    let fixed = head.len() + tail.len();
    let digits = (1..=10)
        .find(|d| (total - fixed - d).to_string().len() == *d)
        .expect("a length of some number of digits");
    let length = total - fixed - digits;
    let mut out = format!("{head}{length}{tail}").into_bytes();
    out.resize(total, b'x');
    out
}

/// A response of exactly `total` bytes, its body one chunk.
fn by_chunks(total: usize) -> Vec<u8> {
    let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    let tail = "\r\n0\r\n\r\n";
    let fixed = head.len() + 2 + tail.len();
    let digits = (1..=8)
        .find(|d| format!("{:x}", total - fixed - d).len() == *d)
        .expect("a size of some number of digits");
    let size = total - fixed - digits;
    let mut out = format!("{head}{size:x}\r\n").into_bytes();
    out.resize(out.len() + size, b'x');
    out.extend_from_slice(tail.as_bytes());
    out
}

/// A response of exactly `total` bytes, its body to the end of the connection.
fn to_the_end(total: usize) -> Vec<u8> {
    let mut out = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    out.resize(total, b'x');
    out
}

/// Catches a status token longer or shorter than three digits, one with a sign or a letter, one
/// outside 100 to 599, or a version other than 1.0 and 1.1 accepted; and a bare code or a code
/// with a reason refused.
#[test]
fn the_status_line_must_be_exact() {
    for line in [
        "HTTP/1.1 2000 OK",
        "HTTP/1.1 +20 OK",
        "HTTP/1.1 20x OK",
        "HTTP/1.1  200 OK",
        "HTTP/1.1 200OK",
        "HTTP/1.1 099 Low",
        "HTTP/1.1 600 High",
        "HTTP/1.1 ",
        "HTTP/1.1 \u{e9}00 OK",
        "HTTP/1.2 200 OK",
        "http/1.1 200 OK",
    ] {
        let bytes = format!("{line}\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(
            refused(bytes.as_bytes()),
            format!("not an HTTP/1.1 status line: {line:?}"),
            "{line}"
        );
    }
    for (line, status) in [
        ("HTTP/1.1 200", 200),
        ("HTTP/1.1 200 ", 200),
        ("HTTP/1.0 404 Not Found", 404),
        ("HTTP/1.1 599 Odd", 599),
    ] {
        let bytes = format!("{line}\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(read(bytes.as_bytes()), Ok((status, Vec::new())), "{line}");
    }
}

/// Catches two Content-Length headers that disagree resolved to one of them, a repeated equal
/// value refused, a Content-Length that is signed or empty accepted, and Transfer-Encoding
/// together with Content-Length, in either order, accepted.
#[test]
fn ambiguous_framing_is_refused() {
    assert_eq!(
        refused(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\ncontent-length: 3\r\n\r\n{}x"),
        "Content-Length given twice, as 2 and 3"
    );
    assert_eq!(
        read(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\ncontent-length: 2\r\n\r\n{}"),
        Ok((200, b"{}".to_vec()))
    );
    assert_eq!(
        refused(b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\n{}"),
        "Content-Length \"+2\""
    );
    assert_eq!(
        refused(b"HTTP/1.1 200 OK\r\nContent-Length:\r\n\r\n"),
        "Content-Length \"\""
    );
    assert_eq!(
        refused(b"HTTP/1.1 200 OK\r\nContent-Length: 1, 1\r\n\r\nx"),
        "Content-Length \"1, 1\""
    );
    for head in [
        "Content-Length: 2\r\nTransfer-Encoding: chunked",
        "Transfer-Encoding: chunked\r\nContent-Length: 2",
    ] {
        let bytes = format!("HTTP/1.1 200 OK\r\n{head}\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n");
        assert_eq!(
            refused(bytes.as_bytes()),
            "both Transfer-Encoding and Content-Length",
            "{head}"
        );
    }
}

/// Catches an informational response taken for the final one, so that a 103 hides the reply
/// that follows; an upgrade the client never asked for followed; and endless informational
/// responses read past the byte limit.
#[test]
fn informational_responses_are_skipped_and_an_upgrade_is_refused() {
    assert_eq!(
        read(
            b"HTTP/1.1 103 Early Hints\r\nLink: </a>; rel=preload\r\n\r\n\
              HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"
        ),
        Ok((200, b"{}".to_vec()))
    );
    assert_eq!(
        read(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 102 Processing\r\n\r\n\
              HTTP/1.1 404 Not Found\r\nContent-Length: 1\r\n\r\nx"
        ),
        Ok((404, b"x".to_vec()))
    );
    assert_eq!(
        refused(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n"),
        "101 Switching Protocols, which was not asked for"
    );
    assert_eq!(
        refused(b"HTTP/1.1 103 Hints\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: two\r\n\r\n"),
        "Content-Length \"two\""
    );
    let endless = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(MAX_RESPONSE_BYTES / 25 + 2);
    assert_eq!(
        refused(&endless),
        "the response ended or grew too long inside a line"
    );

    let mut stream = FakeStream::replying(
        b"HTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}",
    );
    call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new())
        .expect("the reply after the hints");
}

/// Catches a limit counted differently from head plus body as received, in any framing: a
/// response of exactly the limit is read, one byte more is refused.
#[test]
fn the_limit_is_exact_for_every_framing() {
    let cases: [(&str, Build, &str); 3] = [
        ("Content-Length", by_length, "the body is over the limit"),
        (
            "chunked",
            by_chunks,
            "the response ended or grew too long inside a line",
        ),
        ("to the end", to_the_end, "the body is over the limit"),
    ];
    for (name, build, over) in cases {
        let at = build(MAX_RESPONSE_BYTES);
        assert_eq!(at.len(), MAX_RESPONSE_BYTES, "{name}");
        let (status, body) = read(&at).unwrap_or_else(|e| panic!("{name} at the limit: {e}"));
        assert_eq!(status, 200, "{name}");
        assert!(body.len() < MAX_RESPONSE_BYTES && body.iter().all(|b| *b == b'x'));
        let past = build(MAX_RESPONSE_BYTES + 1);
        assert_eq!(past.len(), MAX_RESPONSE_BYTES + 1, "{name}");
        assert_eq!(read(&past), Err(over.to_owned()), "{name} past the limit");
    }
}

/// Catches the gateway limit mistaken for the gRPC message limit: a 3 MiB `bytes` value is
/// within tonic's 4 MiB decoded limit but is 4 MiB of base64 in JSON, so that its response is
/// over the gateway's limit and refused, while a smaller one is read back whole.
#[test]
fn json_expansion_can_take_a_reply_over_the_gateway_limit() {
    let blob = |raw: usize| {
        let base64 = "A".repeat(raw / 3 * 4);
        response("200 OK", &format!(r#"{{"blob": "{base64}"}}"#))
    };
    let mut stream = FakeStream::replying(&blob(3 * 1024 * 1024));
    let status = call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new())
        .expect_err("over the gateway limit");
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(
        status.message(),
        "malformed reply: the body is over the limit"
    );

    let mut stream = FakeStream::replying(&blob(3 * 1024 * 1024 - 3 * 1024));
    let reply = call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new())
        .expect("within the gateway limit");
    let got = DynamicMessage::decode(crate::testutil::message("Widget"), reply).expect("decode");
    let bytes = got.get_field_by_name("blob").expect("blob");
    assert_eq!(
        bytes.as_bytes().map(Bytes::len),
        Some(3 * 1024 * 1024 - 3 * 1024)
    );
}

/// Catches a 200 whose body has content after the first JSON document accepted as a reply
/// (the first document returned and the rest ignored), and trailing white space refused.
#[test]
fn a_reply_is_one_json_document() {
    for body in ["{}junk", "{}{}", "{} {\"name\": \"x\"}", "{}]"] {
        let mut stream = FakeStream::replying(&response("200 OK", body));
        let status =
            call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new()).expect_err(body);
        assert_eq!(status.code(), Code::Internal, "{body}");
        // The wording after the prefix is serde_json's.
        assert!(
            status
                .message()
                .starts_with("malformed reply: trailing characters at line 1 column "),
            "{body}: {status:?}"
        );
    }
    for body in ["{}  ", "{}\r\n", " \t{\"name\": \"x\"}\n\n"] {
        let mut stream = FakeStream::replying(&response("200 OK", body));
        call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new()).expect(body);
    }
}
