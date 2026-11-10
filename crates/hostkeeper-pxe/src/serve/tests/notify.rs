//! The readiness notice: with `--notify-systemd`, `READY=1` is sent in one datagram to the
//! socket systemd named once the start line is logged; without the flag nothing is sent; a
//! start with the flag and no socket is refused before anything is bound; a socket that cannot
//! be written refuses the run.

use std::ffi::OsString;
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

use super::kit::{Kit, Running, client, refused_notifying};

/// The command line of a router-mode run of `kit`, with `extra` after it.
fn args(kit: &Kit, extra: &[&str]) -> Vec<OsString> {
    let sink = client();
    kit.args("router", sink.local_addr().expect("its address"), extra)
}

/// A datagram socket bound to an abstract name of this process and test (a socket path in the
/// scratch directory could be longer than a socket address allows), and the name as systemd
/// writes it: `@name`.
fn notify_socket(name: &str) -> (UnixDatagram, OsString) {
    use std::os::linux::net::SocketAddrExt;
    let name = format!("pxe-test-{}-{name}", std::process::id());
    let address = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
        .expect("an abstract address");
    let socket = UnixDatagram::bind_addr(&address).expect("a notify socket");
    // A guard against a run that never sends, not a bound on how long a send takes.
    socket
        .set_read_timeout(Some(Duration::from_secs(120)))
        .expect("a read guard");
    (socket, format!("@{name}").into())
}

/// With `--notify-systemd` the run sends `READY=1`, as one datagram, to the socket named, and
/// serves on; the start line is logged first.
///
/// Catches a `Type=notify` unit that never becomes ready, and one told it is ready before its
/// sockets are bound and its seal is checked.
#[test]
fn ready_is_sent_to_the_notify_socket_after_the_start() {
    let kit = Kit::new("notify-sent");
    let (socket, path) = notify_socket("sent");
    let running = Running::start_notifying(args(&kit, &["--notify-systemd"]), Some(path));
    let mut buffer = [0_u8; 64];
    let read = socket.recv(&mut buffer).expect("the notice");
    assert_eq!(&buffer[..read], b"READY=1");
    assert!(
        running
            .seen
            .iter()
            .any(|l| l.contains("started in router mode"))
    );
    let (result, _) = running.stop();
    assert_eq!(result, Ok(()));
}

/// Without `--notify-systemd` nothing is sent, even to a socket systemd named.
///
/// Catches a service that tells systemd it is ready when it was not asked to.
#[test]
fn nothing_is_sent_without_the_flag() {
    let kit = Kit::new("notify-flag-off");
    let (socket, path) = notify_socket("flag-off");
    socket.set_nonblocking(true).expect("nonblocking");
    let running = Running::start_notifying(args(&kit, &[]), Some(path));
    let (result, _) = running.stop();
    assert_eq!(result, Ok(()));
    let mut buffer = [0_u8; 64];
    let error = socket.recv(&mut buffer).expect_err("nothing was sent");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

/// The flag without a notify socket refuses the start before any file is read or socket bound:
/// one line, nothing logged.
///
/// Catches a `Type=notify` unit started where systemd gave no socket, which would hang until
/// systemd's start timeout.
#[test]
fn the_flag_without_a_socket_is_refused_before_anything_is_bound() {
    let kit = Kit::new("notify-none");
    let mut args = args(&kit, &["--notify-systemd"]);
    let at = args
        .iter()
        .position(|a| a == "--daemon-config")
        .expect("the flag");
    args[at + 1] = "/nonexistent/daemon.toml".into();
    let (result, lines) = refused_notifying(args, None);
    assert_eq!(
        result,
        Err(
            "--notify-systemd was given, but systemd set no notify socket (NOTIFY_SOCKET)"
                .to_owned()
        )
    );
    assert!(lines.is_empty());
}

/// A notify socket that cannot be written (no such path, an abstract name nobody listens on)
/// refuses the run with a line that names the socket and the error.
///
/// Catches a unit reported started that systemd was never told about.
#[test]
fn a_notice_that_cannot_be_sent_refuses_the_run() {
    let kit = Kit::new("notify-broken");
    let missing = kit.token.with_file_name("nobody.sock");
    let (result, lines) = refused_notifying(
        args(&kit, &["--notify-systemd"]),
        Some(missing.clone().into_os_string()),
    );
    let why = result.expect_err("refused");
    assert!(
        why.starts_with(&format!(
            "cannot tell systemd the start is complete, on {}: ",
            missing.display()
        )),
        "{why}"
    );
    assert!(
        lines.iter().any(|l| l.contains("started in router mode")),
        "{lines:?}"
    );
    let (result, _) = refused_notifying(
        args(&kit, &["--notify-systemd"]),
        Some("@pxe-test-nobody-listens".into()),
    );
    let why = result.expect_err("refused");
    assert!(
        why.starts_with("cannot tell systemd the start is complete, on @pxe-test-nobody-listens: "),
        "{why}"
    );
}
