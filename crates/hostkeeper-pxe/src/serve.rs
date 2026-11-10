//! `pxe serve` (docs/design/network-install.md, section 3, item N9): it serves a
//! directory the `prepare` step sealed to the machines the coordinator's inventory may install, in one of
//! two modes, until it is stopped. It keeps no durable state: what it learns in a run is in
//! memory, and it reads the inventory from the coordinator for each answer.
//!
//! The start, in this order ([`serve_with`]):
//!
//! 0. With `--notify-systemd`, the socket systemd named must be known ([`ServePorts`]): a start
//!    without it is refused before anything is bound.
//! 1. The files the flags name are read as the starting user (root on a host): the daemon
//!    config, with the daemon's own loader (a config the daemon would refuse is refused here,
//!    before a seed is made), the repository directory `--repositories` names, if given, with
//!    the daemon's loader of repository entries (see `crate::repositories`), the SSH keys file,
//!    and the coordinator's CA; the token file is opened and checked, not read.
//! 2. Every socket is bound ([`sockets`]): the ports are below 1024.
//! 3. The process drops to `--user-id` and `--group-id` ([`crate::privilege`]).
//! 4. The token is read from the descriptor opened in step 1, and the sealed directory is checked
//!    against its seal ([`sealed`]): any difference refuses the start, naming the file.
//! 5. The runtime starts. The coordinator is read once: a read that fails refuses the start
//!    (the operator is there to see the token or the address is wrong), and so does a
//!    `--reinstall` that names no record. The start is logged, with the platforms that have a
//!    boot set. With `--notify-systemd`, `READY=1` is then sent to systemd's notify socket
//!    ([`notify`]); a send that fails refuses the run.
//! 6. DHCP, TFTP and HTTP are served ([`dhcp`], [`tftp`], [`http`], over [`server`]) until the
//!    stop.
//!
//! No packet is read before step 6, so none is parsed as root.

pub mod answers;
pub mod inventory;

mod dhcp;
mod http;
mod log;
mod notify;
pub(crate) mod platforms;
mod sealed;
mod server;
mod sockets;
mod tftp;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::future::Future;
use std::net::{SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_config::trust::read_trusted;
use hostkeeper_pxe_packets::dhcp::{Mode, Port};
use hostkeeper_pxe_seed::inputs::{BootServer, DaemonConfigText};
use hostkeeper_pxe_seed::keys::AuthorizedKeys;
use hostkeeper_wire::HostId;
use hostkeeper_wire::secret::open_token;
use tokio::sync::watch;

use crate::cli::{ModeFlag, ServeFlags};
use crate::privilege::{DropTo, drop_to};
use crate::serve::answers::RunFacts;
use crate::serve::inventory::Coordinator;
use crate::serve::sockets::{Addresses, Bound};

pub use log::{Log, Now, Sink};
pub use sealed::{SealError, Sealed};
pub use server::{GRUB_PREFIX, Settings};

/// The stop of a run: a future made inside the run's runtime, which ends when the run is to
/// stop.
pub type Stop = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// What a run of `serve` takes from its host: the clock its log is stamped by, where its log
/// goes, when it stops, and the notify socket systemd named for the process (`NOTIFY_SOCKET`,
/// read by `main`), used only with `--notify-systemd`.
pub struct ServePorts {
    pub now: Now,
    pub sink: Sink,
    pub stop: Stop,
    pub notify_socket: Option<OsString>,
}

impl ServePorts {
    /// The host's: the system clock, standard error, SIGINT or SIGTERM, and the notify socket
    /// `main` read from the environment.
    pub fn host(notify_socket: Option<OsString>) -> ServePorts {
        ServePorts {
            now: Arc::new(log::system_now),
            sink: Arc::new(|line| eprintln!("pxe serve: {line}")),
            stop: Box::new(|| Box::pin(signalled())),
            notify_socket,
        }
    }
}

/// Ends at the first SIGINT or SIGTERM.
async fn signalled() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Runs `serve` as `flags` say, the daemon config owned by `owner` (root, 0, on a host), over
/// `ports`, until the stop. The error is the line the command prints.
///
/// # Errors
/// Why the start was refused, or why a socket of the run failed.
pub(crate) fn serve_with(flags: &ServeFlags, owner: u32, ports: ServePorts) -> Result<(), String> {
    let notify = match (flags.notify_systemd, ports.notify_socket.clone()) {
        (true, None) => {
            return Err(
                "--notify-systemd was given, but systemd set no notify socket (NOTIFY_SOCKET)"
                    .to_owned(),
            );
        }
        (true, socket) => socket,
        (false, _) => None,
    };
    let shown =
        |path: &std::path::Path, why: &dyn std::fmt::Display| format!("{}: {why}", path.display());
    let config = read_trusted(&flags.daemon_config, owner)
        .map_err(|e| shown(&flags.daemon_config, &e))
        .and_then(|bytes| {
            String::from_utf8(bytes).map_err(|_| shown(&flags.daemon_config, &"not UTF-8"))
        })?;
    let daemon_config =
        DaemonConfigText::parse(&config).map_err(|e| shown(&flags.daemon_config, &e))?;
    let repositories = flags
        .repositories
        .as_deref()
        .map(|dir| crate::repositories::read(dir, owner))
        .transpose()?;
    let keys = std::fs::read_to_string(&flags.ssh_authorized_keys)
        .map_err(|e| shown(&flags.ssh_authorized_keys, &e))
        .and_then(|text| {
            AuthorizedKeys::parse(&text).map_err(|e| shown(&flags.ssh_authorized_keys, &e))
        })?;
    let ca_pem = std::fs::read(&flags.ca_file).map_err(|e| shown(&flags.ca_file, &e))?;
    let token = open_token(&flags.token_file).map_err(|e| e.to_string())?;
    let mode = match flags.mode {
        ModeFlag::Proxy => Mode::Proxy,
        ModeFlag::Router => Mode::Router,
    };
    let bound = Bound::bind(&Addresses {
        dhcp: flags.dhcp_listen,
        proxy: (mode == Mode::Proxy).then_some(flags.proxy_listen),
        tftp: flags.tftp_listen,
        http: flags.http_listen,
        interface: flags.interface.clone(),
    })
    .map_err(|e| e.to_string())?;
    let to = DropTo {
        uid: flags.user_id,
        gid: flags.group_id,
    };
    drop_to(to).map_err(|e| e.to_string())?;
    let token = token.read().map_err(|e| e.to_string())?;
    let sealed = Sealed::open(&flags.dir).map_err(|e| shown(&flags.dir, &e))?;
    let settings = Settings {
        mode,
        address: flags.address,
        daemon_config,
        keys,
        install_disk: flags.install_disk.clone().unwrap_or_default(),
        serial_console: flags.serial_console.clone(),
        repositories,
        reinstall: flags.reinstall.iter().cloned().collect(),
        tftp_timeout: Duration::from_millis(flags.tftp_timeout_ms),
    };
    let start = Start {
        settings,
        bound,
        sealed,
        coordinator: flags.coordinator.clone(),
        ca_pem,
        token,
        reply_to: flags.dhcp_reply_to,
        dropped: to,
        notify,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the runtime: {e}"))?;
    let log = Log::new(ports.now, ports.sink);
    runtime.block_on(async move { run(start, log, (ports.stop)()).await })
}

/// Everything a run starts from, once the privileges are dropped.
struct Start {
    settings: Settings,
    bound: Bound,
    sealed: Sealed,
    coordinator: String,
    ca_pem: Vec<u8>,
    token: String,
    reply_to: SocketAddrV4,
    dropped: DropTo,
    /// The notify socket to send `READY=1` to once started; none without `--notify-systemd`.
    notify: Option<OsString>,
}

/// The address a bound socket took: its port is the one the kernel chose for a port of 0.
fn local(address: std::io::Result<SocketAddr>) -> String {
    address.map_or_else(|e| format!("unknown ({e})"), |a| a.to_string())
}

/// Step 5 and 6 of the module documentation.
async fn run(
    start: Start,
    log: Log,
    stop: Pin<Box<dyn Future<Output = ()> + Send>>,
) -> Result<(), String> {
    let Start {
        settings,
        bound,
        sealed,
        coordinator,
        ca_pem,
        token,
        reply_to,
        dropped,
        notify,
    } = start;
    let http_port = bound.http.local_addr().map_or(0, |a| a.port());
    let http = BootServer::new(SocketAddrV4::new(settings.address, http_port))
        .map_err(|e| format!("--address {}: {e}", settings.address))?;
    let coordinator = Coordinator::new(&coordinator, &ca_pem, &token).map_err(|e| e.to_string())?;
    let snapshot = coordinator
        .snapshot()
        .await
        .map_err(|e| format!("the coordinator could not be read: {e}"))?;
    let unknown: Vec<&HostId> = settings
        .reinstall
        .iter()
        .filter(|id| !snapshot.records.iter().any(|r| &r.host_id == *id))
        .collect();
    if let Some(id) = unknown.first() {
        return Err(format!(
            "--reinstall {id}: no inventory record has that host id"
        ));
    }
    let platforms: BTreeSet<_> = sealed.platforms().collect();
    let at_start = answers::answers(
        &snapshot,
        &RunFacts {
            mode: settings.mode,
            platforms: &platforms,
            reinstall: &settings.reinstall,
            seeded: &BTreeSet::new(),
        },
    );
    let names: Vec<&str> = platforms.iter().map(|p| p.as_str()).collect();
    log.line(format_args!(
        "started in {} mode as uid {} gid {}, answering as {}: the boot sets of hostkeeper {} for {}; DHCP {}, proxyDHCP {}, TFTP {}, HTTP {}",
        settings.mode,
        dropped.uid,
        dropped.gid,
        settings.address,
        sealed.version(),
        names.join(" and "),
        local(bound.dhcp.local_addr()),
        bound
            .proxy
            .as_ref()
            .map_or_else(|| "not bound".to_owned(), |s| local(s.local_addr())),
        local(bound.tftp.local_addr()),
        local(bound.http.local_addr()),
    ));
    if let Some(socket) = &notify {
        notify::ready(socket).map_err(|e| {
            format!(
                "cannot tell systemd the start is complete, on {}: {e}",
                socket.to_string_lossy()
            )
        })?;
    }
    for key in sealed.skipped() {
        log.line(format_args!(
            "the seal names the platform {key:?}, which this release does not serve: it is not served"
        ));
    }
    for id in &settings.reinstall {
        log.line(format_args!(
            "--reinstall {id}: its record is installed even while a host or a pending request holds it"
        ));
    }
    log.line(format_args!(
        "the coordinator holds {} records and {} pending requests; {} MACs are in the answer set",
        snapshot.records.len(),
        snapshot.pending.len(),
        at_start.menus.len()
    ));
    let socket = |s: std::net::UdpSocket| {
        tokio::net::UdpSocket::from_std(s).map_err(|e| format!("a socket cannot be served: {e}"))
    };
    let dhcp_socket = Arc::new(socket(bound.dhcp)?);
    let proxy_socket = bound.proxy.map(socket).transpose()?.map(Arc::new);
    let tftp_socket = socket(bound.tftp)?;
    let http_listener = tokio::net::TcpListener::from_std(bound.http)
        .map_err(|e| format!("a socket cannot be served: {e}"))?;
    let server = Arc::new(server::Server::new(
        settings,
        http,
        sealed,
        coordinator,
        log.clone(),
    ));
    let (stopping, stopped) = watch::channel(false);
    let mut tasks = vec![
        tokio::spawn(dhcp::serve(
            Arc::clone(&server),
            dhcp_socket,
            Port::Dhcp,
            reply_to,
            stopped.clone(),
        )),
        tokio::spawn(tftp::serve(
            Arc::clone(&server),
            tftp_socket,
            stopped.clone(),
        )),
        tokio::spawn(http::serve(
            Arc::clone(&server),
            http_listener,
            stopped.clone(),
        )),
    ];
    if let Some(proxy) = proxy_socket {
        tasks.push(tokio::spawn(dhcp::serve(
            Arc::clone(&server),
            proxy,
            Port::ProxyDhcp,
            reply_to,
            stopped,
        )));
    }
    stop.await;
    let _ = stopping.send(true);
    for task in tasks {
        let _ = task.await;
    }
    log.line("stopped");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
