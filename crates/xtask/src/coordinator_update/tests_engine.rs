//! The tests of the curl adapters: the exact command lines `coordinator-update` gives `curl` for
//! the Docker Engine API and for the coordinator's version route, and how each answer is read.
//! The `Exec` is a script; no program runs.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::engine::{CurlEngine, CurlProbe, Engine, Probe, Probed};
use crate::release::contract::Version;
use crate::release::fakes::scratch;
use crate::release::ports::{Exec, Ran};

/// What one call to curl looked like, and the request body file it was given.
struct Call {
    program: String,
    args: Vec<String>,
    /// The content and mode of the `@file` of `--data-binary`, read when curl "ran".
    body: Option<(Vec<u8>, u32)>,
}

/// An `Exec` that answers from a queue and records each call, reading the body file at the
/// moment of the call (the verb removes it after).
#[derive(Default)]
struct Script {
    replies: RefCell<VecDeque<std::io::Result<Ran>>>,
    calls: RefCell<Vec<Call>>,
}

impl Script {
    fn says(self, status: u16, body: &[u8]) -> Script {
        let mut out = body.to_vec();
        out.extend_from_slice(format!("\n{status}").as_bytes());
        self.ran(true, &out, b"")
    }

    fn ran(self, success: bool, stdout: &[u8], stderr: &[u8]) -> Script {
        self.replies.borrow_mut().push_back(Ok(Ran {
            success,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }));
        self
    }

    fn cannot_start(self) -> Script {
        self.replies
            .borrow_mut()
            .push_back(Err(std::io::Error::other("no such program")));
        self
    }

    fn args(&self, n: usize) -> Vec<String> {
        self.calls.borrow()[n].args.clone()
    }
}

impl Exec for Script {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        let body = args
            .iter()
            .position(|a| a == "--data-binary")
            .and_then(|at| args[at + 1].strip_prefix('@'))
            .filter(|path| path.ends_with("engine-request.json"))
            .map(|path| {
                let mode = std::fs::metadata(path)
                    .expect("the body file")
                    .permissions()
                    .mode();
                (std::fs::read(path).expect("the body file"), mode & 0o777)
            });
        self.calls.borrow_mut().push(Call {
            program: program.to_owned(),
            args: args.to_vec(),
            body,
        });
        self.replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err(std::io::Error::other("not scripted")))
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// The arguments every Engine API call starts with.
fn engine_args(method: &str, tail: &[&str]) -> Vec<String> {
    let mut args = strings(&[
        "-q",
        "--silent",
        "--show-error",
        "--unix-socket",
        "/run/docker.sock",
        "-X",
        method,
    ]);
    args.extend(strings(tail));
    args
}

#[test]
fn an_inspect_is_a_get_through_the_unix_socket_and_a_404_is_none() {
    let dir = scratch("cu-engine-inspect");
    let exec = Script::default()
        .says(200, br#"{"Id":"abc"}"#)
        .says(404, br#"{"message":"No such container"}"#)
        .says(200, br#"{"Id":"img"}"#);
    let engine = CurlEngine::new(&exec, "curl", Path::new("/run/docker.sock"), &dir);
    assert_eq!(
        engine.container("hostkeeper-coordinator").expect("inspect"),
        Some(json!({"Id": "abc"}))
    );
    assert_eq!(engine.container("gone").expect("inspect"), None);
    assert_eq!(
        engine.image("sha256:abc").expect("inspect"),
        Some(json!({"Id": "img"}))
    );
    assert_eq!(
        exec.args(0),
        engine_args(
            "GET",
            &[
                "--write-out",
                "\n%{http_code}",
                "http://localhost/v1.43/containers/hostkeeper-coordinator/json"
            ]
        )
    );
    assert_eq!(
        exec.args(1)[exec.args(1).len() - 1],
        "http://localhost/v1.43/containers/gone/json"
    );
    assert_eq!(
        exec.args(2)[exec.args(2).len() - 1],
        "http://localhost/v1.43/images/sha256:abc/json"
    );
    assert_eq!(exec.calls.borrow()[0].program, "curl");
}

#[test]
fn a_call_that_fails_says_what_curl_or_docker_said() {
    let dir = scratch("cu-engine-errors");
    let exec = Script::default()
        .says(500, b"  boom \n")
        .ran(false, b"", b"curl: (7) Couldn't connect\n")
        .cannot_start()
        .ran(true, b"no status here", b"")
        .says(200, b"not json")
        .says(204, b"")
        .ran(true, b"\nabc", b"");
    let engine = CurlEngine::new(&exec, "/opt/curl", Path::new("/run/docker.sock"), &dir);
    let want = [
        "docker GET /containers/a/json: HTTP 500: boom",
        "docker GET /containers/a/json: curl failed: curl: (7) Couldn't connect",
        "docker GET /containers/a/json: /opt/curl could not be started: no such program",
        "docker GET /containers/a/json: curl printed no HTTP status",
        "docker GET /containers/a/json: not JSON: expected ident at line 1 column 2",
        "docker GET /containers/a/json: HTTP 204: ",
        "docker GET /containers/a/json: curl printed no HTTP status",
    ];
    for why in want {
        assert_eq!(
            engine.container("a").expect_err("an error").to_string(),
            why
        );
    }
    assert_eq!(exec.calls.borrow()[2].program, "/opt/curl");
}

#[test]
fn a_socket_path_that_is_not_text_is_refused() {
    let dir = scratch("cu-engine-socket");
    let exec = Script::default();
    let socket = PathBuf::from(std::ffi::OsStr::from_bytes(b"/run/\xff.sock"));
    let engine = CurlEngine::new(&exec, "curl", &socket, &dir);
    assert_eq!(
        engine.container("a").expect_err("refused").to_string(),
        "docker GET /containers/a/json: the docker socket path is not UTF-8"
    );
    assert!(exec.calls.borrow().is_empty());
}

#[test]
fn a_load_sends_the_tarball_and_reads_the_error_lines_of_the_stream() {
    let dir = scratch("cu-engine-load");
    let tar = dir.join("image.tar");
    let exec = Script::default()
        .says(200, b"{\"stream\":\"Loaded image: x\"}\nnot json\n[1]\n")
        .says(200, b"{\"stream\":\"a\"}\n{\"error\":\"unexpected EOF\",\"errorDetail\":{\"message\":\"unexpected EOF\"}}\n")
        .says(200, b"{\"error\":{\"code\":3}}\n")
        .says(500, b"daemon unavailable");
    let engine = CurlEngine::new(&exec, "curl", Path::new("/run/docker.sock"), &dir);
    engine.load(&tar).expect("loaded");
    assert_eq!(
        exec.args(0),
        engine_args(
            "POST",
            &[
                "--header",
                "Content-Type: application/x-tar",
                "--data-binary",
                &format!("@{}", tar.display()),
                "--write-out",
                "\n%{http_code}",
                "http://localhost/v1.43/images/load",
            ]
        )
    );
    assert_eq!(
        engine.load(&tar).expect_err("error line").to_string(),
        "docker POST /images/load: unexpected EOF"
    );
    assert_eq!(
        engine.load(&tar).expect_err("error object").to_string(),
        "docker POST /images/load: {\"code\":3}"
    );
    assert_eq!(
        engine.load(&tar).expect_err("status").to_string(),
        "docker POST /images/load: HTTP 500: daemon unavailable"
    );
}

#[test]
fn a_create_sends_its_body_from_a_private_file_that_is_removed_after() {
    let dir = scratch("cu-engine-create");
    let exec = Script::default()
        .says(201, br#"{"Id":"newid","Warnings":[]}"#)
        .says(409, br#"{"message":"name in use"}"#)
        .says(201, b"{}")
        .says(201, b"nope");
    let engine = CurlEngine::new(&exec, "curl", Path::new("/run/docker.sock"), &dir);
    let body = json!({"Image": "sha256:abc", "Cmd": ["serve"]});
    assert_eq!(
        engine
            .create("hostkeeper-coordinator-next", &body)
            .expect("created"),
        "newid"
    );
    let file = dir.join("engine-request.json");
    assert_eq!(
        exec.args(0),
        engine_args(
            "POST",
            &[
                "--header",
                "Content-Type: application/json",
                "--data-binary",
                &format!("@{}", file.display()),
                "--write-out",
                "\n%{http_code}",
                "http://localhost/v1.43/containers/create?name=hostkeeper-coordinator-next",
            ]
        )
    );
    let sent = exec.calls.borrow()[0].body.clone().expect("a body");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&sent.0).expect("JSON"),
        body
    );
    assert_eq!(sent.1, 0o600);
    assert!(!file.exists());

    assert_eq!(
        engine.create("n", &body).expect_err("conflict").to_string(),
        "docker POST /containers/create?name=n: HTTP 409: {\"message\":\"name in use\"}"
    );
    assert_eq!(
        engine.create("n", &body).expect_err("no id").to_string(),
        "docker POST /containers/create?name=n: the answer has no Id"
    );
    assert_eq!(
        engine.create("n", &body).expect_err("not json").to_string(),
        "docker POST /containers/create?name=n: not JSON: expected ident at line 1 column 2"
    );
    assert!(!file.exists());
}

#[test]
fn a_body_that_cannot_be_written_is_an_error_and_runs_nothing() {
    let dir = scratch("cu-engine-nowrite");
    let exec = Script::default();
    let missing = dir.join("missing");
    let engine = CurlEngine::new(&exec, "curl", Path::new("/run/docker.sock"), &missing);
    let err = engine
        .create("n", &json!({}))
        .expect_err("no directory")
        .to_string();
    assert_eq!(
        err,
        format!(
            "docker POST /containers/create?name=n: write {}: No such file or directory (os error 2)",
            missing.join("engine-request.json").display()
        )
    );
    assert!(exec.calls.borrow().is_empty());
}

#[test]
fn stop_start_remove_and_rename_are_posts_and_a_delete_without_the_volumes_flag() {
    let dir = scratch("cu-engine-verbs");
    let exec = Script::default()
        .says(204, b"")
        .says(304, b"")
        .says(204, b"")
        .says(304, b"")
        .says(204, b"")
        .says(204, b"")
        .says(404, b"{\"message\":\"no such container\"}");
    let engine = CurlEngine::new(&exec, "curl", Path::new("/run/docker.sock"), &dir);
    engine.stop("abc", 30).expect("stopped");
    engine.stop("abc", 30).expect("already stopped");
    engine.start("abc").expect("started");
    engine.start("abc").expect("already started");
    engine.remove("abc").expect("removed");
    engine
        .rename("abc", "hostkeeper-coordinator")
        .expect("renamed");
    let last = |n: usize| {
        let args = exec.args(n);
        (args[6].clone(), args[args.len() - 1].clone(), args.len())
    };
    assert_eq!(
        last(0),
        (
            "POST".to_owned(),
            "http://localhost/v1.43/containers/abc/stop?t=30".to_owned(),
            10
        )
    );
    assert_eq!(
        last(2),
        (
            "POST".to_owned(),
            "http://localhost/v1.43/containers/abc/start".to_owned(),
            10
        )
    );
    assert_eq!(
        last(4),
        (
            "DELETE".to_owned(),
            "http://localhost/v1.43/containers/abc".to_owned(),
            10
        )
    );
    assert_eq!(
        last(5),
        (
            "POST".to_owned(),
            "http://localhost/v1.43/containers/abc/rename?name=hostkeeper-coordinator".to_owned(),
            10
        )
    );
    assert_eq!(
        engine
            .rename("abc", "n")
            .expect_err("no such container")
            .to_string(),
        "docker POST /containers/abc/rename?name=n: HTTP 404: {\"message\":\"no such container\"}"
    );
}

/// A frame of Docker's multiplexed stream.
fn frame(stream: u8, text: &str) -> Vec<u8> {
    let mut out = vec![stream, 0, 0, 0];
    out.extend_from_slice(&(text.len() as u32).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

#[test]
fn logs_are_read_without_the_frame_headers_of_the_multiplexed_stream() {
    let dir = scratch("cu-engine-logs");
    let mut framed = frame(1, "line one\n");
    framed.extend(frame(2, "line two\n"));
    let mut truncated = frame(1, "ok\n");
    truncated.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 9, b'a']);
    let mut bad_stream = frame(1, "ok\n");
    bad_stream.extend(frame(7, "x"));
    let exec = Script::default()
        .says(200, &framed)
        .says(200, b"plain text from a terminal\n")
        .says(200, &truncated)
        .says(200, &bad_stream)
        .says(200, b"")
        .says(200, &[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, b'a'])
        .says(404, b"{\"message\":\"gone\"}");
    let engine = CurlEngine::new(&exec, "curl", Path::new("/run/docker.sock"), &dir);
    assert_eq!(
        engine.logs("abc", 50).expect("logs"),
        "line one\nline two\n"
    );
    assert_eq!(
        exec.args(0)[exec.args(0).len() - 1],
        "http://localhost/v1.43/containers/abc/logs?stdout=1&stderr=1&tail=50"
    );
    assert_eq!(
        engine.logs("abc", 50).expect("plain"),
        "plain text from a terminal\n"
    );
    let truncated_text = String::from_utf8_lossy(&truncated).into_owned();
    assert_eq!(engine.logs("abc", 50).expect("truncated"), truncated_text);
    let bad_text = String::from_utf8_lossy(&bad_stream).into_owned();
    assert_eq!(engine.logs("abc", 50).expect("bad stream"), bad_text);
    assert_eq!(engine.logs("abc", 50).expect("empty"), "");
    let huge = String::from_utf8_lossy(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, b'a']).into_owned();
    assert_eq!(engine.logs("abc", 50).expect("huge frame"), huge);
    assert_eq!(
        engine.logs("abc", 50).expect_err("gone").to_string(),
        "docker GET /containers/abc/logs?stdout=1&stderr=1&tail=50: HTTP 404: {\"message\":\"gone\"}"
    );
}

/// The probe over a script, at `https://c.example.com:8444`.
fn probe_of<'a>(exec: &'a Script, ca: &'a Path) -> CurlProbe<'a> {
    CurlProbe::new(exec, "curl", "https://c.example.com:8444", ca)
}

#[test]
fn the_version_probe_is_a_pinned_https_get_trusting_only_the_ca_file() {
    let ca = PathBuf::from("/etc/hostkeeper/coordinator-ca.pem");
    let exec = Script::default()
        .says(200, br#"{"version":"0.1.219","commit":"0123456789ab"}"#)
        .says(409, br#"{"error":"coordinator_older"}"#);
    let probe = probe_of(&exec, &ca);
    assert_eq!(
        probe.version(None),
        Probed::Running("0.1.219".parse::<Version>().expect("v"))
    );
    assert_eq!(
        exec.args(0),
        strings(&[
            "-q",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-redirs",
            "0",
            "--cacert",
            "/etc/hostkeeper/coordinator-ca.pem",
            "--max-time",
            "10",
            "--write-out",
            "\n%{http_code}",
            "--url",
            "https://c.example.com:8444/v1/version",
        ])
    );
    let at = "0.1.220".parse::<Version>().expect("v");
    assert_eq!(probe.version(Some(&at)), Probed::Older);
    assert_eq!(
        exec.args(1)[exec.args(1).len() - 1],
        "https://c.example.com:8444/v1/version?at_least=0.1.220"
    );
}

#[test]
fn the_version_probe_tells_a_missing_route_from_trouble() {
    let ca = PathBuf::from("/ca.pem");
    let exec = Script::default()
        .says(404, br#"{"error":"not_found"}"#)
        .says(200, b"<html>")
        .says(200, br#"{"version":"x"}"#)
        .says(503, b"")
        .ran(false, b"", b"curl: (60) SSL certificate problem\n")
        .cannot_start();
    let probe = probe_of(&exec, &ca);
    assert_eq!(probe.version(None), Probed::NotFound);
    let url = "GET https://c.example.com:8444/v1/version";
    assert_eq!(
        probe.version(None),
        Probed::Failed(format!("{url}: no version in \"<html>\""))
    );
    assert_eq!(
        probe.version(None),
        Probed::Failed(format!(
            "{url}: no version in \"{{\\\"version\\\":\\\"x\\\"}}\""
        ))
    );
    assert_eq!(
        probe.version(None),
        Probed::Failed(format!("{url}: HTTP 503"))
    );
    assert_eq!(
        probe.version(None),
        Probed::Failed(format!(
            "{url}: curl failed: curl: (60) SSL certificate problem"
        ))
    );
    assert_eq!(
        probe.version(None),
        Probed::Failed(format!("{url}: curl could not be started: no such program"))
    );
}
