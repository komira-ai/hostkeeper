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
use support::{FakeProbe, FakeRunner, scratch};

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

/// Catches the server path not binding the port it was given, not answering from the version and
/// the `--fail-ready` flag it was given, and not ending in exit 1 with its reason when it stops.
#[test]
fn the_server_path_serves_on_its_listener_until_it_stops() {
    for (flag, want) in [
        (None, "HTTP/1.1 200 OK\r\n"),
        (Some("--fail-ready"), "HTTP/1.1 503 "),
    ] {
        let asked = Cell::new(0u16);
        let client = Cell::new(None);
        let listen = |port: u16| {
            asked.set(port);
            let listener = TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            let mut stream = TcpStream::connect(listener.local_addr()?)?;
            stream.write_all(b"GET /readyz HTTP/1.1\r\n\r\n")?;
            client.set(Some(stream));
            Ok(listener)
        };
        let mut words = vec!["--port", "8123"];
        words.extend(flag);
        let ran = run_with(&words, &listen);
        assert_eq!(asked.get(), 8123);
        assert_eq!(ran.code, 1);
        assert_eq!(ran.out, "");
        assert!(
            ran.err
                .contains(&format!("hello {VERSION}: serving on 127.0.0.1:8123\n")),
            "{}",
            ran.err
        );
        assert!(ran.err.contains("hello: stopped serving: "), "{}", ran.err);
        let mut answer = String::new();
        client
            .take()
            .expect("a client")
            .read_to_string(&mut answer)
            .expect("read");
        assert!(answer.starts_with(want), "{flag:?}: {answer:?}");
    }
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
