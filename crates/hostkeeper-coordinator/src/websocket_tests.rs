//! The WebSocket handshake and frames: what is taken, what is refused, and the bytes sent. The
//! transports are byte slices and vectors, so no test waits on anything.

use hyper::HeaderMap;
use hyper::header::{HeaderName, HeaderValue};
use tokio::io::AsyncReadExt;

use super::{
    BadUpgrade, CLOSE_NORMAL, Message, Outbox, ReadError, Reader, Writer, accept_key, handshake,
};

/// The mask the tests' frames use.
const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

/// One frame as a browser sends it: `first` is the first byte (FIN, reserved bits, opcode), the
/// payload is masked with [`MASK`].
pub(crate) fn raw_frame(first: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![first];
    match payload.len() {
        short @ 0..=125 => frame.push(0x80 | u8::try_from(short).expect("short")),
        medium @ 126..=65535 => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&u16::try_from(medium).expect("medium").to_be_bytes());
        }
        long => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&u64::try_from(long).expect("long").to_be_bytes());
        }
    }
    frame.extend_from_slice(&MASK);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(at, byte)| byte ^ MASK[at % 4]),
    );
    frame
}

/// A final frame of `opcode` as a browser sends it.
pub(crate) fn client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    raw_frame(0x80 | opcode, payload)
}

async fn read_one(bytes: &[u8], limit: usize) -> Result<Message, ReadError> {
    Reader::new(bytes, limit).next().await
}

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    map
}

const GOOD: [(&str, &str); 4] = [
    ("upgrade", "websocket"),
    ("connection", "keep-alive, Upgrade"),
    ("sec-websocket-version", "13"),
    ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
];

fn without(missing: &str) -> HeaderMap {
    let kept: Vec<(&'static str, &'static str)> = GOOD
        .iter()
        .copied()
        .filter(|(name, _)| *name != missing)
        .collect();
    headers(&kept)
}

/// Proves: the accept key is the one RFC 6455 gives for its sample key.
///
/// Catches: a wrong GUID, hash or encoding, which no browser would accept.
#[test]
fn the_accept_key_is_the_one_of_the_rfc() {
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

/// Proves: a handshake with `Upgrade: websocket`, a `Connection` that has the `upgrade` token
/// among others in any case, version 13 and a key of 16 bytes answers with the accept key; each
/// header missing or wrong is refused with its own reason, the `Upgrade` and `Connection` checks
/// reading the comma-separated tokens and a key that is not 16 bytes of base64 being refused.
///
/// Catches: a handshake that opens on a request that is no upgrade, and one that refuses a
/// browser's `Connection: keep-alive, Upgrade`.
#[test]
fn a_handshake_is_accepted_only_when_every_header_is_right() {
    assert_eq!(
        handshake(&headers(&GOOD)),
        Ok("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".to_owned())
    );
    assert_eq!(handshake(&without("upgrade")), Err(BadUpgrade::Upgrade));
    assert_eq!(
        handshake(&without("connection")),
        Err(BadUpgrade::Connection)
    );
    assert_eq!(
        handshake(&without("sec-websocket-version")),
        Err(BadUpgrade::Version)
    );
    assert_eq!(
        handshake(&without("sec-websocket-key")),
        Err(BadUpgrade::Key)
    );
    let mut other = GOOD;
    other[0].1 = "h2c";
    assert_eq!(handshake(&headers(&other)), Err(BadUpgrade::Upgrade));
    let mut other = GOOD;
    other[1].1 = "keep-alive";
    assert_eq!(handshake(&headers(&other)), Err(BadUpgrade::Connection));
    let mut other = GOOD;
    other[2].1 = "8";
    assert_eq!(handshake(&headers(&other)), Err(BadUpgrade::Version));
    for key in ["not base64!", "dGhlIHNhbXBsZQ==", ""] {
        let mut other = headers(&GOOD);
        other.insert(
            "sec-websocket-key",
            HeaderValue::from_str(key).expect("a value"),
        );
        assert_eq!(handshake(&other), Err(BadUpgrade::Key), "{key:?}");
    }
    let mut binary = headers(&GOOD);
    binary.insert(
        "sec-websocket-key",
        HeaderValue::from_bytes(&[0xff, 0xfe]).expect("a value"),
    );
    assert_eq!(handshake(&binary), Err(BadUpgrade::Key));
    assert_eq!(BadUpgrade::Key.to_string(), "no valid Sec-WebSocket-Key");
}

/// Proves: masked text and binary messages of the three length forms (7 bits, 16 bits, 64 bits)
/// are read whole and unmasked, an empty one too, and a message that is exactly the limit is
/// taken.
///
/// Catches: a length form misread, a mask applied from the wrong offset, and a limit that is off
/// by one.
#[tokio::test]
async fn masked_messages_of_every_length_form_are_read() {
    assert_eq!(
        read_one(&client_frame(1, b"hello"), 100).await,
        Ok(Message::Text("hello".to_owned()))
    );
    assert_eq!(
        read_one(&client_frame(2, b""), 100).await,
        Ok(Message::Binary(Vec::new()))
    );
    let medium: Vec<u8> = (0..300_u32)
        .map(|n| u8::try_from(n % 251).expect("a byte"))
        .collect();
    assert_eq!(
        read_one(&client_frame(2, &medium), 300).await,
        Ok(Message::Binary(medium))
    );
    let long = vec![7_u8; 70_000];
    let mut wire = client_frame(2, &long);
    assert_eq!(wire[1], 0x80 | 127);
    assert_eq!(
        read_one(&wire, 70_000).await,
        Ok(Message::Binary(long.clone()))
    );
    wire.truncate(wire.len() - 1);
    assert_eq!(read_one(&wire, 70_000).await, Err(ReadError::Ended));
}

/// Proves: a message one byte over the limit is refused as too big from its header alone (no byte
/// of its payload is needed), a 64-bit length that no machine could hold is too big, and a
/// control frame is not held to the data limit.
///
/// Catches: a reader that allocates what a header claims.
#[tokio::test]
async fn a_message_over_the_limit_is_refused_before_its_payload_is_read() {
    let mut header = client_frame(2, &[0_u8; 11]);
    header.truncate(2 + 4);
    assert_eq!(read_one(&header, 10).await, Err(ReadError::TooBig));
    let mut huge = vec![0x82, 0x80 | 127];
    huge.extend_from_slice(&u64::MAX.to_be_bytes());
    assert_eq!(read_one(&huge, 10).await, Err(ReadError::TooBig));
    assert_eq!(
        read_one(&client_frame(9, &[1; 100]), 10).await,
        Ok(Message::Ping(vec![1; 100]))
    );
}

/// Proves: a ping gives its payload, a pong is skipped and the message after it is the one read, a
/// close frame is a close, and a stream that ends between frames, in a header or in a payload is
/// an end.
///
/// Catches: a pong that ends the terminal, and a close that is not seen.
#[tokio::test]
async fn control_frames_are_told_or_skipped_and_an_end_is_an_end() {
    let mut wire = client_frame(0xa, b"x");
    wire.extend(client_frame(1, b"after"));
    assert_eq!(
        read_one(&wire, 10).await,
        Ok(Message::Text("after".to_owned()))
    );
    assert_eq!(
        read_one(&client_frame(9, b"abc"), 10).await,
        Ok(Message::Ping(b"abc".to_vec()))
    );
    assert_eq!(
        read_one(&client_frame(8, &[3, 232]), 10).await,
        Ok(Message::Close)
    );
    assert_eq!(read_one(&[], 10).await, Err(ReadError::Ended));
    assert_eq!(read_one(&[0x81], 10).await, Err(ReadError::Ended));
    assert_eq!(read_one(&[0x81, 0x85], 10).await, Err(ReadError::Ended));
    assert_eq!(
        read_one(&[0x81, 0x85, 1, 2, 3, 4], 10).await,
        Err(ReadError::Ended)
    );
}

/// Proves: each break of the protocol is refused with its reason: an unmasked frame, a reserved
/// bit, a fragment (a frame that is not final, and a continuation), a control frame that is
/// longer than 125 bytes or not final, an opcode the terminal does not take, and a text message
/// that is not UTF-8.
///
/// Catches: a browser that is taken for another's, and a text message with bytes that are no text
/// reaching the JSON reader.
#[tokio::test]
async fn a_break_of_the_protocol_is_refused() {
    let cases: [(Vec<u8>, &str); 8] = [
        (vec![0x81, 0x01, b'x'], "a frame is not masked"),
        (raw_frame(0xc1, b"x"), "a reserved bit is set"),
        (raw_frame(0x01, b"x"), "a message is fragmented"),
        (client_frame(0, b"x"), "an opcode that is not taken"),
        (client_frame(8, &[0; 126]), "a control frame is too long"),
        (raw_frame(0x09, b""), "a message is fragmented"),
        (client_frame(3, b"x"), "an opcode that is not taken"),
        (
            client_frame(1, &[0xff, 0xfe]),
            "a text message is not UTF-8",
        ),
    ];
    for (wire, reason) in cases {
        assert_eq!(
            read_one(&wire, 1000).await,
            Err(ReadError::Protocol(reason)),
            "{reason}"
        );
    }
    assert_eq!(
        ReadError::Protocol("x").to_string(),
        "websocket protocol error: x"
    );
}

async fn sent(send: impl AsyncFnOnce(&Outbox<&mut Vec<u8>>) -> std::io::Result<()>) -> Vec<u8> {
    let mut out = Vec::new();
    let outbox = Outbox::new(Writer::new(&mut out));
    send(&outbox).await.expect("sent");
    drop(outbox);
    out
}

/// Proves: the frames the coordinator sends are final, unmasked and carry their payload with the
/// shortest length form: text and binary of 5, 300 and 70,000 bytes, a pong with its payload, and
/// a close with its status, after which the transport is shut down.
///
/// Catches: a length form that a browser reads as another length, and a close frame without its
/// status.
#[tokio::test]
async fn sent_frames_have_the_shortest_length_form_and_no_mask() {
    assert_eq!(
        sent(async |outbox| outbox.text("hello").await).await,
        [&[0x81, 5][..], b"hello"].concat()
    );
    let medium = vec![9_u8; 300];
    assert_eq!(
        sent(async |outbox| outbox.binary(&medium).await).await,
        [&[0x82, 126, 1, 44][..], &medium].concat()
    );
    let long = vec![9_u8; 70_000];
    let wire = sent(async |outbox| outbox.binary(&long).await).await;
    assert_eq!(&wire[..10], &[0x82, 127, 0, 0, 0, 0, 0, 1, 17, 112]);
    assert_eq!(wire.len(), 10 + 70_000);
    assert_eq!(
        sent(async |outbox| outbox.pong(b"p").await).await,
        vec![0x8a, 1, b'p']
    );
    assert_eq!(
        sent(async |outbox| outbox.close(CLOSE_NORMAL).await).await,
        vec![0x88, 2, 3, 232]
    );
}

/// Proves: a close is followed by the end of the stream: the peer reads the frame and then end of
/// file.
///
/// Catches: a close frame sent on a connection that is then left open.
#[tokio::test]
async fn a_close_ends_the_stream_after_its_frame() {
    let (ours, mut theirs) = tokio::io::duplex(1024);
    let outbox = Outbox::new(Writer::new(ours));
    let clone = outbox.clone();
    clone.close(CLOSE_NORMAL).await.expect("closed");
    let mut all = Vec::new();
    theirs.read_to_end(&mut all).await.expect("to the end");
    assert_eq!(all, vec![0x88, 2, 3, 232]);
}

/// Proves: a close frame carries its status and its reason, and a reason longer than the 123
/// bytes a close frame holds is cut at the last character's edge at or before them: a reason of
/// two-byte characters is cut to 122 bytes, not split.
///
/// Catches: a close frame longer than a control frame may be (which a browser treats as a broken
/// connection), and a reason cut inside a character, which is not UTF-8.
#[tokio::test]
async fn a_close_reason_is_cut_at_a_characters_edge() {
    let short = sent(async |outbox| outbox.close_with(4002, "bmc_sol_busy").await).await;
    assert_eq!(
        short,
        [&[0x88, 14, 0x0f, 0xa2][..], b"bmc_sol_busy"].concat()
    );
    let long = "é".repeat(100);
    let cut = sent(async |outbox| outbox.close_with(4001, &long).await).await;
    assert_eq!(cut[..4], [0x88, 124, 0x0f, 0xa1]);
    assert_eq!(cut[4..], "é".repeat(61).into_bytes()[..]);
}
