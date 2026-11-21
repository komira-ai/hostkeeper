//! `hk tailnet-wait` through `run`: its flags and their usage errors, the real probe against a
//! loopback server of port 0 that hangs up or answers, the waits recorded by the world's clock
//! (which returns at once), and the rejoin through a stand-in `tailscale` (the test binary
//! `hk-fake-mac-tool` under that name) that records its arguments, says something on stderr,
//! exits non-zero or never ends, as each test sets it up.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_hk::tailnet_wait::Tailnet;

use crate::rig::{Opts, Ran, Reply, Rig, Server, args, closed_port};

/// A test's directory with the stand-in `tailscale` in `bin`.
struct Wait {
    rig: Rig,
    tailscale: PathBuf,
}

impl Wait {
    fn new(name: &str) -> Wait {
        let rig = Rig::new(&format!("tailnet-wait-{name}"));
        let bin = rig.path("bin");
        std::fs::create_dir(&bin).expect("the bin directory");
        let tailscale = bin.join("tailscale");
        // A link, not a copy: a copy is a file open for writing, which a process another test
        // forks meanwhile could hold when this one runs it.
        std::fs::hard_link(env!("CARGO_BIN_EXE_hk-fake-mac-tool"), &tailscale).expect("a stand-in");
        Wait { rig, tailscale }
    }

    /// Writes `name` beside the stand-in.
    fn tool(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.rig.path("bin").join(name), bytes).expect("written");
    }

    /// The arguments the stand-in was run with, one run a line; empty when it never ran.
    fn ran(&self) -> String {
        std::fs::read_to_string(self.rig.path("bin").join("tailscale.args")).unwrap_or_default()
    }

    /// `hk --endpoint https://127.0.0.1:<port> --ca-file <ca> tailnet-wait <words>` with the
    /// stand-in, `limit` for each of its runs, and a clock that records each wait in `waits`.
    fn run(
        &self,
        port: u16,
        words: &[&str],
        limit: Duration,
        waits: &RefCell<Vec<Duration>>,
    ) -> Ran {
        let wait = |time: Duration| waits.borrow_mut().push(time);
        let tailnet = Tailnet {
            tailscale: self.tailscale.clone(),
            limit,
            wait: &wait,
        };
        let mut all = vec![
            "--endpoint".to_owned(),
            format!("https://127.0.0.1:{port}"),
            "--ca-file".to_owned(),
            self.rig.path("ca.pem").display().to_string(),
            "tailnet-wait".to_owned(),
        ];
        all.extend(args(words));
        self.rig.run(Opts::real().tailnet(&tailnet), &all)
    }
}

/// A limit no run of the stand-in that ends by itself comes near.
const AMPLE: Duration = Duration::from_secs(3600);

/// The lines of `text`, each with what follows its last `: ` cut off: the transport's own words
/// for why there was no answer, which these tests do not hold to.
fn heads(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| match l.rsplit_once(": no answer: ") {
            Some((head, _)) => format!("{head}: no answer: _"),
            None => l.to_owned(),
        })
        .collect()
}

/// Proves: hk asks `GET /v1/version` with no token, trusting the CA file, until the third
/// request answers; it waits the interval twice, says why each of the first two had no answer,
/// and prints one line naming the third attempt; tailscale never runs.
#[test]
fn the_third_request_answered_ends_the_wait_green() {
    let wait = Wait::new("third");
    let server = Server::sequence(
        &wait.rig.pki,
        vec![Reply::hang_up(), Reply::hang_up(), Reply::new(200, "{}")],
    );
    let waits = RefCell::new(Vec::new());
    let ran = wait.run(server.port, &["--interval-secs", "7"], AMPLE, &waits);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        ran.stdout,
        "the coordinator answered (HTTP 200) on attempt 3 of 6 of round 1, after the join\n"
    );
    assert_eq!(
        heads(&ran.stderr),
        [
            "hk: tailnet-wait: round 1, attempt 1 of 6: no answer: _",
            "hk: tailnet-wait: round 1, attempt 2 of 6: no answer: _",
        ]
    );
    assert_eq!(*waits.borrow(), [Duration::from_secs(7); 2]);
    let seen = server.seen();
    assert_eq!(seen.len(), 3);
    for request in &seen {
        assert_eq!(
            (request.method.as_str(), request.target.as_str()),
            ("GET", "/v1/version")
        );
        assert_eq!(request.header("authorization"), None);
    }
    assert_eq!(wait.ran(), "");
}

/// Proves: when round 1 has no answer, hk runs `tailscale down`, then `tailscale up` with no
/// other argument, once, and the answer of round 2 ends the wait green, saying so.
#[test]
fn a_rejoin_after_a_silent_round_then_an_answer_is_green() {
    let wait = Wait::new("rejoin");
    let server = Server::sequence(
        &wait.rig.pki,
        vec![Reply::hang_up(), Reply::hang_up(), Reply::new(200, "{}")],
    );
    let waits = RefCell::new(Vec::new());
    let ran = wait.run(server.port, &["--attempts", "2"], AMPLE, &waits);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        ran.stdout,
        "the coordinator answered (HTTP 200) on attempt 1 of 2 of round 2, after the rejoin (round 1 had no answer in 2 attempts)\n"
    );
    assert_eq!(wait.ran(), "down\nup\n");
    // The default interval, once in round 1.
    assert_eq!(*waits.borrow(), [Duration::from_secs(10)]);
    assert_eq!(
        heads(&ran.stderr),
        [
            "hk: tailnet-wait: round 1, attempt 1 of 2: no answer: _",
            "hk: tailnet-wait: round 1, attempt 2 of 2: no answer: _",
            "hk: tailnet-wait: no answer in the 2 attempts of round 1; rejoining the tailnet (tailscale down, then tailscale up)",
        ]
    );
}

/// Proves: a coordinator that never answers is exit 1 after both rounds and one rejoin, the
/// last line naming both rounds.
#[test]
fn no_answer_in_either_round_is_red() {
    let wait = Wait::new("never");
    let waits = RefCell::new(Vec::new());
    let ran = wait.run(closed_port(), &["--attempts", "1"], AMPLE, &waits);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(ran.stdout, "");
    assert_eq!(wait.ran(), "down\nup\n");
    let last = ran.stderr.lines().last().unwrap_or_default();
    assert!(
        last.starts_with(
            "hk: the coordinator did not answer: round 1, after the join: no answer in 1 attempts, the last: "
        ),
        "{last}"
    );
    assert!(
        last.contains("; round 2, after the rejoin: no answer in 1 attempts, the last: "),
        "{last}"
    );
    assert!(waits.borrow().is_empty());
}

/// Proves: a `tailscale up` that exits non-zero fails the rejoin, naming the command, its exit
/// and the last line it said; round 2 does not run.
#[test]
fn a_tailscale_up_that_fails_ends_the_wait_red() {
    let wait = Wait::new("up-fails");
    wait.tool("tailscale.up.exit", b"1\n");
    wait.tool(
        "tailscale.up.err",
        b"some detail\nAccess denied: prefs write access denied\n",
    );
    let waits = RefCell::new(Vec::new());
    let port = closed_port();
    let ran = wait.run(port, &["--attempts", "1"], AMPLE, &waits);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(wait.ran(), "down\nup\n");
    let last = ran.stderr.lines().last().unwrap_or_default();
    let tailscale = wait.tailscale.display();
    assert!(
        last.ends_with(&format!(
            "; the rejoin failed: {tailscale} up ended with exit status: 1: Access denied: prefs write access denied; round 2 did not run"
        )),
        "{last}"
    );
}

/// Proves: a `tailscale down` that never ends is stopped at the limit, which fails the rejoin
/// and says so; `tailscale up` is not run.
#[test]
fn a_tailscale_that_never_ends_is_stopped() {
    let wait = Wait::new("hangs");
    wait.tool("tailscale.down.hang", b"");
    wait.tool("tailscale.down.err", b"stopping\n");
    let waits = RefCell::new(Vec::new());
    let ran = wait.run(
        closed_port(),
        &["--attempts", "1"],
        Duration::from_millis(200),
        &waits,
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(wait.ran(), "down\n");
    let last = ran.stderr.lines().last().unwrap_or_default();
    let tailscale = wait.tailscale.display();
    assert!(
        last.ends_with(&format!(
            "; the rejoin failed: {tailscale} down did not end within 0.2 seconds and was stopped: stopping; round 2 did not run"
        )),
        "{last}"
    );
}

/// Proves: a `tailscale` that cannot be started fails the rejoin, naming it.
#[test]
fn a_tailscale_that_cannot_run_ends_the_wait_red() {
    let rig = Rig::new("tailnet-wait-missing");
    let mut all = vec![
        "--endpoint".to_owned(),
        format!("https://127.0.0.1:{}", closed_port()),
        "--ca-file".to_owned(),
        rig.path("ca.pem").display().to_string(),
    ];
    all.extend(args(&["tailnet-wait", "--attempts=1", "--interval-secs=0"]));
    // The rig's default tailnet: a `tailscale` that does not exist.
    let ran = rig.hk(&all);
    assert_eq!(ran.code, 1, "{ran:?}");
    let missing = Path::new(&rig.path("no-tailscale")).display().to_string();
    let last = ran.stderr.lines().last().unwrap_or_default();
    assert!(
        last.contains(&format!("; the rejoin failed: cannot run {missing} down (")),
        "{last}"
    );
}

/// Proves: `--help` prints the command's help, wherever it stands, and runs nothing.
#[test]
fn help_prints_the_commands_help() {
    let rig = Rig::new("tailnet-wait-help");
    for words in [
        &["tailnet-wait", "--help"][..],
        &["tailnet-wait", "--attempts", "3", "-h"],
    ] {
        let ran = rig.hk(&args(words));
        assert_eq!(ran.code, 0);
        assert!(
            ran.stdout
                .starts_with("hk tailnet-wait [--attempts N] [--interval-secs S]\n"),
            "{}",
            ran.stdout
        );
        assert_eq!(ran.stderr, "");
    }
}

/// Proves: every mistake in the command line is a usage error, exit 2, said in one line, before
/// any request or program runs.
#[test]
fn a_mistaken_command_line_is_a_usage_error() {
    let wait = Wait::new("usage");
    let ca = wait.rig.path("ca.pem").display().to_string();
    let empty = wait.rig.put("empty.pem", b"no certificate here\n", 0o644);
    let token = wait.rig.path("token").display().to_string();
    let missing = wait.rig.path("missing.pem").display().to_string();
    let good = |more: &[&str]| -> Vec<String> {
        let mut all = args(&["--endpoint", "https://127.0.0.1:1", "--ca-file", &ca]);
        all.extend(args(more));
        all
    };
    let cases: Vec<(Vec<String>, String)> = vec![
        (
            args(&["tailnet-wait"]),
            "hk: hk tailnet-wait needs --endpoint and --ca-file".to_owned(),
        ),
        (
            args(&["--endpoint", "https://127.0.0.1:1", "tailnet-wait"]),
            "hk: hk tailnet-wait needs --endpoint and --ca-file".to_owned(),
        ),
        (
            args(&["--endpoint", "http://127.0.0.1:1", "--ca-file", &ca, "tailnet-wait"]),
            "hk: --endpoint takes https://<host>[:<port>]".to_owned(),
        ),
        (
            args(&["--endpoint", "https://127.0.0.1:1", "--ca-file", &missing, "tailnet-wait"]),
            format!("hk: cannot read --ca-file {missing}"),
        ),
        (
            args(&["--endpoint", "https://127.0.0.1:1", "--ca-file", &empty, "tailnet-wait"]),
            format!("hk: --ca-file {empty} holds no certificate"),
        ),
        (
            good(&["tailnet-wait", "--token-file", &token]),
            "hk: hk tailnet-wait takes no token and writes no answer: --token-file, --token-stdin, --github-oidc-audience and --response-file are not for it".to_owned(),
        ),
        (
            good(&["--token-stdin", "tailnet-wait"]),
            "hk: hk tailnet-wait takes no token and writes no answer: --token-file, --token-stdin, --github-oidc-audience and --response-file are not for it".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--github-oidc-audience", "a"]),
            "hk: hk tailnet-wait takes no token and writes no answer: --token-file, --token-stdin, --github-oidc-audience and --response-file are not for it".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--response-file", "out.json"]),
            "hk: hk tailnet-wait takes no token and writes no answer: --token-file, --token-stdin, --github-oidc-audience and --response-file are not for it".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--attempts", "0"]),
            "hk: --attempts takes a whole number from 1 to 100".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--attempts=101"]),
            "hk: --attempts takes a whole number from 1 to 100".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--attempts", "two"]),
            "hk: --attempts takes a whole number from 1 to 100".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--attempts"]),
            "hk: --attempts needs a value".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--attempts", "2", "--attempts", "3"]),
            "hk: --attempts is given twice".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--interval-secs", "601"]),
            "hk: --interval-secs takes a whole number from 0 to 600".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--interval-secs=1", "--interval-secs=1"]),
            "hk: --interval-secs is given twice".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--ping"]),
            "hk: hk tailnet-wait has no flag --ping".to_owned(),
        ),
        (
            good(&["tailnet-wait", "coordinator"]),
            "hk: hk tailnet-wait takes no argument: coordinator".to_owned(),
        ),
        (
            good(&["tailnet-wait", "--timeout-secs", "0"]),
            "hk: --timeout-secs takes a whole number of seconds, 1 to 999999".to_owned(),
        ),
    ];
    for (all, want) in cases {
        let ran = wait.rig.hk(&all);
        assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{all:?}: {ran:?}");
        assert_eq!(ran.stderr, format!("{want}\n"), "{all:?}");
    }
    assert_eq!(wait.ran(), "");
}

/// Proves: `--timeout-secs` and an endpoint with a `/` at its end are taken: the request is still
/// `GET /v1/version`.
#[test]
fn the_endpoints_slash_and_the_timeout_are_taken() {
    let wait = Wait::new("slash");
    let server = Server::canned(&wait.rig.pki, 404, "{}");
    let all = args(&[
        "--endpoint",
        &format!("https://127.0.0.1:{}/", server.port),
        "--ca-file",
        &wait.rig.path("ca.pem").display().to_string(),
        "tailnet-wait",
        "--timeout-secs",
        "5",
    ]);
    let ran = wait.rig.hk(&all);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        ran.stdout,
        "the coordinator answered (HTTP 404) on attempt 1 of 6 of round 1, after the join\n"
    );
    assert_eq!(server.seen()[0].target, "/v1/version");
}
