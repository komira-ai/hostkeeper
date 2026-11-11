//! The start of `serve`: each file, socket, drop and read it depends on refuses the start with
//! one line that names what is wrong, before anything is served; a start that succeeds says how
//! it runs in its first line, and a stop says so in its last.

use std::ffi::OsString;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use clap::Parser as _;

use super::kit::{Kit, NOW, client, refused};
use crate::cli::{Cli, Verb};
use crate::serve::{ServePorts, serve_with};

/// The command line of a proxy-mode run of `kit`, with `extra` after it.
fn args(kit: &Kit, extra: &[&str]) -> Vec<OsString> {
    let sink = client();
    kit.args("proxy", sink.local_addr().expect("its address"), extra)
}

/// The command line of `kit` with the value of `flag` replaced by `value`.
fn with(kit: &Kit, flag: &str, value: &str) -> Vec<OsString> {
    let mut args = args(kit, &[]);
    let at = args.iter().position(|a| a == flag).expect("the flag");
    args[at + 1] = value.into();
    args
}

/// The refusal of `args`: its error, once no line was logged.
fn refusal(args: Vec<OsString>) -> String {
    let (result, lines) = refused(args);
    assert!(lines.is_empty(), "{lines:?}");
    result.expect_err("refused")
}

/// Writes `bytes` at `path` with `mode`.
fn write(path: &Path, bytes: &[u8], mode: u32) {
    std::fs::write(path, bytes).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// A sealed file that differs from its seal by one byte refuses the start, naming the
/// directory, the file and both digests.
///
/// Catches a tampered or stale boot set that is served.
#[test]
fn a_seal_mismatch_refuses_the_start_naming_the_file() {
    let kit = Kit::new("start-seal");
    let deb = kit.dir.join("deb/hostkeeper_amd64.deb");
    let mut bytes = std::fs::read(&deb).expect("the deb");
    bytes[0] ^= 1;
    std::fs::write(&deb, &bytes).expect("write");
    let why = refusal(args(&kit, &[]));
    let prefix = format!(
        "{}: deb/hostkeeper_amd64.deb: its SHA-256 is {}, and the seal says ",
        kit.dir.display(),
        crate::testing::sha256_hex(&bytes)
    );
    assert!(why.starts_with(&prefix), "{why}");
}

/// The files read as root are refused, each naming its path and the rule: a daemon config the
/// daemon's parser refuses, one the group may write, one that is not UTF-8, one owned by
/// another user than the trusted one; an SSH keys file holding a private key; a CA file that is
/// not there.
///
/// Catches a seed made from a config the daemon would refuse on twenty machines, and a private
/// key copied into every seed.
#[test]
fn the_files_read_at_the_start_are_refused_with_their_rule() {
    let kit = Kit::new("start-files");
    let config = kit.config.display().to_string();
    write(&kit.config, b"nonsense = 1\n", 0o644);
    let why = refusal(args(&kit, &[]));
    assert!(why.starts_with(&format!("{config}: ")), "{why}");
    assert!(why.contains("nonsense"), "{why}");
    write(&kit.config, super::kit::DAEMON_CONFIG.as_bytes(), 0o664);
    assert_eq!(
        refusal(args(&kit, &[])),
        format!("{config}: mode 664 lets the group or others write it")
    );
    write(&kit.config, &[0xff, 0xfe], 0o644);
    assert_eq!(refusal(args(&kit, &[])), format!("{config}: not UTF-8"));
    write(&kit.config, super::kit::DAEMON_CONFIG.as_bytes(), 0o644);
    let me = rustix::process::getuid().as_raw();
    let Verb::Serve(flags) = Cli::try_parse_from(args(&kit, &[])).expect("parses").verb else {
        panic!("serve")
    };
    let ports = ServePorts {
        now: Arc::new(|| NOW),
        sink: Arc::new(|line| panic!("logged {line}")),
        stop: Box::new(|| Box::pin(async {})),
        notify_socket: None,
    };
    assert_eq!(
        serve_with(&flags, me + 1, ports),
        Err(format!(
            "{config}: owned by uid {me}, not the trusted uid {}",
            me + 1
        ))
    );

    let keys = kit.keys.display().to_string();
    write(
        &kit.keys,
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
        0o644,
    );
    assert_eq!(
        refusal(args(&kit, &[])),
        format!(
            "{keys}: the SSH keys file holds a private key at line 1; it must hold public keys only"
        )
    );
    let good_keys = std::fs::read_to_string(Kit::new("start-files-keys").keys).expect("keys");
    write(&kit.keys, good_keys.as_bytes(), 0o644);
    let missing = kit.scratch.path().join("missing-ca.pem");
    let why = refusal(with(&kit, "--ca-file", &missing.display().to_string()));
    assert_eq!(
        why,
        format!(
            "{}: No such file or directory (os error 2)",
            missing.display()
        )
    );
}

/// The token file is checked when it is opened, as root, and read after the drop: a file the
/// group or others may read is refused at the open, a file too short at the read; neither error
/// shows the token or the path.
///
/// Catches a token readable by every user of the host, and one read before the drop.
#[test]
fn the_token_file_is_held_to_the_secret_file_rule() {
    let kit = Kit::new("start-token");
    write(
        &kit.token,
        format!("{}\n", super::kit::TOKEN).as_bytes(),
        0o644,
    );
    assert_eq!(
        refusal(args(&kit, &[])),
        "--token-file: its mode must be 0600 or 0400"
    );
    write(&kit.token, b"short\n", 0o600);
    assert_eq!(
        refusal(args(&kit, &[])),
        "--token-file: it must hold one token of 32 to 4096 visible ASCII characters"
    );
}

/// The drop refuses root as the user to drop to, and a group the starting user cannot take.
///
/// Catches a `serve` that parses packets as root.
#[test]
fn the_drop_refuses_root_and_a_group_it_cannot_take() {
    let kit = Kit::new("start-drop");
    assert_eq!(
        refusal(with(&kit, "--user-id", "0")),
        "--user-id and --group-id must name an unprivileged user and group, not root (0)"
    );
    let other = rustix::process::getgid().as_raw().wrapping_add(1).max(1);
    let why = refusal(with(&kit, "--group-id", &other.to_string()));
    assert!(
        why.starts_with(&format!("cannot set the group to {other}: ")),
        "{why}"
    );
}

/// A port already taken refuses the start naming the socket and the address; an address that
/// names no one host, which the menus and seeds would carry, refuses it too.
///
/// Catches a run that serves nothing on one protocol while the others answer, and menus that
/// name 0.0.0.0.
#[test]
fn a_taken_port_or_an_unusable_address_refuses_the_start() {
    let kit = Kit::new("start-sockets");
    let taken = TcpListener::bind("127.0.0.1:0").expect("a listener");
    let address = taken.local_addr().expect("its address").to_string();
    let why = refusal(with(&kit, "--http-listen", &address));
    assert!(
        why.starts_with(&format!("cannot bind the HTTP socket to {address}: ")),
        "{why}"
    );
    assert_eq!(
        refusal(with(&kit, "--address", "0.0.0.0")),
        "--address 0.0.0.0: the boot server's address 0.0.0.0 names no one host"
    );
}

/// A coordinator that does not answer at the start refuses it, naming the call; so does one
/// whose certificate does not chain to `--ca-file` (another CA's), which is sent nothing; a
/// `--reinstall` of a host id no record has refuses it.
///
/// Catches a run started against a wrong address or token that then answers nothing for
/// hours, a token sent to a coordinator the CA does not vouch for, and a reinstall that names a
/// host it can never install.
#[test]
fn the_coordinator_is_read_once_at_the_start() {
    let kit = Kit::new("start-coordinator");
    let closed = TcpListener::bind("127.0.0.1:0").expect("a listener");
    let port = closed.local_addr().expect("its address").port();
    drop(closed);
    let (result, lines) = refused(with(
        &kit,
        "--coordinator",
        &format!("https://127.0.0.1:{port}"),
    ));
    let why = result.expect_err("refused");
    assert!(
        why.starts_with("the coordinator could not be read: Enrollment.ListRecords: Unavailable: "),
        "{why}"
    );
    assert!(lines.is_empty(), "{lines:?}");
    let stranger =
        super::fake::Coordinator::start(&super::fake::pki(), super::fake::Fake::default());
    assert_eq!(
        std::fs::read_to_string(&kit.ca).expect("the CA file"),
        kit.pki.ca
    );
    let why = refusal(with(&kit, "--coordinator", &stranger.endpoint()));
    assert!(
        why.starts_with("the coordinator could not be read: Enrollment.ListRecords: "),
        "{why}"
    );
    assert!(
        stranger.fake.get(|s| s.bearers.is_empty()),
        "the token was sent"
    );
    assert_eq!(
        refusal(args(&kit, &["--reinstall", "nobody-1"])),
        "--reinstall nobody-1: no inventory record has that host id"
    );
}

/// A run that starts says, in its first line, the mode, the user and group it dropped to, the
/// address it answers as, the release and platforms of its boot sets and where each socket is
/// bound; then a line for a platform of the seal it does not serve, one for each
/// `--reinstall`, and what the coordinator holds, read with the operator token as the bearer of
/// each of its three calls. Its stop is its last line. `--interface lo` binds every socket to
/// that interface.
///
/// Catches a start that does not say which boot sets it serves, or as whom it runs.
#[test]
fn a_start_says_how_it_runs_and_a_stop_says_it_stopped() {
    let kit = Kit::new("start-lines");
    let seal_path = kit.dir.join("seal.json");
    let mut seal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seal_path).expect("the seal"))
            .expect("JSON");
    let x86 = seal["platforms"]["linux-x86_64"].clone();
    seal["platforms"]["macos-arm64"] = x86;
    std::fs::write(&seal_path, seal.to_string()).expect("write");
    kit.fake()
        .set(|s| s.pending = vec![super::fake::pending("r-1", "old-1")]);
    let run = kit.start(args(&kit, &["--reinstall", "new-1", "--interface", "lo"]));
    let (dhcp, proxy, tftp, http) = (run.dhcp, run.proxy.expect("proxy"), run.tftp, run.http);
    let (result, lines) = run.stop();
    assert_eq!(result, Ok(()));
    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();
    assert_eq!(
        lines,
        [
            format!(
                "{NOW} started in proxy mode as uid {uid} gid {gid}, answering as 127.0.0.1: the \
                 boot sets of hostkeeper 1.2.3 for linux-x86_64 and linux-arm64; DHCP {dhcp}, \
                 proxyDHCP {proxy}, TFTP {tftp}, HTTP {http}"
            ),
            format!(
                "{NOW} the seal names the platform \"macos-arm64\", which this release does not \
                 serve: it is not served"
            ),
            format!(
                "{NOW} --reinstall new-1: its record is installed even while a host or a pending \
                 request holds it"
            ),
            format!(
                "{NOW} the coordinator holds 2 records and 1 pending requests; 1 MACs \
                 are in the answer set"
            ),
            format!("{NOW} stopped"),
        ]
    );
    let bearers = kit.fake().get(|s| s.bearers.clone());
    assert_eq!(bearers, vec![format!("Bearer {}", super::kit::TOKEN); 2]);
}

/// Router mode binds no proxyDHCP socket, and says so.
///
/// Catches a router-mode run that holds UDP 4011.
#[test]
fn router_mode_binds_no_proxy_socket() {
    let kit = Kit::new("start-router");
    let sink = client();
    let run = kit.start(kit.args("router", sink.local_addr().expect("its address"), &[]));
    assert_eq!(run.proxy, None);
    let (result, lines) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(
        lines[0].contains("started in router mode") && lines[0].contains(", proxyDHCP not bound,"),
        "{lines:?}"
    );
}

/// The host's ports stamp the log by the system clock, which is past the first release of
/// network install.
///
/// Catches a log stamped with a fixed or zero time on a host.
#[test]
fn the_host_ports_stamp_by_the_system_clock() {
    let ports = ServePorts::host(None);
    assert!((ports.now)() > 1_788_220_800);
    (ports.sink)("a line of the host ports' test");
}
