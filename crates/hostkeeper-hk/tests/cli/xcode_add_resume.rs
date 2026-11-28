//! `hk xcode add`'s upload that resumes, against a fake Xcode store on a loopback port of 0 that
//! answers the query (`GET /v1/xcode/<sha256>/upload`) and takes a `PUT` with `Content-Range` as
//! the coordinator does: a part is appended only at the offset the store holds (else `409
//! offset_mismatch` and its `received`), `202` while bytes are due, the whole file hashed when the
//! last byte is in, `201` then. A test scripts what the store does to the next requests (cut the
//! connection at a byte, lose the answer, refuse). The waits between the tries are read from the
//! world's wait, which these tests replace: nothing sleeps.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_hk::tailnet_wait::Tailnet;
use rustls::{ServerConnection, StreamOwned};

use crate::add_rig::{Add, FLEET, LIST, Store, sha256, xip};
use crate::rig::{Opts, Pki, Ran, Reply, TOKEN, server_config};

/// What the store does to its next `PUT`.
#[derive(Clone, Copy)]
enum Put {
    /// Reads this many bytes of the body, keeps them, and closes the connection.
    Cut(usize),
    /// Takes the whole request as usual and closes the connection without answering.
    Lose,
    /// Another writer got `n` bytes in: the store holds them, and this request is `409
    /// offset_mismatch` with `received` `n`.
    Mismatch(usize),
    /// `409 partial_locked`.
    Locked,
    /// `409 partial_differs`.
    Differs,
    /// Keeps this many bytes of the body and answers `202`.
    Accept(usize),
    /// `409 offset_mismatch` with this `received`, whatever the store holds.
    Report(usize),
}

/// What the store does to its next query.
#[derive(Clone)]
enum Get {
    /// Answers as the store is.
    Normal,
    /// Closes the connection with no answer.
    Hang,
    /// Answers `status` and `body`.
    Answer(u16, &'static str),
}

/// One request the store saw.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body_len: usize,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How the store is set up and what it holds.
#[derive(Default)]
struct State {
    /// The store answers the query `405`, and takes a `PUT` whole: a coordinator without resume.
    older: bool,
    /// The bytes of the partial upload.
    partial: Vec<u8>,
    /// The bytes of the archive committed.
    stored: Option<Vec<u8>>,
    puts: VecDeque<Put>,
    gets: VecDeque<Get>,
    seen: Vec<Seen>,
}

/// The store, and its port.
struct Fake {
    port: u16,
    state: Arc<Mutex<State>>,
    archive: Vec<u8>,
}

/// The archive of these tests: an xip of 1004 bytes.
fn archive() -> Vec<u8> {
    xip(&"0123456789".repeat(100))
}

fn reply(tls: &mut StreamOwned<ServerConnection, std::net::TcpStream>, status: u16, body: &str) {
    let text = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tls.write_all(text.as_bytes());
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

/// The first and the size of `bytes <first>-<last>/<size>`.
fn range_of(text: &str) -> Option<(usize, usize, usize)> {
    let (span, size) = text.strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    Some((first.parse().ok()?, last.parse().ok()?, size.parse().ok()?))
}

impl Fake {
    fn start(pki: &Pki, archive: Vec<u8>, state: State) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let config = server_config(pki);
        let state = Arc::new(Mutex::new(state));
        let (shared, bytes) = (Arc::clone(&state), archive.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let Ok(conn) = ServerConnection::new(Arc::clone(&config)) else {
                    continue;
                };
                let mut tls = StreamOwned::new(conn, stream);
                serve(&mut tls, &shared, &bytes);
            }
        });
        Fake {
            port,
            state,
            archive,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.state.lock().expect("the state").seen.clone()
    }

    /// The requests that are the upload's: the queries and the `PUT`s, as `METHOD target`.
    fn upload_requests(&self) -> Vec<String> {
        self.seen()
            .into_iter()
            .filter(|s| s.method != "POST")
            .map(|s| format!("{} {}", s.method, s.target.split('?').next().unwrap_or("")))
            .collect()
    }

    /// The `Content-Range` of each `PUT`, `-` for one with none.
    fn ranges(&self) -> Vec<String> {
        self.seen()
            .into_iter()
            .filter(|s| s.method == "PUT")
            .map(|s| s.header("content-range").unwrap_or("-").to_owned())
            .collect()
    }

    fn stored(&self) -> Option<Vec<u8>> {
        self.state.lock().expect("the state").stored.clone()
    }

    fn partial(&self) -> usize {
        self.state.lock().expect("the state").partial.len()
    }

    fn sha(&self) -> String {
        sha256(&self.archive)
    }
}

/// Reads one request and answers it.
fn serve(
    tls: &mut StreamOwned<ServerConnection, std::net::TcpStream>,
    shared: &Mutex<State>,
    archive: &[u8],
) {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let split = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        match tls.read(&mut chunk) {
            Ok(n) if n > 0 => bytes.extend_from_slice(&chunk[..n]),
            _ => return,
        }
    };
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let mut body = bytes[split + 4..].to_vec();
    let mut lines = head.lines();
    let mut start = lines.next().unwrap_or_default().split(' ');
    let method = start.next().unwrap_or_default().to_owned();
    let target = start.next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    if header("expect").is_some() {
        let _ = tls.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        let _ = tls.flush();
    }
    let length: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while body.len() < length {
        match tls.read(&mut chunk) {
            Ok(n) if n > 0 => body.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }
    let mut state = shared.lock().expect("the state");
    state.seen.push(Seen {
        method: method.clone(),
        target: target.clone(),
        headers: headers.clone(),
        body_len: body.len(),
    });
    let sha = sha256(archive);
    match method.as_str() {
        "POST" if target == LIST => reply(tls, 200, "{\"configured\":true}"),
        "POST" if FLEET.contains(&target.as_str()) => reply(tls, 200, "{}"),
        "GET" if target == format!("/v1/xcode/{sha}/upload") => {
            if state.older {
                return reply(tls, 405, "{\"error\":\"method_not_allowed\"}");
            }
            match state.gets.pop_front() {
                Some(Get::Hang) => {}
                Some(Get::Answer(status, text)) => reply(tls, status, text),
                Some(Get::Normal) | None => {
                    let answer = if state.stored.is_some() {
                        format!(
                            "{{\"state\":\"held\",\"received\":{0},\"size\":{0}}}",
                            archive.len()
                        )
                    } else if state.partial.is_empty() {
                        "{\"state\":\"none\",\"received\":0,\"size\":0}".to_owned()
                    } else {
                        format!(
                            "{{\"state\":\"partial\",\"received\":{},\"size\":{}}}",
                            state.partial.len(),
                            archive.len()
                        )
                    };
                    reply(tls, 200, &answer);
                }
            }
        }
        "PUT" if target.starts_with(&format!("/v1/xcode/{sha}?")) => {
            put(tls, &mut state, archive, &body, header("content-range"));
        }
        _ => reply(tls, 400, "{\"error\":\"bad_request\"}"),
    }
}

/// A `PUT` of `body`, as the coordinator takes it.
fn put(
    tls: &mut StreamOwned<ServerConnection, std::net::TcpStream>,
    state: &mut State,
    archive: &[u8],
    body: &[u8],
    range: Option<String>,
) {
    let mismatch = |tls: &mut StreamOwned<_, _>, held: usize| {
        reply(
            tls,
            409,
            &format!("{{\"error\":\"offset_mismatch\",\"received\":{held}}}"),
        );
    };
    let sha = sha256(archive);
    let Some(range) = range else {
        // The whole upload of a coordinator that cannot resume.
        return if sha256(body) == sha {
            state.stored = Some(body.to_vec());
            reply(tls, 201, "{}");
        } else {
            reply(tls, 400, "{\"error\":\"digest_mismatch\"}");
        };
    };
    let Some((first, last, size)) = range_of(&range).filter(|(f, l, s)| f <= l && l < s) else {
        return reply(tls, 400, "{\"error\":\"bad_request\"}");
    };
    let act = state.puts.pop_front();
    // A partial that holds every byte is finished by any part: nothing is appended.
    if state.partial.len() == size && size == archive.len() && act.is_none() {
        return finish(tls, state, archive);
    }
    let append = |state: &mut State, part: &[u8]| {
        if first == state.partial.len() {
            state.partial.extend_from_slice(part);
        }
    };
    match act {
        Some(Put::Cut(n)) => return append(state, &body[..n.min(body.len())]),
        Some(Put::Mismatch(n)) => {
            state.partial = archive[..n].to_vec();
            return mismatch(tls, n);
        }
        Some(Put::Report(n)) => return mismatch(tls, n),
        Some(Put::Locked) => return reply(tls, 409, "{\"error\":\"partial_locked\"}"),
        Some(Put::Differs) => return reply(tls, 409, "{\"error\":\"partial_differs\"}"),
        Some(Put::Accept(n)) => {
            append(state, &body[..n.min(body.len())]);
            return reply(
                tls,
                202,
                &format!("{{\"received\":{}}}", state.partial.len()),
            );
        }
        Some(Put::Lose) | None => {}
    }
    if first != state.partial.len() {
        return mismatch(tls, state.partial.len());
    }
    if body.len() != last + 1 - first || size != archive.len() {
        return reply(tls, 400, "{\"error\":\"bad_request\"}");
    }
    append(state, body);
    finish_with(tls, state, archive, size, matches!(act, Some(Put::Lose)));
}

/// The end of a part of a store that holds every byte: hashed, stored or refused.
fn finish(
    tls: &mut StreamOwned<ServerConnection, std::net::TcpStream>,
    state: &mut State,
    archive: &[u8],
) {
    finish_with(tls, state, archive, archive.len(), false);
}

/// The answer to a part after which the store holds `state.partial`, of an archive of `size`.
fn finish_with(
    tls: &mut StreamOwned<ServerConnection, std::net::TcpStream>,
    state: &mut State,
    archive: &[u8],
    size: usize,
    lose: bool,
) {
    let sha = sha256(archive);
    let answer = if state.partial.len() < size {
        Some((202, format!("{{\"received\":{}}}", state.partial.len())))
    } else if sha256(&state.partial) == sha {
        state.stored = Some(std::mem::take(&mut state.partial));
        Some((
            201,
            "{\"sha256\":\"\",\"version\":\"27.2\",\"build\":\"27B52\"}".to_owned(),
        ))
    } else {
        state.partial.clear();
        Some((400, "{\"error\":\"digest_mismatch\"}".to_owned()))
    };
    if let (Some((status, text)), false) = (answer, lose) {
        reply(tls, status, &text);
    }
}

/// The run of `hk xcode add <archive>` against `fake`, and the seconds each wait was asked for.
fn go(add: &Add, fake: &Fake, path: &str) -> (Ran, Vec<u64>) {
    let waits = RefCell::new(Vec::new());
    let wait = |time: Duration| waits.borrow_mut().push(time.as_secs());
    let tailnet = Tailnet {
        tailscale: add.rig.path("no-tailscale"),
        limit: Duration::from_secs(3600),
        wait: &wait,
        now: &|| Duration::ZERO,
    };
    let ran = add.run_with(
        Opts::real().tailnet(&tailnet),
        fake.port,
        &["xcode", "add", path],
    );
    (ran, waits.into_inner())
}

/// A test: its directory, the archive in `~/Downloads`, and the fake store set up by `state`.
fn rig(name: &str, state: State) -> (Add, Fake, String) {
    let add = Add::new(&format!("resume-{name}"));
    let path = add.file("Xcode_27.2.xip", &archive(), 100);
    let fake = Fake::start(&add.rig.pki, archive(), state);
    (add, fake, path)
}

fn with(puts: &[Put], gets: &[Get]) -> State {
    State {
        puts: puts.iter().copied().collect(),
        gets: gets.iter().cloned().collect(),
        ..State::default()
    }
}

/// The line a good run ends its upload with on stdout.
fn stored_line() -> String {
    format!(
        "stored: Xcode 27.2 (27B52), sha256 {}, {} bytes\n",
        sha256(&archive()),
        archive().len()
    )
}

fn last_line(ran: &Ran) -> &str {
    ran.stderr.lines().last().unwrap_or_default()
}

/// Proves: a good run asks the store what it holds, then sends the archive in one `PUT` with
/// `Content-Range: bytes 0-1003/1004` and `Content-Length` 1004, after `Expect: 100-continue`;
/// no wait is made, and the store ends with the archive's bytes.
#[test]
fn a_new_upload_asks_first_and_sends_one_put_with_the_whole_range() {
    let (add, fake, path) = rig("new", State::default());
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, stored_line().as_str()),
        "{ran:?}"
    );
    assert_eq!(
        fake.upload_requests(),
        [
            format!("GET /v1/xcode/{}/upload", fake.sha()),
            format!("PUT /v1/xcode/{}", fake.sha())
        ]
    );
    assert_eq!(fake.ranges(), ["bytes 0-1003/1004"]);
    let put = fake
        .seen()
        .into_iter()
        .find(|s| s.method == "PUT")
        .expect("a PUT");
    assert_eq!(put.header("content-length"), Some("1004"));
    assert_eq!(put.header("expect"), Some("100-continue"));
    assert_eq!(waits, Vec::<u64>::new());
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: a connection cut after 400 bytes of the body continues from the 400 bytes the store
/// holds: after a wait of 2 seconds the query is asked again and the second `PUT` says
/// `Content-Range: bytes 400-1003/1004` with 604 bytes; the bytes the store ends with equal the
/// archive; stderr says where it continues; stdout is the one `stored:` line.
#[test]
fn a_cut_connection_continues_from_the_byte_the_store_holds() {
    let (add, fake, path) = rig("cut", with(&[Put::Cut(400)], &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, stored_line().as_str()),
        "{ran:?}"
    );
    assert_eq!(fake.ranges(), ["bytes 0-1003/1004", "bytes 400-1003/1004"]);
    let lengths: Vec<usize> = fake
        .seen()
        .iter()
        .filter(|s| s.method == "PUT")
        .map(|s| s.body_len)
        .collect();
    assert_eq!(lengths, [1004, 604]);
    let methods: Vec<String> = fake.upload_requests();
    assert_eq!(
        methods
            .iter()
            .map(|m| m.split(' ').next().unwrap_or(""))
            .collect::<Vec<_>>(),
        ["GET", "PUT", "GET", "PUT"]
    );
    assert_eq!(waits, [2]);
    assert_eq!(fake.stored(), Some(archive()));
    assert!(
        ran.stderr.contains("hk: continuing at 0.40 of 1.00 KB\n"),
        "{ran:?}"
    );
}

/// Proves: a `409 offset_mismatch` is retried from the `received` of its answer without asking
/// again (the second `PUT` starts at 300, where another writer got to), after a wait of 2
/// seconds, and the store ends with the archive.
#[test]
fn an_offset_mismatch_is_retried_from_the_offset_it_reports() {
    let (add, fake, path) = rig("mismatch", with(&[Put::Mismatch(300)], &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(fake.ranges(), ["bytes 0-1003/1004", "bytes 300-1003/1004"]);
    assert_eq!(fake.upload_requests().len(), 3);
    assert_eq!(waits, [2]);
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: a `409 partial_locked` is retried after the query is asked again, twice here, with
/// waits of 2 and 4 seconds, and then the upload goes through from byte 0.
#[test]
fn a_partial_locked_is_retried_after_asking_again() {
    let (add, fake, path) = rig("locked", with(&[Put::Locked, Put::Locked], &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, stored_line().as_str()),
        "{ran:?}"
    );
    assert_eq!(fake.ranges(), ["bytes 0-1003/1004"; 3]);
    assert_eq!(fake.upload_requests().len(), 6);
    assert_eq!(waits, [2, 4]);
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: five cut connections end the run with exit 1 and exactly the line `the upload stopped
/// at 0.50 of 1.00 KB; run hk xcode add again to continue`, the waits are 2, 4, 8 and 16 seconds
/// in that order, and a second run continues from the 500 bytes the store kept and stores the
/// archive.
#[test]
fn five_failures_stop_with_the_offset_said_and_the_next_run_continues() {
    let cuts = [Put::Cut(100); 5];
    let (add, fake, path) = rig("five", with(&cuts, &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
    assert_eq!(
        last_line(&ran),
        "hk: the upload stopped at 0.50 of 1.00 KB; run hk xcode add again to continue"
    );
    assert_eq!(waits, [2, 4, 8, 16]);
    assert_eq!(fake.ranges().len(), 5);
    assert_eq!(fake.partial(), 500);
    assert_eq!(fake.stored(), None);
    let (again, waits) = go(&add, &fake, &path);
    assert_eq!(
        (again.code, again.stdout.as_str()),
        (0, stored_line().as_str()),
        "{again:?}"
    );
    assert_eq!(
        fake.ranges().last().map(String::as_str),
        Some("bytes 500-1003/1004")
    );
    assert_eq!(waits, Vec::<u64>::new());
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: a store that already holds a partial upload at the start (an earlier run's) is
/// continued from its `received` with the first `PUT`, and no wait is made.
#[test]
fn a_partial_the_store_holds_is_continued_by_the_first_put() {
    let state = State {
        partial: archive()[..250].to_vec(),
        ..State::default()
    };
    let (add, fake, path) = rig("held-partial", state);
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(fake.ranges(), ["bytes 250-1003/1004"]);
    assert_eq!(waits, Vec::<u64>::new());
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: a partial that holds every byte of the archive (the store stopped after the last block
/// and before the commit) is finished by one `PUT` of the last byte again, `Content-Range: bytes
/// 1003-1003/1004`, which is a valid range, not `bytes 1004-1003/1004`; the run ends `stored:` with
/// exit 0 and no wait is made.
#[test]
fn a_partial_that_holds_every_byte_is_finished_by_the_last_byte_again() {
    let state = State {
        partial: archive(),
        ..State::default()
    };
    let (add, fake, path) = rig("full-partial", state);
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(ran.stdout, stored_line());
    assert_eq!(fake.ranges(), ["bytes 1003-1003/1004"]);
    assert_eq!(waits, Vec::<u64>::new());
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: a connection that breaks after the last byte was received, with the answer lost, ends
/// in `stored:` and exit 0, because the query then says the store holds the archive; no second
/// `PUT` is made.
#[test]
fn an_answer_lost_after_the_last_byte_is_found_stored_by_the_query() {
    let (add, fake, path) = rig("lost", with(&[Put::Lose], &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, stored_line().as_str()),
        "{ran:?}"
    );
    assert_eq!(fake.ranges().len(), 1);
    assert_eq!(waits, [2]);
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: a `202` (a part kept, bytes still due) goes on at once from its `received`, with no
/// wait and no query.
#[test]
fn a_202_goes_on_at_once_from_its_received() {
    let (add, fake, path) = rig("accepted", with(&[Put::Accept(600)], &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(fake.ranges(), ["bytes 0-1003/1004", "bytes 600-1003/1004"]);
    assert_eq!(fake.upload_requests().len(), 3);
    assert_eq!(waits, Vec::<u64>::new());
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: an older coordinator (`405` on the query) gets the whole archive in one `PUT` with no
/// `Content-Range`, hk says it cannot resume, and the archive is stored; no wait is made.
#[test]
fn an_older_store_gets_one_whole_put() {
    let state = State {
        older: true,
        ..State::default()
    };
    let (add, fake, path) = rig("older", state);
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, stored_line().as_str()),
        "{ran:?}"
    );
    assert_eq!(fake.ranges(), ["-"]);
    assert_eq!(fake.upload_requests().len(), 2);
    assert!(
        ran.stderr
            .contains("hk: this coordinator cannot resume an upload"),
        "{ran:?}"
    );
    assert_eq!(waits, Vec::<u64>::new());
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: a `404` on the query is a coordinator with no Xcode store: exit 1 and the line saying
/// so, with no `PUT`.
#[test]
fn a_404_on_the_query_is_no_xcode_store() {
    let (add, fake, path) = rig(
        "none",
        with(&[], &[Get::Answer(404, "{\"error\":\"not_found\"}")]),
    );
    let (ran, _) = go(&add, &fake, &path);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
    assert_eq!(
        last_line(&ran),
        "hk: the coordinator has no Xcode store: it was started without --xcode-dir"
    );
    assert!(fake.ranges().is_empty());
}

/// Proves: the query's other answers end the run before any byte is sent: a refusal of the token
/// is exit 3 with its `refused:` line; a status with no code of the store's is exit 1; a `200`
/// that is not the query's answer is exit 1; and no answer at all is exit 1 saying nothing was
/// uploaded.
#[test]
fn the_querys_other_answers_end_the_run_before_the_body() {
    let cases: [(Get, u8, &str); 4] = [
        (
            Get::Answer(401, "{\"error\":\"unauthorized\"}"),
            3,
            "refused: unauthorized",
        ),
        (
            Get::Answer(500, "<html>"),
            1,
            "hk: the answer (HTTP 500) is not the Xcode store's",
        ),
        (
            Get::Answer(200, "{\"state\":\"gone\"}"),
            1,
            "hk: the answer (HTTP 200) is not the Xcode store's",
        ),
        (Get::Hang, 1, ""),
    ];
    for (get, code, says) in cases {
        let (add, fake, path) = rig("query-other", with(&[], &[get]));
        let (ran, _) = go(&add, &fake, &path);
        assert_eq!((ran.code, ran.stdout.as_str()), (code, ""), "{ran:?}");
        if says.is_empty() {
            assert!(
                last_line(&ran).ends_with("; nothing was uploaded"),
                "{ran:?}"
            );
        } else {
            assert_eq!(last_line(&ran), says);
        }
        assert!(fake.ranges().is_empty());
    }
}

/// Proves: a `partial_differs` is a refusal and not a retry: exit 2, the words that say to remove
/// the partial, `refused: partial_differs`, one `PUT`, no wait.
#[test]
fn a_partial_that_differs_is_exit_2_with_no_retry() {
    let (add, fake, path) = rig("differs", with(&[Put::Differs], &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "hk: the coordinator refused the upload (HTTP 409)\nrefused: partial_differs\n"
        ),
        "{ran:?}"
    );
    assert!(ran.stderr.contains("hk xcode remove --sha256"), "{ran:?}");
    assert_eq!(fake.ranges().len(), 1);
    assert_eq!(waits, Vec::<u64>::new());
}

/// Proves: a store that holds the bytes already when the query is first asked (stored since the
/// list) ends the run with `stored:` and sends nothing.
#[test]
fn a_store_that_holds_the_bytes_when_asked_is_stored_with_nothing_sent() {
    let state = State {
        stored: Some(archive()),
        ..State::default()
    };
    let (add, fake, path) = rig("held", state);
    let (ran, _) = go(&add, &fake, &path);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, stored_line().as_str()),
        "{ran:?}"
    );
    assert!(fake.ranges().is_empty());
}

/// Proves: a store that reports more bytes than the archive has, at the query or in a `409
/// offset_mismatch`, ends the run with exit 1 saying so, and sends no further byte.
#[test]
fn an_offset_beyond_the_archive_is_a_failure() {
    let beyond = "{\"state\":\"partial\",\"received\":5000,\"size\":1004}";
    let (add, fake, path) = rig("beyond-query", with(&[], &[Get::Answer(200, beyond)]));
    let (ran, _) = go(&add, &fake, &path);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
    assert_eq!(
        last_line(&ran),
        "hk: the store reports 5000 bytes of this archive, which has 1004"
    );
    assert!(fake.ranges().is_empty());
    let (add, fake, path) = rig("beyond-mismatch", with(&[Put::Report(5000)], &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
    assert_eq!(
        last_line(&ran),
        "hk: the store reports 5000 bytes of this archive, which has 1004"
    );
    assert_eq!(fake.ranges().len(), 1);
    assert_eq!(waits, Vec::<u64>::new());
}

/// Proves: a query that fails between two tries (no answer) costs a try and the run goes on: the
/// waits are 2 and 4 seconds, the archive is stored; one that is refused (`500`) ends the run
/// after one wait, with no second `PUT`.
#[test]
fn a_query_that_fails_between_tries_costs_a_try() {
    let (add, fake, path) = rig(
        "query-cut",
        with(
            &[Put::Cut(400)],
            &[
                Get::Answer(200, "{\"state\":\"none\",\"received\":0,\"size\":0}"),
                Get::Hang,
            ],
        ),
    );
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(waits, [2, 4]);
    assert_eq!(fake.stored(), Some(archive()));
    let (add, fake, path) = rig(
        "query-500",
        with(
            &[Put::Cut(400)],
            &[
                Get::Answer(200, "{\"state\":\"none\",\"received\":0,\"size\":0}"),
                Get::Answer(500, "{\"error\":\"store_failed\"}"),
            ],
        ),
    );
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(waits, [2]);
    assert_eq!(fake.ranges().len(), 1);
}

/// Proves: when the query after the last try is not answered, the stop line says the offset of
/// the last try (400 here, where the store held 500); when it says `none` (the partial lapsed) the
/// line says 0.
#[test]
fn the_stop_line_says_what_the_last_query_or_try_knew() {
    let mut gets = vec![Get::Normal; 5];
    gets.push(Get::Hang);
    let (add, fake, path) = rig("silent", with(&[Put::Cut(100); 5], &gets));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
    assert_eq!(waits, [2, 4, 8, 16]);
    assert_eq!(
        last_line(&ran),
        "hk: the upload stopped at 0.40 of 1.00 KB; run hk xcode add again to continue"
    );
    let mut gets = vec![Get::Normal; 5];
    gets.push(Get::Answer(
        200,
        "{\"state\":\"none\",\"received\":0,\"size\":0}",
    ));
    let (add, fake, path) = rig("lapsed", with(&[Put::Cut(100); 5], &gets));
    let (ran, _) = go(&add, &fake, &path);
    assert_eq!(
        last_line(&ran),
        "hk: the upload stopped at 0.00 of 1.00 KB; run hk xcode add again to continue"
    );
}

/// Proves: when the fifth try loses its answer after the store took the last byte, the query
/// after it finds the archive stored: exit 0 and `stored:`, not the stop line.
#[test]
fn an_archive_stored_by_the_last_try_is_found_by_the_last_query() {
    let mut puts = vec![Put::Locked; 4];
    puts.push(Put::Lose);
    let (add, fake, path) = rig("last-lost", with(&puts, &[]));
    let (ran, waits) = go(&add, &fake, &path);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, stored_line().as_str()),
        "{ran:?}"
    );
    assert_eq!(waits, [2, 4, 8, 16]);
    assert_eq!(fake.stored(), Some(archive()));
}

/// Proves: the token is the bearer of every request to the store through a cut and a resume (the
/// two `Xcode.List` calls, both queries, both `PUT`s and the summary's call) and is in no other
/// header and no target; it is in no argument or environment of a program run, and in no line of
/// stdout or stderr.
#[test]
fn the_token_is_in_the_authorization_header_alone_through_a_resume() {
    let (add, fake, path) = rig("token", with(&[Put::Cut(400)], &[]));
    let (ran, _) = go(&add, &fake, &path);
    assert_eq!(ran.code, 0, "{ran:?}");
    let bearer = format!("Bearer {TOKEN}");
    let seen = fake.seen();
    assert_eq!(seen.len(), 7, "{seen:#?}");
    for request in &seen {
        assert_eq!(request.header("authorization"), Some(bearer.as_str()));
        assert!(!request.target.contains(TOKEN));
        for (name, value) in &request.headers {
            assert!(name == "authorization" || !value.contains(TOKEN), "{name}");
        }
    }
    assert!(!ran.stdout.contains(TOKEN) && !ran.stderr.contains(TOKEN));
    for tool in ["pkgutil", "xip", "plutil", "codesign", "df"] {
        let argv = add.record(&format!("{tool}.args"));
        let env = add.record(&format!("{tool}.env"));
        assert!(!argv.contains(TOKEN) && !env.contains(TOKEN), "{tool}");
    }
}

/// Proves: against the real coordinator's route on its private listener (the one `grpc::route`
/// serves, with a store of its own) the query and the part of a resumed upload are what the
/// coordinator takes: its query answers `{"received":0,"size":0,"state":"none"}`, and its first
/// check that is not about the request's form refuses the `PUT` that carries `Content-Range:
/// bytes 0-1003/1004` as `not_listed` (a coordinator that read no list of releases), which hk
/// says and exits 2 on. The store's own answers to a part that is stored are tested in the
/// coordinator's crate: no list of releases can be given to a coordinator from here without the
/// network, so no archive is stored in this test.
#[test]
fn the_real_coordinators_route_takes_the_query_and_the_content_range() {
    let add = Add::new("resume-real");
    let path = add.file("Xcode_27.2.xip", &archive(), 100);
    let server = crate::gateway::start_with(&add.rig.pki, &add.rig.path(""), true);
    let ran = add.run(server.port, &["xcode", "add", &path]);
    assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{ran:?}");
    assert!(
        ran.stderr
            .ends_with("hk: the coordinator refused the upload (HTTP 409)\nrefused: not_listed\n"),
        "{ran:?}"
    );
    let sha = sha256(&archive());
    let seen = server.seen();
    let query = seen
        .iter()
        .find(|s| s.method == "GET")
        .expect("the query reached the coordinator");
    assert_eq!(query.target, format!("/v1/xcode/{sha}/upload"));
    assert_eq!(
        String::from_utf8_lossy(&query.answer),
        "{\"received\":0,\"size\":0,\"state\":\"none\"}"
    );
    let put = seen
        .iter()
        .find(|s| s.method == "PUT")
        .expect("the part reached the coordinator");
    assert_eq!(put.header("content-range"), Some("bytes 0-1003/1004"));
    assert_eq!(
        String::from_utf8_lossy(&put.answer),
        "{\"error\":\"not_listed\"}"
    );
}

/// Proves: the whole `PUT` to a coordinator that cannot resume is refused as any upload is: `409
/// not_listed` is said in words, `refused: not_listed`, exit 2.
#[test]
fn the_whole_put_to_an_older_store_that_refuses_it_exits_as_listed() {
    let add = Add::new("resume-older-refused");
    let path = add.file("Xcode_27.2.xip", &archive(), 100);
    let server = Store::default()
        .answering(409, "{\"error\":\"not_listed\"}")
        .serve_with(&add.rig, |request| {
            request
                .target
                .ends_with("/upload")
                .then(|| Reply::new(405, "{\"error\":\"method_not_allowed\"}"))
        });
    let ran = add.run(server.port, &["xcode", "add", &path]);
    assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{ran:?}");
    assert!(
        ran.stderr
            .ends_with("hk: the coordinator refused the upload (HTTP 409)\nrefused: not_listed\n"),
        "{ran:?}"
    );
}
