//! The program as a whole: `run` over its ports (version, usage errors, the server path on a
//! listener the test hands in) and the real binary, so that `main` is run too.

mod support;

use std::cell::Cell;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

use hello::VERSION;
use hello::ports::Host;
use hostkeeper_hook::{Outcome, Report};
use support::{FakeProbe, FakeRunner, scratch, wait_until_acceptable};

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

struct Ran {
    code: i32,
    out: String,
    err: String,
}

fn run_with(words: &[&str], listen: &dyn Fn(u16) -> std::io::Result<TcpListener>) -> Ran {
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    let host = Host {
        runner: &runner,
        probe: &probe,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = hello::run_with(&os(words), &host, &mut out, &mut err, listen);
    assert!(runner.take_calls().is_empty(), "the server runs no command");
    Ran {
        code,
        out: String::from_utf8(out).expect("text"),
        err: String::from_utf8(err).expect("text"),
    }
}

/// Catches the version being printed by something other than the package version, on the wrong
/// stream, or starting the server.
#[test]
fn version_prints_the_package_version() {
    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    let ran = run_with(&["--version"], &hello::bind_loopback);
    assert_eq!(
        (ran.code, ran.out.as_str(), ran.err.as_str()),
        (0, &*format!("hello {VERSION}\n"), "")
    );
}

/// Catches an argument the program does not know starting a server or exiting with another code
/// than 2, and the message not reaching standard error.
#[test]
fn an_unknown_argument_is_exit_two_on_standard_error() {
    let ran = run_with(&["--nope"], &hello::bind_loopback);
    assert_eq!(ran.code, 2);
    assert_eq!(ran.out, "");
    assert_eq!(ran.err, "hello: unknown argument \"--nope\"\n");
}

/// Runs the server path with `words` on a listener the test hands in, with one client already
/// connected that sent `request` and waiting in the listener's queue; returns the port it was asked to listen on, what the program
/// did, and the whole answer the client read.
fn serve_one(words: &[&str], request: &str) -> (u16, Ran, String) {
    let asked = Cell::new(0u16);
    let client = Cell::new(None);
    let listen = |port: u16| {
        asked.set(port);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let mut stream = TcpStream::connect(listener.local_addr()?)?;
        stream.write_all(request.as_bytes())?;
        client.set(Some(stream));
        // The connection must be in the listener's queue before the server path's first accept,
        // or that accept fails with `WouldBlock` and the client reads a reset (macOS).
        wait_until_acceptable(&listener);
        Ok(listener)
    };
    let ran = run_with(words, &listen);
    let mut answer = String::new();
    client
        .take()
        .expect("a client")
        .read_to_string(&mut answer)
        .expect("read");
    (asked.get(), ran, answer)
}

/// Catches the server path not binding the port it was given, not answering from the version and
/// the `--fail-ready` flag it was given, not naming the version it serves when it starts, and not
/// ending in exit 1 with its reason when it stops.
#[test]
fn the_server_path_serves_on_its_listener_until_it_stops() {
    for (flag, want, version) in [
        (None, "HTTP/1.1 200 OK\r\n", VERSION.to_owned()),
        (
            Some("--fail-ready"),
            "HTTP/1.1 503 ",
            format!("{VERSION}-fail-ready"),
        ),
    ] {
        let mut words = vec!["--port", "8123"];
        words.extend(flag);
        let (asked, ran, answer) = serve_one(&words, "GET /readyz HTTP/1.1\r\n\r\n");
        assert_eq!(asked, 8123);
        assert_eq!(ran.code, 1);
        assert_eq!(ran.out, "");
        assert!(
            ran.err
                .starts_with(&format!("hello {version}: serving on 127.0.0.1:8123\n")),
            "{}",
            ran.err
        );
        assert!(ran.err.contains("hello: stopped serving: "), "{}", ran.err);
        assert!(answer.starts_with(want), "{flag:?}: {answer:?}");
    }
}

/// Catches the failing build answering `/version` with the good build's version (so the two can
/// be told apart only by `/readyz`), a suffix other than the one its package's version carries,
/// or the suffix leaking into the good build's answer.
#[test]
fn version_answers_the_builds_own_version_with_and_without_fail_ready() {
    let body = |flag: Option<&str>| {
        let mut words = vec!["--port", "8125"];
        words.extend(flag);
        let (_, ran, answer) = serve_one(&words, "GET /version HTTP/1.1\r\n\r\n");
        assert_eq!(ran.code, 1, "{}", ran.err);
        let (head, body) = answer.split_once("\r\n\r\n").expect("a head and a body");
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{answer:?}");
        body.to_owned()
    };
    let good = body(None);
    let failing = body(Some("--fail-ready"));
    assert_eq!(good, format!("{VERSION}\n"));
    assert_eq!(failing, format!("{VERSION}-fail-ready\n"));
    assert_ne!(good, failing);
    assert_eq!(hello::served_version(false), VERSION);
    assert_eq!(
        hello::served_version(true),
        format!("{VERSION}{}", hello::FAIL_READY_SUFFIX)
    );
}

/// Catches a listener that cannot be opened being served anyway, and the reason being lost.
#[test]
fn a_port_that_cannot_be_listened_on_is_exit_one_with_the_reason() {
    let ran = run_with(&["--port", "8124"], &|_| {
        Err(std::io::Error::other("taken"))
    });
    assert_eq!(ran.code, 1);
    assert_eq!(ran.err, "hello: cannot listen on 127.0.0.1:8124: taken\n");
}

/// Catches the real bind not being on loopback, or a port in use being bound twice and served.
#[test]
fn the_real_listener_is_on_loopback_and_a_taken_port_is_refused() {
    let listener = hello::bind_loopback(0).expect("a free port");
    let address = listener.local_addr().expect("the address");
    assert_eq!(
        (address.ip().is_loopback(), address.port() != 0),
        (true, true),
        "{address}"
    );

    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    let host = Host {
        runner: &runner,
        probe: &probe,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let port = address.port().to_string();
    let code = hello::run(&os(&["--port", &port]), &host, &mut out, &mut err);
    assert_eq!(code, 1, "{}", String::from_utf8_lossy(&err));
    let err = String::from_utf8(err).expect("text");
    assert!(
        err.starts_with(&format!("hello: cannot listen on 127.0.0.1:{port}: ")),
        "{err}"
    );
    assert!(hello::bind_loopback(address.port()).is_err());
}

/// Catches the binary not running the library's program: its version, its exit code for a bad
/// command line, and its exit code and last line for a hook step (75 is a byte the contract reads
/// as deferred).
#[test]
fn the_binary_runs_the_program() {
    let exe = env!("CARGO_BIN_EXE_hello");
    let version = Command::new(exe).arg("--version").output().expect("run");
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("hello {VERSION}\n")
    );

    let bad = Command::new(exe).arg("--nope").output().expect("run");
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).starts_with("hello: unknown argument"));

    // verify against a port nobody listens on: deferred, with a record that has cut over. It runs
    // no command and talks to loopback only.
    let dir = scratch("binary");
    let record = dir.path().join("record");
    std::fs::create_dir(&record).expect("the record directory");
    std::fs::write(record.join("cutover"), "").expect("cutover");
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("the address").port()
    };
    let mut params = hostkeeper_hook::Params::default();
    params.insert("units", hostkeeper_hook::Scalar::Text("systemd".to_owned()));
    params.insert("port", hostkeeper_hook::Scalar::Integer(i64::from(closed)));
    params.insert(
        "record_dir",
        hostkeeper_hook::Scalar::Text(record.display().to_string()),
    );
    params.insert(
        "updater.target_version",
        hostkeeper_hook::Scalar::Text("1.2.3".to_owned()),
    );
    let file = dir.path().join("params.toml");
    std::fs::write(&file, params.to_toml()).expect("params");
    for words in [["update-hook", "verify"], ["verify", "--params"]] {
        let mut command = Command::new(exe);
        command.args(words);
        if words[0] == "update-hook" {
            command.arg("--params");
        }
        let ran = command.arg(&file).output().expect("run");
        let stdout = String::from_utf8_lossy(&ran.stdout).into_owned();
        assert_eq!(ran.status.code(), Some(75), "{words:?}: {stdout}");
        assert_eq!(Outcome::from_exit(ran.status.code()), Outcome::Deferred);
        let report = Report::from_stdout(&stdout).expect("the line");
        assert_eq!(report.reason.as_deref(), Some("not_listening"));
        assert_eq!(report.retry_after_secs, Some(2));
    }
}
