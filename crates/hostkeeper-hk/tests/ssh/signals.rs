//! `hk ssh-proxy` as the process ssh runs as its `ProxyCommand`, stopped by a signal: ssh sends
//! SIGHUP to it when ssh exits. The built `hk` runs against the fake coordinator, which audits
//! each tunnel's close with the cause the coordinator's relay gives it (`fake::relay`).
//!
//! Each step waits for an explicit sign that the one before it happened (a byte relayed, the
//! host's answer to a `close`), never for a time.

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

use rustix::process::{Pid, Signal, kill_process};

use crate::fake::{HOST, PUBLIC, Rig, Tunnel};

/// What the coverage tool sets for a child, so that the child's coverage is counted; nothing
/// else of the test's environment reaches the program.
#[expect(
    clippy::disallowed_methods,
    reason = "the tests of a process pass on the variable the coverage tool names, and nothing else"
)]
fn coverage_env() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| name.to_string_lossy().starts_with("LLVM_PROFILE"))
        .collect()
}

/// A running `hk ssh-proxy` to [`HOST`] with the key `--identity` names, its stdin and stdout.
struct Proxy {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
}

impl Proxy {
    /// Starts the built `hk` as ssh starts its proxy: the coordinator's flags, then
    /// `ssh-proxy <host> --identity <key>`, with a home of its own and an empty `PATH` (with
    /// `--identity` the proxy runs no OpenSSH program).
    fn start(rig: &Rig) -> Proxy {
        let key = rig.dir().join("operator-key");
        std::fs::write(&key, "private").expect("the private key");
        std::fs::write(rig.dir().join("operator-key.pub"), format!("{PUBLIC} hk\n"))
            .expect("the public key");
        let (home, path) = (rig.dir().join("home"), rig.dir().join("empty-path"));
        for dir in [&home, &path] {
            std::fs::create_dir_all(dir).expect("a directory");
        }
        let coordinator = rig.coordinator();
        let child = Command::new(env!("CARGO_BIN_EXE_hk"))
            .arg("--endpoint")
            .arg(&coordinator.endpoint)
            .arg("--ca-file")
            .arg(&coordinator.ca_file)
            .arg("--token-file")
            .arg(&coordinator.token_file)
            .args(["ssh-proxy", HOST, "--identity"])
            .arg(&key)
            .env_clear()
            .envs(coverage_env())
            .env("HOME", &home)
            .env("PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("hk starts");
        let mut proxy = Proxy {
            stdin: None,
            stdout: None,
            child,
        };
        proxy.stdin = proxy.child.stdin.take();
        proxy.stdout = proxy.child.stdout.take();
        proxy
    }

    /// Writes `bytes` to the proxy's stdin.
    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin.write_all(bytes).expect("written");
        stdin.flush().expect("flushed");
    }

    /// The next `len` bytes of the proxy's stdout, waiting for them.
    fn receive(&mut self, len: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; len];
        self.stdout
            .as_mut()
            .expect("stdout is open")
            .read_exact(&mut bytes)
            .expect("the bytes relayed");
        bytes
    }

    /// A relayed round trip: `ping` reaches the host, whose echo reaches stdout. After it the
    /// tunnel is open and the proxy's signal handling is in place.
    fn ping(&mut self) {
        self.send(b"ping");
        assert_eq!(self.receive(4), b"ping");
    }

    fn signal(&self, signal: Signal) {
        let pid = Pid::from_child(&self.child);
        kill_process(pid, signal).expect("the signal is sent");
    }

    /// How the proxy ended, what else it wrote to stdout and what it wrote to stderr, after
    /// waiting for it to end by itself: its stdin stays open until then, so no end of stdin
    /// ends it.
    fn finish(mut self) -> (ExitStatus, Vec<u8>, String) {
        let mut rest = Vec::new();
        if let Some(mut stdout) = self.stdout.take() {
            stdout.read_to_end(&mut rest).expect("stdout");
        }
        let stdin = self.stdin.take();
        let output = self.child.wait_with_output().expect("hk ends");
        drop(stdin);
        let stderr = String::from_utf8(output.stderr).expect("text");
        (output.status, rest, stderr)
    }
}

fn queue(rig: &Rig, mode: Tunnel) {
    rig.script.tunnels.lock().expect("tunnels").push_back(mode);
}

/// Proves: a proxy that gets SIGHUP (what ssh sends its `ProxyCommand` when it exits), or
/// SIGTERM, in the middle of a session, while its stdin is still open, sends `close`, relays the
/// host's last bytes, exits 0 with nothing on stderr, and the tunnel is audited `host_closed`,
/// not `error`.
#[test]
fn a_proxy_stopped_by_sighup_or_sigterm_closes_its_tunnel_as_host_closed() {
    for signal in [Signal::HUP, Signal::TERM] {
        let rig = Rig::start();
        let mut proxy = Proxy::start(&rig);
        proxy.ping();
        proxy.signal(signal);
        let (status, rest, stderr) = proxy.finish();
        assert_eq!(
            (status.code(), rest.as_slice(), stderr.as_str()),
            (Some(0), &b"bye"[..], ""),
            "{signal:?}"
        );
        assert_eq!(rig.audited(), "host_closed", "{signal:?}");
        assert_eq!(rig.seen(|s| s.frames.concat()), b"ping", "{signal:?}");
    }
}

/// Proves: a proxy whose stdout is gone (ssh exited and closed its end of the pipe) while its
/// stdin is still open sends `close` at the first byte it cannot hand on, waits for the
/// coordinator to end the stream, exits 0, and the tunnel is audited `host_closed`, not `error`.
#[test]
fn a_proxy_whose_stdout_is_gone_closes_its_tunnel_as_host_closed() {
    let rig = Rig::start();
    let mut proxy = Proxy::start(&rig);
    proxy.ping();
    drop(proxy.stdout.take());
    proxy.send(b"more");
    let (status, _, stderr) = proxy.finish();
    assert_eq!((status.code(), stderr.as_str()), (Some(0), ""));
    assert_eq!(rig.audited(), "host_closed");
    assert_eq!(rig.seen(|s| s.frames.concat()), b"pingmore");
}

/// Proves: after a first signal the proxy sends exactly one `close` and waits for the host,
/// still relaying what the host sends; a SIGTERM then ends it at once, with exit code 1 and the
/// reason on stderr, and with no second `close` sent.
#[test]
fn sigterm_after_a_first_signal_ends_a_proxy_whose_host_has_not_closed() {
    let rig = Rig::start();
    queue(&rig, Tunnel::Linger);
    let mut proxy = Proxy::start(&rig);
    proxy.ping();
    proxy.signal(Signal::HUP);
    // The host's answer to the `close` the first signal sent.
    assert_eq!(proxy.receive(11), b"half-closed");
    proxy.signal(Signal::TERM);
    let (status, rest, stderr) = proxy.finish();
    assert_eq!(
        (status.code(), rest.as_slice(), stderr.as_str()),
        (
            Some(1),
            &b""[..],
            "hk: SIGTERM after a first signal ended the proxy before the tunnel closed\n"
        )
    );
    assert_eq!(rig.audited(), "error");
}

/// Proves: a second SIGHUP (a terminal that closes sends one to the proxy as well as ssh's) does
/// not end a proxy that is waiting for its host: the host's close still ends it with exit code 0
/// and the tunnel is audited `host_closed`, with one `close` sent.
#[test]
fn a_second_sighup_leaves_the_proxy_waiting_for_its_host() {
    let rig = Rig::start();
    queue(&rig, Tunnel::Linger);
    let mut proxy = Proxy::start(&rig);
    proxy.ping();
    proxy.signal(Signal::HUP);
    assert_eq!(proxy.receive(11), b"half-closed");
    proxy.signal(Signal::HUP);
    rig.script.release.notify_one();
    let (status, rest, stderr) = proxy.finish();
    assert_eq!(
        (status.code(), rest.as_slice(), stderr.as_str()),
        (Some(0), &b"bye"[..], "")
    );
    assert_eq!(rig.audited(), "host_closed");
}

/// Proves: a signal after the proxy's stdin has ended, when its `close` is already sent, sends no
/// second `close`: the host's close ends the proxy with exit code 0 and the tunnel is audited
/// `host_closed`.
#[test]
fn a_signal_after_end_of_stdin_sends_no_second_close() {
    let rig = Rig::start();
    queue(&rig, Tunnel::Linger);
    let mut proxy = Proxy::start(&rig);
    proxy.ping();
    drop(proxy.stdin.take());
    assert_eq!(proxy.receive(11), b"half-closed");
    proxy.signal(Signal::HUP);
    rig.script.release.notify_one();
    let (status, rest, stderr) = proxy.finish();
    assert_eq!(
        (status.code(), rest.as_slice(), stderr.as_str()),
        (Some(0), &b"bye"[..], "")
    );
    assert_eq!(rig.audited(), "host_closed");
}

/// Proves: a signal while the proxy waits for the coordinator's `opened` ends it with exit code 1
/// and the reason on stderr, instead of being held until the tunnel opens.
#[test]
fn a_signal_before_the_tunnel_opens_ends_the_proxy() {
    let rig = Rig::start();
    queue(&rig, Tunnel::HoldOpen);
    let proxy = Proxy::start(&rig);
    assert_eq!(rig.audited(), "open held");
    proxy.signal(Signal::HUP);
    let (status, rest, stderr) = proxy.finish();
    assert_eq!(
        (status.code(), rest.as_slice(), stderr.as_str()),
        (
            Some(1),
            &b""[..],
            "hk: a signal ended the proxy before the tunnel opened\n"
        )
    );
}
