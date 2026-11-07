//! The calls to the services' APIs on loopback: printed always, made only with `--apply`, and
//! the real client's request and answer on an in-process listener on `127.0.0.1`, port 0.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;

use common::{FakeApi, FakeNet, config, with_env_api};
use hostkeeper_mdm::api::{Api, Call, HttpLoopback, MAX_ANSWER, Method, basic_auth, call, shown};
use hostkeeper_mdm::{Loopback, MdmError};
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Mode};

/// A host with NanoHUB's secret file, the key `k3y`.
fn host_with_key() -> FakeHost {
    let host = FakeHost::new(0);
    host.put_dir("/etc/hostkeeper-mdm", 0o750, (0, 900));
    host.put_file(
        "/etc/hostkeeper-mdm/nanohub.env",
        0o600,
        (0, 0),
        b"NANOHUB_API_KEY=k3y\n",
    );
    host.put_file(
        "/etc/hostkeeper-mdm/nanodep.env",
        0o600,
        (0, 0),
        b"NANODEP_API=d3p\n",
    );
    host
}

/// Catches a dry run that calls the API, a call line that shows the body (a private key travels
/// in it) or the key, and a refusal reported as done: the dry run prints the line and sends
/// nothing; the apply prints `+ <line>` and sends the body with the key from the secret file;
/// an answer outside 2xx fails (exit 1) with its status and the start of its body; a call that
/// does not complete fails with the transport's reason.
#[test]
fn a_call_is_printed_and_made_only_with_apply() {
    let host = host_with_key();
    let net = FakeNet::new(Default::default());
    let api = FakeApi::new();
    api.reply(
        Method::Put,
        "/api/v1/nanomdm/pushcert",
        200,
        b"{\"topic\":\"t\"}",
    );
    api.reply(Method::Post, "/refuse", 403, b"  no such topic  ");
    let c = config();
    with_env_api(&host, &net, &api, 0, |env| {
        let nanohub = Api::nanohub(env, &c).expect("the key file is good");
        let put = Call::put("/api/v1/nanomdm/pushcert", b"CERT+KEY".to_vec());
        let mut out = Vec::new();
        assert_eq!(
            call(env, &nanohub, &put, Mode::DryRun, &mut out).unwrap(),
            None
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "DRY-RUN: PUT http://127.0.0.1:9004/api/v1/nanomdm/pushcert (NanoHUB API, 8 bytes)\n"
        );
        assert!(api.sent().is_empty());

        let mut out = Vec::new();
        let got = call(env, &nanohub, &put, Mode::Apply, &mut out).unwrap();
        assert_eq!(got, Some(b"{\"topic\":\"t\"}".to_vec()));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "+ PUT http://127.0.0.1:9004/api/v1/nanomdm/pushcert (NanoHUB API, 8 bytes)\n"
        );
        let sent = api.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            (
                sent[0].service,
                sent[0].port,
                sent[0].user,
                sent[0].key.as_str()
            ),
            ("NanoHUB", 9004, "nanohub", "k3y")
        );
        assert_eq!(sent[0].call, put);

        let refused = Call::post_json("/refuse", b"{}".to_vec());
        let err = call(env, &nanohub, &refused, Mode::Apply, &mut Vec::new()).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(
            err.to_string(),
            "POST http://127.0.0.1:9004/refuse (NanoHUB API, 2 bytes): answered 403: no such topic"
        );
        let get = Call::get("/down");
        let err = call(env, &nanohub, &get, Mode::Apply, &mut Vec::new()).unwrap_err();
        assert!(matches!(err, MdmError::Api { .. }));
        assert_eq!(
            err.to_string(),
            "GET http://127.0.0.1:9004/down (NanoHUB API): fake api: no reply for GET /down"
        );

        let nanodep = Api::nanodep(env, &c).expect("the key file is good");
        assert_eq!(
            (nanodep.name, nanodep.port, nanodep.user),
            ("NanoDEP", 9001, "depserver")
        );
        assert_eq!(nanodep.key.expose(), "d3p");
    });
}

/// Catches an API key read from a file others can read or change, or a call made without a
/// key: a key file that is not 0600 and root's is refused (exit 2) before any call, and so is
/// a missing one.
#[test]
fn a_key_file_others_can_read_is_refused() {
    let host = host_with_key();
    let net = FakeNet::new(Default::default());
    let api = FakeApi::new();
    let c = config();
    host.put_file(
        "/etc/hostkeeper-mdm/nanohub.env",
        0o644,
        (0, 0),
        b"NANOHUB_API_KEY=k3y\n",
    );
    host.remove("/etc/hostkeeper-mdm/nanodep.env").unwrap();
    with_env_api(&host, &net, &api, 0, |env| {
        let err = Api::nanohub(env, &c).err().expect("refused");
        assert_eq!(err.exit_code(), 2);
        assert!(matches!(err, MdmError::SecretMode { .. }), "{err}");
        let err = Api::nanodep(env, &c).err().expect("refused");
        assert_eq!(
            err.to_string(),
            "/etc/hostkeeper-mdm/nanodep.env has no NANODEP_API= line with a value"
        );
    });
    assert!(api.sent().is_empty());
}

/// Catches a call line that names another address than loopback, or a `Debug` of a call that
/// prints its body: the URL is `http://127.0.0.1:<port><path>` and the body is shown by size.
#[test]
fn a_call_is_shown_by_its_url_and_size_only() {
    let api = Api {
        name: "NanoDEP",
        port: 9001,
        user: "depserver",
        key: secret("k"),
    };
    let post = Call::post_json("/proxy/x/profile", b"{\"secret\":1}".to_vec());
    assert_eq!(
        shown(&api, &post),
        "POST http://127.0.0.1:9001/proxy/x/profile (NanoDEP API, 12 bytes)"
    );
    assert_eq!(
        format!("{post:?}"),
        "Call { method: Post, path: \"/proxy/x/profile\", body: 12 bytes, json: true }"
    );
    assert_eq!(Method::Get.to_string(), "GET");
    // base64("nanohub:k3y")
    assert_eq!(
        basic_auth("nanohub", &secret("k3y")),
        "Basic bmFub2h1YjprM3k="
    );
}

fn secret(value: &str) -> hostkeeper_mdm::secret::Secret {
    let host = FakeHost::new(0);
    host.put_file("/k.env", 0o600, (0, 0), format!("K={value}\n").as_bytes());
    hostkeeper_mdm::secret::read(&host, "/k.env", "K").expect("a good key file")
}

/// One request as the listener read it: the request line, the headers (lower-case names) and
/// the body.
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Serves one connection on `127.0.0.1`, port 0: reads the request, answers `reply` whole, and
/// returns what it read.
fn serve_once(reply: Vec<u8>) -> (u16, thread::JoinHandle<Seen>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let port = listener.local_addr().expect("address").port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("a connection");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("request line");
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).expect("header");
            let h = h.trim_end().to_owned();
            if h.is_empty() {
                break;
            }
            let (n, v) = h.split_once(':').expect("name: value");
            headers.push((n.to_ascii_lowercase(), v.trim().to_owned()));
        }
        let len = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .map_or(0, |(_, v)| v.parse::<usize>().expect("a length"));
        let mut body = vec![0; len];
        reader.read_exact(&mut body).expect("body");
        let mut stream = stream;
        stream.write_all(&reply).expect("reply");
        Seen {
            line: line.trim_end().to_owned(),
            headers,
            body,
        }
    });
    (port, handle)
}

fn http(status: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn api_on(port: u16) -> Api {
    Api {
        name: "NanoHUB",
        port,
        user: "nanohub",
        key: secret("k3y"),
    }
}

/// Catches a real client that puts the key anywhere but the Basic authentication header, drops
/// the body or the JSON content type, or turns a refusal into an error of the transport: the
/// listener reads the method, path, header and body sent, and a 403 comes back as an answer
/// with its status and body.
#[test]
fn the_real_client_sends_the_key_in_the_header_and_returns_any_status() {
    let (port, seen) = serve_once(http("200 OK", b"{\"topic\":\"t\"}"));
    let put = Call::put("/api/v1/nanomdm/pushcert", b"CERT+KEY".to_vec());
    let answer = HttpLoopback::new()
        .send(&api_on(port), &put)
        .expect("answered");
    assert_eq!(
        (answer.status, answer.body.as_slice()),
        (200, &b"{\"topic\":\"t\"}"[..])
    );
    let seen = seen.join().expect("served");
    assert_eq!(seen.line, "PUT /api/v1/nanomdm/pushcert HTTP/1.1");
    assert_eq!(seen.header("authorization"), Some("Basic bmFub2h1YjprM3k="));
    assert_eq!(seen.header("content-type"), None);
    assert_eq!(seen.body, b"CERT+KEY");
    assert!(!seen.line.contains("k3y"));

    let (port, seen) = serve_once(http("403 Forbidden", b"no"));
    let post = Call::post_json("/proxy/x/profile", b"{}".to_vec());
    let answer = HttpLoopback::default()
        .send(&api_on(port), &post)
        .expect("answered");
    assert_eq!((answer.status, answer.body.as_slice()), (403, &b"no"[..]));
    let seen = seen.join().expect("served");
    assert_eq!(seen.line, "POST /proxy/x/profile HTTP/1.1");
    assert_eq!(
        seen.header("content-type"),
        Some("application/json;charset=UTF8")
    );
    assert_eq!(seen.body, b"{}");

    let (port, seen) = serve_once(http("200 OK", b"pem"));
    let get = Call::get("/v1/tokenpki/x?cn=y&validity_days=7");
    let answer = HttpLoopback::new()
        .send(&api_on(port), &get)
        .expect("answered");
    assert_eq!(answer.body, b"pem");
    let seen = seen.join().expect("served");
    assert_eq!(
        seen.line,
        "GET /v1/tokenpki/x?cn=y&validity_days=7 HTTP/1.1"
    );
}

/// Catches an answer read without a bound, and a service that is down taken for an answer: an
/// answer longer than [`MAX_ANSWER`] is an error, and so is a port nothing listens on.
#[test]
fn an_oversized_answer_or_a_closed_port_is_an_error() {
    let big = vec![b'x'; usize::try_from(MAX_ANSWER).unwrap() + 1];
    let (port, seen) = serve_once(http("200 OK", &big));
    let err = HttpLoopback::new()
        .send(&api_on(port), &Call::get("/big"))
        .unwrap_err();
    assert!(err.contains("limit"), "{err}");
    seen.join().expect("served");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    assert!(
        HttpLoopback::new()
            .send(&api_on(port), &Call::get("/x"))
            .is_err()
    );
}
