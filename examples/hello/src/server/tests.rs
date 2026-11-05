use std::io::{self, Cursor, Read};

use super::{Answer, MAX_HEAD, answer, find_end, read_head, request_line};

const VERSION: &str = "1.2.3";

fn ask(head: &str, fail_ready: bool) -> String {
    let answer = answer(
        head.as_bytes(),
        &Answer {
            version: VERSION,
            fail_ready,
        },
    );
    String::from_utf8(answer).expect("the answer is text")
}

fn reply(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Catches `/readyz` not answering 200 `ready`, or answering 503 without `--fail-ready`.
#[test]
fn readyz_is_ready() {
    let want = reply("200 OK", "ready\n");
    assert_eq!(ask("GET /readyz HTTP/1.1\r\nHost: x\r\n\r\n", false), want);
    assert_eq!(ask("GET /readyz HTTP/1.0\r\n\r\n", false), want);
    assert_eq!(ask("GET /readyz?probe=1 HTTP/1.1\r\n\r\n", false), want);
}

/// Catches `--fail-ready` still answering ready, or failing `/version` as well.
#[test]
fn readyz_is_503_when_asked_to_fail() {
    assert_eq!(
        ask("GET /readyz HTTP/1.1\r\n\r\n", true),
        reply("503 Service Unavailable", "not ready\n")
    );
    assert_eq!(
        ask("GET /version HTTP/1.1\r\n\r\n", true),
        reply("200 OK", "1.2.3\n")
    );
}

/// Catches `/version` not reporting the version it was given.
#[test]
fn version_is_the_version() {
    assert_eq!(
        ask("GET /version HTTP/1.1\r\n\r\n", false),
        reply("200 OK", "1.2.3\n")
    );
}

/// Catches an unknown path answered with anything but 404.
#[test]
fn another_path_is_404() {
    let want = reply("404 Not Found", "not found\n");
    assert_eq!(ask("GET / HTTP/1.1\r\n\r\n", false), want);
    assert_eq!(ask("GET /readyz/ HTTP/1.1\r\n\r\n", false), want);
    assert_eq!(ask("GET /healthz HTTP/1.1\r\n\r\n", false), want);
}

/// Catches another method answered like a GET.
#[test]
fn another_method_is_405() {
    let want = reply("405 Method Not Allowed", "method not allowed\n");
    assert_eq!(ask("POST /readyz HTTP/1.1\r\n\r\n", false), want);
    assert_eq!(ask("HEAD /version HTTP/1.1\r\n\r\n", false), want);
    assert_eq!(ask("DELETE /nothing HTTP/1.1\r\n\r\n", false), want);
}

/// Catches a request head that is not `<METHOD> /<target> HTTP/1.x` being answered as a request.
#[test]
fn an_unparsable_head_is_400() {
    let want = reply("400 Bad Request", "bad request\n");
    for head in [
        "",
        "\r\n\r\n",
        "GET\r\n\r\n",
        "GET /readyz\r\n\r\n",
        "GET /readyz HTTP/2\r\n\r\n",
        "GET /readyz HTTP/1.1 extra\r\n\r\n",
        "get /readyz HTTP/1.1\r\n\r\n",
        " /readyz HTTP/1.1\r\n\r\n",
        "GET readyz HTTP/1.1\r\n\r\n",
        "GET  /readyz HTTP/1.1\r\n\r\n",
    ] {
        assert_eq!(ask(head, false), want, "{head:?}");
    }
    let not_text = [b'G', b'E', b'T', b' ', 0xff, b'\r', b'\n', b'\r', b'\n'];
    assert_eq!(
        answer(
            &not_text,
            &Answer {
                version: VERSION,
                fail_ready: false
            }
        ),
        want.into_bytes()
    );
}

/// Catches a head over 8 KiB being answered, and one of exactly 8 KiB being refused.
#[test]
fn a_head_over_eight_kib_is_400() {
    let line = "GET /readyz HTTP/1.1\r\n";
    let base = MAX_HEAD - line.len() - "X: ".len();
    let head = |n: usize| format!("{line}X: {}\r\n\r\n", "a".repeat(n));
    let exact = head(base);
    assert_eq!(find_end(exact.as_bytes()), Some(MAX_HEAD));
    assert_eq!(ask(&exact, false), reply("200 OK", "ready\n"));
    let over = head(base + 1);
    assert_eq!(find_end(over.as_bytes()), Some(MAX_HEAD + 1));
    assert_eq!(ask(&over, false), reply("400 Bad Request", "bad request\n"));
    let no_end = "a".repeat(MAX_HEAD + 1);
    assert_eq!(
        ask(&no_end, false),
        reply("400 Bad Request", "bad request\n")
    );
}

/// Catches a request line ended by a bare line feed, or followed by a body, being misread.
#[test]
fn the_request_line_is_the_first_line_only() {
    assert_eq!(
        request_line(b"GET /version HTTP/1.1\nHost: x"),
        Some(("GET", "/version"))
    );
    assert_eq!(
        request_line(b"GET /version HTTP/1.1\r\nHost: x"),
        Some(("GET", "/version"))
    );
    assert_eq!(request_line(b""), None);
}

/// Catches a head read past its blank line, or cut short when it arrives in pieces.
#[test]
fn a_head_is_read_to_its_blank_line() {
    struct Pieces(std::vec::IntoIter<&'static [u8]>);
    impl Read for Pieces {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let piece = self.0.next().unwrap_or_default();
            buf[..piece.len()].copy_from_slice(piece);
            Ok(piece.len())
        }
    }
    let mut pieces = Pieces(
        vec![
            b"GET /rea".as_slice(),
            b"dyz HTTP/1.1\r",
            b"\n\r",
            b"\n",
            b"never read",
        ]
        .into_iter(),
    );
    assert_eq!(
        read_head(&mut pieces),
        b"GET /readyz HTTP/1.1\r\n\r\n".to_vec()
    );
    assert_eq!(pieces.0.len(), 1);
}

/// Catches a client that stops, or fails, before its blank line hanging the read or losing what
/// it sent.
#[test]
fn a_head_that_stops_early_is_what_arrived() {
    let mut cut = Cursor::new(b"GET /readyz HT".to_vec());
    assert_eq!(read_head(&mut cut), b"GET /readyz HT".to_vec());
    struct Fails;
    impl Read for Fails {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("reset"))
        }
    }
    assert!(read_head(&mut Fails).is_empty());
}

/// Catches a stream with no blank line being read without bound.
#[test]
fn a_head_without_a_blank_line_is_read_only_past_the_limit() {
    let mut endless = Cursor::new(vec![b'a'; MAX_HEAD * 4]);
    let head = read_head(&mut endless);
    assert_eq!(
        (head.len() > MAX_HEAD, head.len() <= MAX_HEAD + 1024),
        (true, true),
        "{}",
        head.len()
    );
}
