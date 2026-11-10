//! The end-to-end rig of `tests/tierstore_shaped.rs`: the real coordinator and the real daemon
//! update a tierstore-shaped package under the real systemd of the host, from a signed apt
//! repository the test builds on disk, through the `apt` entry of each version.
//!
//! - [`aptrepo`]: [`aptrepo::AptRepo`], a signed apt repository of fixture `.deb`s and the
//!   host's entry of it, and [`aptrepo::entry`], the `apt` entry of an order.
//! - [`der`] and [`signer`]: [`signer::FixtureSigner`], a Sigstore bundle for an artifact digest
//!   and the trusted root `gh attestation verify` checks it against, offline: what the
//!   network-install rig's `hostkeeper-pxe` checks a netboot with, and what the cross-release
//!   rig's released daemon finds at the fixed places of the host.
//! - [`package`]: [`package::PackageBuilder`], the fixture package (`fixtures/tierstore-shaped/`
//!   and the program `tierstore-like`) built into a `.deb` and its entry at test time.
//! - [`host_root`]: [`host_root::HostRoot`], the directories of one test on the host and the
//!   host's fixed paths the daemon uses, all removed when the test ends, failed or not; and
//!   [`host_root::DaemonUnit`], the daemon under a `Type=notify` unit.
//! - [`coordinator`]: [`coordinator::CoordinatorHandle`], the real coordinator in process, and
//!   the operator's calls against it ([`operator_api`], shared with `support::pair`).
//! - [`scenario`]: the states the tests start from, each built the same way by every test.
//! - On a Mac, `tests/device_mac_e2e.rs` runs hello's `.pkg` through the same coordinator and the
//!   daemon under launchd: `hello` builds the package and its entry (`pkg_build` is the Mac's
//!   `pkgbuild` and `productbuild`, `macos_pkg` the files the daemon's package port serves and
//!   the answer to its signature check), and `mac_host` is the Mac's directories and the
//!   daemon's job.
//!
//! `tests/tierstore_rig.rs` runs the parts that need no root (the package's shape against the
//! daemon's tree rules and manifest checks, the held ports) in the pull-request set.

#![allow(dead_code, reason = "each test file uses part of the end-to-end rig")]

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

#[allow(
    clippy::duplicate_mod,
    reason = "the fixture packages are shared by the two harnesses, which a test file may use both of"
)]
#[path = "../shared/aptrepo.rs"]
pub mod aptrepo;
pub mod coordinator;
pub mod der;
pub mod host_root;
#[allow(
    clippy::duplicate_mod,
    reason = "the operator's calls are shared by the two harnesses, which a test file may use both of"
)]
#[path = "../shared/operator_api.rs"]
pub mod operator_api;
pub mod package;
pub mod scenario;
pub mod signer;

// The Mac's end-to-end rig (`tests/device_mac_e2e.rs`): hello's `.pkg`, the Mac the daemon runs
// on as a launchd job, and the `.pkg` files and signature answers the program serves.
#[cfg(target_os = "macos")]
pub mod hello;
#[cfg(target_os = "macos")]
pub mod mac_host;
#[cfg(target_os = "macos")]
#[path = "../shared/macos_pkg.rs"]
pub mod macos_pkg;
#[cfg(target_os = "macos")]
#[path = "../shared/pkg_build.rs"]
pub mod pkg_build;

/// The service of the fixture package, which is also its package; every unit of it starts with
/// this name.
pub const SERVICE: &str = "hostkeeper-ci-tierstore";
/// The channel of the fixture packages' repository, `<registry name>-<suite>`: the name of the
/// host's repository entry and the `repository` of the entries an order carries. The registry
/// name is [`aptrepo::REGISTRY_NAME`], which the coordinator's rollout file names for the service.
pub const REPOSITORY_NAME: &str = aptrepo::REPOSITORY;
/// The fixture program, at `usr/bin/tierstore-like` in the package.
pub const BINARY_NAME: &str = "tierstore-like";
/// The template unit of the fixture's instances, `hostkeeper-ci-tierstore@<port>.service`.
pub const UNIT_TEMPLATE: &str = "hostkeeper-ci-tierstore@.service";
/// The fixture program as cargo built it.
pub const FIXTURE_BINARY: &str = env!("CARGO_BIN_EXE_tierstore-like");
/// The `hostkeeper` CLI (`daemon`), as cargo built it: `hostkeeper-device-daemon` only
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

/// An unused loopback port: one the system gave to a listener on port 0, released at once. For a
/// port nothing binds while the test runs (a package built only to be read); a port a unit of the
/// test listens on later is a [`HeldPort`], since a released port is the system's to give again.
pub fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .expect("a loopback port")
}

/// A loopback port held for as long as this value lives: a TCP socket bound to `127.0.0.1:<port>`
/// with `SO_REUSEADDR` that never listens, so it answers nothing.
///
/// Linux gives a held port to no one else: not to a `bind` to port 0 (the coordinator's
/// listeners take theirs that way), not to a `connect`, and not to an
/// explicit `bind` without `SO_REUSEADDR`. An explicit `bind` with `SO_REUSEADDR` and a `listen`
/// on it succeed, again and again: the fixture's `serve` (`std::net::TcpListener::bind`, which
/// sets it) takes the port each time a unit of it starts, and gives it back each time it stops.
/// A port of [`free_port`] is free only until the next `bind` to port 0 takes it.
pub struct HeldPort {
    socket: socket2::Socket,
    port: u16,
}

impl HeldPort {
    /// A port the system picks, held.
    pub fn new() -> HeldPort {
        use socket2::{Domain, Protocol, SockAddr, Socket, Type};
        let socket =
            Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).expect("a TCP socket");
        socket
            .set_reuse_address(true)
            .expect("SO_REUSEADDR on the held socket");
        socket
            .bind(&SockAddr::from(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))))
            .expect("a loopback port");
        let port = socket
            .local_addr()
            .ok()
            .and_then(|address| address.as_socket())
            .map(|address| address.port())
            .expect("the held port");
        HeldPort { socket, port }
    }

    /// The port held.
    pub fn port(&self) -> u16 {
        self.port
    }
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
