//! The addresses `serve` bound, the line it prints for them, and the socket it binds with.

use std::io;
use std::net::SocketAddr;

use crate::listen::accept_tcp;

/// Where the listeners are bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    /// The heartbeat listener.
    pub heartbeat: SocketAddr,
    /// The private listener.
    pub control: SocketAddr,
    /// The LAN listener, when `--lan-listen` is given.
    pub lan: Option<SocketAddr>,
    /// The listener of the Macs, when `--mdm-listen` is given.
    pub mdm: Option<SocketAddr>,
    /// The health listener, when `--health-listen` is given.
    pub health: Option<SocketAddr>,
}

/// Binds a TCP socket at `addr`: the address it got, and the function that accepts its
/// connections.
///
/// # Errors
/// The address cannot be bound.
pub async fn bind_tcp(
    addr: SocketAddr,
) -> io::Result<(
    SocketAddr,
    impl FnMut() -> futures::future::BoxFuture<'static, io::Result<tokio::net::TcpStream>> + Send,
)> {
    let tcp = tokio::net::TcpListener::bind(addr).await?;
    Ok((tcp.local_addr()?, accept_tcp(tcp)))
}

/// The line `serve` prints once its listeners are bound ([`start`]): the version, then where
/// each listener is, `lan=` (with `--lan-listen` only) after `control=`, `mdm=` (with `--mdm-listen`
/// only) after it, and `health=` (with `--health-listen` only) last.
#[must_use]
pub fn start_line(bound: &Bound) -> String {
    let lan = bound
        .lan
        .map_or_else(String::new, |lan| format!(" lan={lan}"));
    let mdm = bound
        .mdm
        .map_or_else(String::new, |mdm| format!(" mdm={mdm}"));
    let health = bound
        .health
        .map_or_else(String::new, |health| format!(" health={health}"));
    let (heartbeat, control) = (bound.heartbeat, bound.control);
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "hostkeeper-coordinator {version} heartbeat={heartbeat} control={control}{lan}{mdm}{health}"
    )
}
