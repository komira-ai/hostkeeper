//! The end-to-end rig of `tests/tierstore_shaped.rs`: the real coordinator and the real daemon
//! update a tierstore-shaped package under the real systemd of the host, from a release channel
//! the test serves on loopback, checked by the production attestation verifier against a trust
//! root the test makes.
//!
//! - [`channel`]: [`channel::ReleaseChannel`], the release channel, an HTTPS server on
//!   `127.0.0.1` and an ephemeral port that logs every request (the witness of "no fetch").
//! - [`der`] and [`signer`]: [`signer::FixtureSigner`], a Sigstore bundle for an archive digest
//!   and the trusted root `gh attestation verify` checks it against, offline.
//! - [`package`]: [`package::PackageBuilder`], the fixture package (`fixtures/tierstore-shaped/`
//!   and the program `tierstore-like`) built into a `.tar.zst` at test time.
//! - [`host_root`]: [`host_root::HostRoot`], the directories of one test on the host and the
//!   host's fixed paths the daemon uses, all removed when the test ends, failed or not; and
//!   [`host_root::DaemonUnit`], the daemon under a `Type=notify` unit.
//! - [`coordinator`]: [`coordinator::CoordinatorHandle`], the real coordinator in process, and
//!   `hostkeeper ctl` against it.
//! - [`scenario`]: the states the tests start from, each built the same way by every test.
//!
//! `tests/tierstore_rig.rs` runs the parts that need no root (the archive shape, the channel's
//! log) in the pull-request set.

#![allow(dead_code, reason = "each test file uses part of the end-to-end rig")]

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

pub mod channel;
pub mod coordinator;
pub mod der;
pub mod host_root;
pub mod package;
pub mod scenario;
pub mod signer;

/// The service of the fixture package; every unit of it starts with this name.
pub const SERVICE: &str = "hostkeeper-ci-tierstore";
/// The fixture program, at `bin/tierstore-like` in the package.
pub const BINARY_NAME: &str = "tierstore-like";
/// The template unit of the fixture's instances, `hostkeeper-ci-tierstore@<port>.service`.
pub const UNIT_TEMPLATE: &str = "hostkeeper-ci-tierstore@.service";
/// The fixture program as cargo built it.
pub const FIXTURE_BINARY: &str = env!("CARGO_BIN_EXE_tierstore-like");
/// The `hostkeeper` CLI (`daemon`, `ctl`), as cargo built it: `hostkeeper-device-daemon` only
/// forwards to `hostkeeper_daemon::cli::main`.
pub const HOSTKEEPER: &str = env!("CARGO_BIN_EXE_hostkeeper-device-daemon");

/// The organisation of the fixture's signer, the daemon config's `trusted_signers`.
pub const ORGANISATION: &str = "example-org";
/// The repository the fixture's attestation names.
pub const REPOSITORY: &str = "tierstore-like";
/// The workflow the fixture's attestation names, the daemon config's `trusted_signers`.
pub const WORKFLOW: &str = ".github/workflows/artifacts.yml";
/// The ref the fixture's attestation names.
pub const SOURCE_REF: &str = "refs/heads/main";
/// The commit the fixture's attestation names.
pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The role the test's host is approved with, the rollout file's `roles`.
pub const ROLE: &str = "daemon";
/// The host id the test approves its daemon as.
pub const HOST_ID: &str = "e2e-host";

/// The pause between two tries of [`wait_for`].
const PAUSE: Duration = Duration::from_millis(500);

/// The platform name of this build (`linux-x86_64`, ...), the one the manifest and the archive's
/// name carry.
pub fn platform() -> &'static str {
    hostkeeper_config::platform::Platform::current().as_str()
}

/// Tries `done` up to `tries` times, pausing between tries, until it holds; panics naming `what`
/// when it never does. A bound on the count of tries, so a lost state ends the test with its name
/// instead of a hung job; nothing is asserted about how long a state took.
pub fn wait_for(what: &str, tries: u32, mut done: impl FnMut() -> bool) {
    for _ in 0..tries {
        if done() {
            return;
        }
        thread::sleep(PAUSE);
    }
    panic!("{what}: not reached after {tries} tries");
}

/// An unused loopback port: one the system gave to a listener on port 0, released at once.
pub fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .expect("a loopback port")
}

/// `GET <path>` over plain HTTP/1.1 to `127.0.0.1:<port>`: the status and the body, or `None`
/// when nothing answers there.
pub fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).ok()?;
    let reply = String::from_utf8(reply).ok()?;
    let (head, body) = reply.split_once("\r\n\r\n")?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, body.to_owned()))
}

/// What the fixture instance on `port` serves at `/healthz`: `version=<version>`, or `None`
/// when it does not answer 200.
pub fn served_version(port: u16) -> Option<String> {
    match http_get(port, "/healthz") {
        Some((200, body)) => body.trim().strip_prefix("version=").map(str::to_owned),
        _ => None,
    }
}

/// The instance unit of the fixture on `port`.
pub fn instance(port: u16) -> String {
    format!("{SERVICE}@{port}.service")
}
