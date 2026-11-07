//! The tests of the NanoHUB client: the flags it is given, its key file, and its calls against
//! an in-process server on `127.0.0.1`, port 0, that answers one call with a canned answer and
//! hands back the request it read.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::thread::JoinHandle;

use super::{ApiUrl, Enqueue, EnrollmentId, NanoHubHttp, Queued, read_key};
use crate::mdm_signing_test::Error;
use crate::release::fakes::scratch;

/// A server that reads one request and writes `answer`; the join gives the request's bytes.
fn serve(answer: &'static [u8]) -> (ApiUrl, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("address");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];
        // The head, then as many body bytes as its Content-Length says.
        loop {
            let n = stream.read(&mut buf).expect("read");
            request.extend_from_slice(&buf[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..end]).into_owned();
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        stream.write_all(answer).expect("answer");
        request
    });
    (format!("http://{addr}").parse().expect("url"), handle)
}

fn id() -> EnrollmentId {
    "0000FE00-ABC".parse().expect("id")
}

fn call(answer: &'static [u8]) -> (Result<Queued, Error>, Vec<u8>) {
    let (url, server) = serve(answer);
    let client = NanoHubHttp::new(&url, "k3y".to_owned());
    let got = client.enqueue(&id(), b"<plist/>");
    (got, server.join().expect("the server"))
}

#[test]
fn the_call_is_a_put_of_the_plist_with_basic_auth_as_nanohub() {
    // Catches: another path, method, user name or content type than NanoHUB's enqueue API
    // takes, or a body other than the command.
    let (got, request) = call(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    assert_eq!(got, Ok(Queued { push_error: None }));
    let text = String::from_utf8(request).expect("utf-8");
    let (head, body) = text.split_once("\r\n\r\n").expect("a head");
    let lines: Vec<&str> = head.lines().collect();
    assert_eq!(
        lines[0],
        "PUT /api/v1/nanomdm/enqueue/0000FE00-ABC HTTP/1.1"
    );
    // base64 of `nanohub:k3y`.
    assert!(
        lines.contains(&"Authorization: Basic bmFub2h1YjprM3k="),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Content-Type: application/xml"),
        "{lines:?}"
    );
    assert!(lines.contains(&"Content-Length: 8"), "{lines:?}");
    assert!(lines.contains(&"Connection: close"), "{lines:?}");
    assert_eq!(body, "<plist/>");
}

#[test]
fn the_answers_of_nanohub_are_read_as_its_enqueue_gives_them() {
    // Catches: a push failure read as a refusal (the command is queued), a command error read
    // as queued, a 401 not naming the key, a chunked answer not joined, or an answer that is not
    // NanoHUB's read as success.
    let cases: [(&'static [u8], Result<Queued, Error>); 8] = [
        (
            b"HTTP/1.1 200 OK\r\n\r\n{\"status\":{\"0000FE00-ABC\":{\"push_result\":\"x\"}}}",
            Ok(Queued { push_error: None }),
        ),
        (
            b"HTTP/1.1 500 Internal Server Error\r\n\r\n{\"push_error\":\"no APNs\"}",
            Ok(Queued {
                push_error: Some("no APNs".to_owned()),
            }),
        ),
        (
            b"HTTP/1.1 500 Internal Server Error\r\n\r\n{\"status\":{\"0000FE00-ABC\":{\"push_error\":\"gone\"}}}",
            Ok(Queued {
                push_error: Some("gone".to_owned()),
            }),
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;x=y\r\n{\"pu\r\n11\r\nsh_error\":\"late\"}\r\n0\r\n\r\n",
            Ok(Queued {
                push_error: Some("late".to_owned()),
            }),
        ),
        (
            b"HTTP/1.1 401 Unauthorized\r\n\r\n",
            Err(Error::NanoHub(
                "401: the API key is not NanoHUB's (NANOHUB_API_KEY of the unit's environment \
                 file)"
                    .to_owned(),
            )),
        ),
        (
            b"HTTP/1.1 500 Internal Server Error\r\n\r\n{\"status\":{\"0000FE00-ABC\":{\"command_error\":\"bad plist\"}}}",
            Err(Error::NanoHub(
                "500: the command was not queued: bad plist".to_owned(),
            )),
        ),
        (
            b"HTTP/1.1 400 Bad Request\r\n\r\n{}",
            Err(Error::NanoHub(
                "400: an answer NanoHUB's enqueue does not give: {}".to_owned(),
            )),
        ),
        (
            b"HTTP/1.1 404 Not Found\r\n\r\nnot found",
            Err(Error::NanoHub(
                "404, and the answer is not JSON: not found".to_owned(),
            )),
        ),
    ];
    for (answer, want) in cases {
        assert_eq!(call(answer).0, want, "{}", String::from_utf8_lossy(answer));
    }
    // A command error at the top of the answer too.
    let (got, _) = call(b"HTTP/1.1 200 OK\r\n\r\n{\"command_error\":\"no such id\"}");
    assert_eq!(
        got,
        Err(Error::NanoHub(
            "200: the command was not queued: no such id".to_owned()
        ))
    );
}

#[test]
fn an_answer_that_is_not_http_is_refused() {
    // Catches: a reply cut short, or from another service on the port, read as NanoHUB's.
    for (answer, why) in [
        (&b"HTTP/1.1 200 OK\r\n"[..], "its head does not end"),
        (&b"SSH-2.0-OpenSSH\r\n\r\n"[..], "no status line"),
        (&b"HTTP/1.1 2x0 OK\r\n\r\n"[..], "no status line"),
        (&b"HTTP/1.1 \xff\r\n\r\n"[..], "its head is not UTF-8"),
        (
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{}"[..],
            "a chunk is malformed",
        ),
        (
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"[..],
            "a chunk is malformed",
        ),
        (
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}xx"[..],
            "a chunk is malformed",
        ),
    ] {
        let leaked: &'static [u8] = Box::leak(answer.to_vec().into_boxed_slice());
        assert_eq!(
            call(leaked).0,
            Err(Error::NanoHub(format!("the answer is not HTTP/1.1: {why}"))),
            "{}",
            String::from_utf8_lossy(answer)
        );
    }
}

#[test]
fn a_nanohub_that_is_not_listening_is_named() {
    // Catches: a refused connection reported without the call and the address.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    drop(listener);
    let url: ApiUrl = format!("http://{addr}/").parse().expect("url");
    let got = NanoHubHttp::new(&url, "k".to_owned()).enqueue(&id(), b"x");
    let Err(Error::NanoHub(why)) = got else {
        panic!("{got:?}")
    };
    assert!(
        why.starts_with(&format!(
            "PUT /api/v1/nanomdm/enqueue/0000FE00-ABC on {addr}: "
        )),
        "{why}"
    );
}

#[test]
fn the_api_url_is_loopback_http_with_a_port_and_nothing_else() {
    // Catches: the key sent to another host, over a name that may resolve elsewhere, or a URL
    // with a path the client would drop.
    for good in [
        "http://127.0.0.1:9004",
        "http://127.0.0.1:9004/",
        "http://[::1]:9004",
    ] {
        assert!(good.parse::<ApiUrl>().is_ok(), "{good}");
    }
    for bad in [
        "https://127.0.0.1:9004",
        "http://localhost:9004",
        "http://192.0.2.1:9004",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://127.0.0.1:9004/api",
        "127.0.0.1:9004",
    ] {
        assert!(bad.parse::<ApiUrl>().is_err(), "{bad}");
    }
    assert_eq!(
        "http://192.0.2.1:9004".parse::<ApiUrl>(),
        Err(
            "\"http://192.0.2.1:9004\" is not NanoHUB's loopback API URL: the address must be \
             loopback and the port not 0: the API is never served beyond the host"
                .to_owned()
        )
    );
}

#[test]
fn an_enrollment_id_is_one_to_64_letters_digits_and_dashes() {
    // Catches: an id that would change the enqueue path (a slash, a query) reaching the URL.
    assert!("A".parse::<EnrollmentId>().is_ok());
    assert!("a".repeat(64).parse::<EnrollmentId>().is_ok());
    for bad in [
        String::new(),
        "a".repeat(65),
        "a/b".to_owned(),
        "a?b".to_owned(),
        "a b".to_owned(),
    ] {
        assert!(bad.parse::<EnrollmentId>().is_err(), "{bad}");
    }
}

#[test]
fn the_key_file_is_one_line_only_its_owner_reads() {
    // Catches: a key read from a file others may read, a trailing newline sent as part of the
    // key, or a file of two lines read as one key.
    let dir = scratch("signing-key");
    let file = |name: &str, text: &str, mode: u32| {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        path
    };
    assert_eq!(read_key(&file("lf", "abc\n", 0o600)), Ok("abc".to_owned()));
    assert_eq!(
        read_key(&file("crlf", "abc\r\n", 0o400)),
        Ok("abc".to_owned())
    );
    assert_eq!(read_key(&file("bare", "abc", 0o600)), Ok("abc".to_owned()));
    let open = file("open", "abc\n", 0o640);
    assert_eq!(
        read_key(&open),
        Err(Error::Refused(format!(
            "{} is open to its group or others (mode 640): an API key file is 0600",
            open.display()
        )))
    );
    for (name, text) in [
        ("empty", ""),
        ("two", "a\nb\n"),
        ("space", "a b"),
        ("nl", "\n"),
    ] {
        let path = file(name, text, 0o600);
        assert_eq!(
            read_key(&path),
            Err(Error::Refused(format!(
                "{} does not hold one key: it is empty or holds a space, a control character \
                 or a second line",
                path.display()
            ))),
            "{name}"
        );
    }
    let sub = dir.join("dir");
    std::fs::create_dir(&sub).expect("dir");
    assert_eq!(
        read_key(&sub),
        Err(Error::Refused(format!(
            "{} is not a regular file",
            sub.display()
        )))
    );
    assert!(matches!(
        read_key(&dir.join("missing")),
        Err(Error::Io { .. })
    ));
    let link = dir.join("link");
    std::os::unix::fs::symlink(dir.join("lf"), &link).expect("link");
    assert!(matches!(read_key(&link), Err(Error::Refused(_))));
}
