//! The command line of `hostkeeper-pxe`: two verbs, `fetch` ([`crate::fetch`]) and `serve`
//! ([`crate::serve`]).
//!
//! ```text
//! hostkeeper-pxe fetch --release <https URL> --dir <new directory> --daemon-config <file>
//!     [--platform linux-x86_64|linux-arm64]... [--ca-bundle <file>] [--gh <file>]
//!     [--trusted-root <file>] [--download-timeout-secs <n>]
//! hostkeeper-pxe serve --mode proxy|router --address <IPv4> --dir <sealed directory>
//!     --daemon-config <file> --ssh-authorized-keys <file> --coordinator <https URL>
//!     --ca-file <file> --token-file <file> --user-id <n> --group-id <n>
//!     [--interface <name>] [--dhcp-listen <a:p>] [--proxy-listen <a:p>] [--tftp-listen <a:p>]
//!     [--http-listen <a:p>] [--dhcp-reply-to <a:p>] [--install-disk <form>=<glob>]
//!     [--serial-console <port>] [--reinstall <host id>]... [--tftp-timeout-ms <n>]
//! ```
//!
//! `fetch`:
//!
//! - `--release`: the `https://` URL of the directory holding the release's assets, such as
//!   `https://github.com/<owner>/<repo>/releases/download/v<X.Y.Z>`; `netboot.json`, the debs and
//!   their bundles are fetched from it. Its redirects are followed, up to [`MAX_REDIRECTS`], to
//!   `https://` URLs only: a GitHub release download answers with one.
//! - `--dir`: the directory to make and fill; it must not exist.
//! - `--daemon-config`: the daemon config the installed hosts get (the one `serve` writes into
//!   the seeds), loaded with the daemon's own loader: root-owned, not writable by group or
//!   others. Its `[trusted_signers]` is the signer `netboot.json` and the debs must be attested
//!   by; its `bounds.attestation_timeout_secs` bounds each check, and `bounds.max_package_bytes`
//!   each deb.
//! - `--platform`: a platform to fetch, repeatable; both when none is named.
//! - `--ca-bundle`, `--gh`, `--trusted-root`: the roots the HTTPS client trusts, the pinned `gh`
//!   and the Sigstore trusted root the hostkeeper deb installs; by default the paths a Linux host
//!   has them at ([`CA_BUNDLE`], [`GH`], [`TRUSTED_ROOT`]). `gh` and the trusted root must be
//!   owned by root and not writable by group or others, as for the daemon.
//! - `--download-timeout-secs`: the bound of each download, [`DOWNLOAD_TIMEOUT_SECS`] by default
//!   (an ISO is several GiB).
//!
//! `serve` (network-install.md section 3, "What it serves, and how it runs"):
//!
//! - `--mode`: `proxy` (a proxyDHCP server beside the router's DHCP) or `router` (the router
//!   names this server and `hostkeeper/boot.efi`; no DHCP packet is sent). No default.
//! - `--address`: this server's IPv4 address on the farm LAN, which its answers, menus and seeds
//!   name; `--interface`: the one interface to answer on, when given.
//! - `--dhcp-listen`, `--proxy-listen`, `--tftp-listen`, `--http-listen`: where each socket is
//!   bound, by default the protocol port on every address (67, 4011, 69, 80); router mode binds
//!   no proxyDHCP socket. `--dhcp-reply-to`: where a proxy offer is sent, by default the limited
//!   broadcast address and the client port, `255.255.255.255:68`.
//! - `--coordinator`, `--ca-file`, `--token-file`: the coordinator's private listener
//!   (`https://<host>[:<port>]`), the CA its certificate chains to, and the operator token, in a
//!   file of the starting user's (root's) with mode 0600 or 0400.
//! - `--dir`: a directory `fetch` sealed. `--daemon-config` (root-owned, loaded with the daemon's
//!   loader), `--ssh-authorized-keys`, `--install-disk` and `--serial-console`: the inputs of
//!   the seeds (section 4).
//! - `--reinstall`: a host id whose record is installed even though a host or a pending request
//!   holds it (section 8), repeatable; it must name a record.
//! - `--user-id`, `--group-id`: the unprivileged user and group `serve` drops to once its
//!   sockets are bound, before it reads the token or a packet.
//! - `--tftp-timeout-ms`: how long a TFTP transfer waits for an ACK before it sends again.
//!
//! Exit status 0 when the fetch is sealed (one line on standard output names it) or `serve`
//! stops on SIGINT or SIGTERM, 1 when the verb is refused (one line on standard error says why;
//! a refused fetch leaves no directory), 2 for a command line that does not parse. `serve` logs
//! one line per event on standard error. Configuration is flags only; nothing is read from the
//! environment.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_package::gh::{FileTrust, GhVerifier, OwnedBy, Runner, SystemRunner};
use hostkeeper_package::net::{Timeouts, UreqTransport};
use hostkeeper_pxe_seed::inputs::{InstallDisk, SerialConsole};
use hostkeeper_steps::{Clock, ProcBootId, SystemClock};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use crate::fetch::{GH_HOME, Ports, Request, VERIFY_DIR, fetch};
use crate::platform::BootPlatform;
use crate::seal::Seal;
use crate::serve::{ServePorts, serve_with};

/// The default of `--ca-bundle`: the Debian family's CA bundle.
pub const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The default of `--gh`: the pinned `gh` the hostkeeper deb installs.
pub const GH: &str = "/usr/lib/hostkeeper/gh";
/// The default of `--trusted-root`: the Sigstore trusted root the hostkeeper deb installs.
pub const TRUSTED_ROOT: &str = "/usr/lib/hostkeeper/trusted_root.json";
/// The default of `--download-timeout-secs`: four hours.
pub const DOWNLOAD_TIMEOUT_SECS: u64 = 4 * 60 * 60;
/// The longest a download waits to connect.
pub const CONNECT_TIMEOUT_SECS: u64 = 30;
/// The most redirects a download follows.
pub const MAX_REDIRECTS: u32 = 5;
/// The owner `--daemon-config`, `gh` and the trusted root must have: root.
const ROOT: u32 = 0;

#[derive(Debug, Parser)]
#[command(
    name = "hostkeeper-pxe",
    about = "The boot server of network install",
    version
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) verb: Verb,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Verb {
    /// Download the boot sets and debs of a release into a new directory, verify them against
    /// the release's attestation and netboot.json, and seal the directory.
    Fetch(FetchFlags),
    /// Serve a sealed directory to the machines the coordinator's inventory may install: by
    /// proxyDHCP or behind the router, by TFTP and by HTTP, until SIGINT or SIGTERM.
    Serve(Box<ServeFlags>),
}

/// The modes of `--mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ModeFlag {
    /// A proxyDHCP server beside the router's DHCP.
    Proxy,
    /// The router names this server and hostkeeper/boot.efi; no DHCP packet is sent.
    Router,
}

/// The flags of `serve`.
#[derive(Debug, clap::Args)]
pub(crate) struct ServeFlags {
    /// proxy or router.
    #[arg(long, value_enum)]
    pub(crate) mode: ModeFlag,
    /// This server's IPv4 address on the farm LAN, which its answers, menus and seeds name.
    #[arg(long)]
    pub(crate) address: Ipv4Addr,
    /// The one interface to answer on.
    #[arg(long)]
    pub(crate) interface: Option<String>,
    /// Where the DHCP socket is bound.
    #[arg(long, default_value = "0.0.0.0:67")]
    pub(crate) dhcp_listen: SocketAddrV4,
    /// Where the proxyDHCP socket is bound (proxy mode).
    #[arg(long, default_value = "0.0.0.0:4011")]
    pub(crate) proxy_listen: SocketAddrV4,
    /// Where the TFTP socket is bound.
    #[arg(long, default_value = "0.0.0.0:69")]
    pub(crate) tftp_listen: SocketAddrV4,
    /// Where the HTTP listener is bound.
    #[arg(long, default_value = "0.0.0.0:80")]
    pub(crate) http_listen: SocketAddrV4,
    /// Where a proxy offer is sent.
    #[arg(long, default_value = "255.255.255.255:68")]
    pub(crate) dhcp_reply_to: SocketAddrV4,
    /// The coordinator's private listener, https://<host>[:<port>].
    #[arg(long)]
    pub(crate) coordinator: String,
    /// The PEM CA the private listener's certificate chains to.
    #[arg(long)]
    pub(crate) ca_file: PathBuf,
    /// The operator token, in a file of the starting user's with mode 0600 or 0400.
    #[arg(long)]
    pub(crate) token_file: PathBuf,
    /// A directory hostkeeper-pxe fetch sealed.
    #[arg(long)]
    pub(crate) dir: PathBuf,
    /// The daemon config the seeds install.
    #[arg(long)]
    pub(crate) daemon_config: PathBuf,
    /// The SSH public keys of the installed system's operator account.
    #[arg(long)]
    pub(crate) ssh_authorized_keys: PathBuf,
    /// The disk to install on: serial=<glob>, model=<glob> or path=<glob>; the smallest SSD
    /// when not given.
    #[arg(long)]
    pub(crate) install_disk: Option<InstallDisk>,
    /// The installed system's serial console: ttyS<n> or ttyAMA<n>.
    #[arg(long)]
    pub(crate) serial_console: Option<SerialConsole>,
    /// A host id to reinstall (repeatable).
    #[arg(long = "reinstall")]
    pub(crate) reinstall: Vec<hostkeeper_wire::HostId>,
    /// The user to drop to once the sockets are bound.
    #[arg(long)]
    pub(crate) user_id: u32,
    /// The group to drop to once the sockets are bound.
    #[arg(long)]
    pub(crate) group_id: u32,
    /// How long a TFTP transfer waits for an ACK before it sends again, in milliseconds.
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) tftp_timeout_ms: u64,
}

#[derive(Debug, clap::Args)]
pub(crate) struct FetchFlags {
    /// The https:// URL of the directory holding the release's assets.
    #[arg(long, value_parser = release_url)]
    release: String,
    /// The directory to make and fill; it must not exist.
    #[arg(long)]
    dir: PathBuf,
    /// The daemon config whose trusted signer the assets must be attested by.
    #[arg(long)]
    daemon_config: PathBuf,
    /// A platform to fetch (repeatable); both when none is named.
    #[arg(long = "platform")]
    platforms: Vec<BootPlatform>,
    /// The PEM bundle of the roots the HTTPS client trusts.
    #[arg(long, default_value = CA_BUNDLE)]
    ca_bundle: PathBuf,
    /// The pinned gh that checks the attestations.
    #[arg(long, default_value = GH)]
    gh: PathBuf,
    /// The Sigstore trusted root gh checks against.
    #[arg(long, default_value = TRUSTED_ROOT)]
    trusted_root: PathBuf,
    /// The bound of each download, in seconds.
    #[arg(long, default_value_t = DOWNLOAD_TIMEOUT_SECS)]
    download_timeout_secs: u64,
}

/// `text` as the release URL: `https://`, then printable characters with no query or fragment.
fn release_url(text: &str) -> Result<String, String> {
    let rest = text.strip_prefix("https://").unwrap_or_default();
    let plain = rest
        .bytes()
        .all(|b| b.is_ascii_graphic() & !matches!(b, b'?' | b'#'));
    match !rest.is_empty() & plain {
        true => Ok(text.to_owned()),
        false => Err("not an https:// URL without a query or fragment".to_owned()),
    }
}

/// The program: [`run`] over the host's ports, with root as the owner of the daemon config,
/// `gh` and the trusted root; `serve` logs to standard error, by the host's clock, until SIGINT
/// or SIGTERM.
pub fn main(args: Vec<OsString>) -> ExitCode {
    let mut out = io::stdout();
    let mut err = io::stderr();
    ExitCode::from(run(
        args,
        &|flags| fetch_with(flags, ROOT),
        &|flags| serve_with(flags, ROOT, ServePorts::host()),
        &mut out,
        &mut err,
    ))
}

/// How `fetch` is run once its flags parse: [`fetch_with`] on a host.
type Fetch<'a> = &'a dyn Fn(&FetchFlags) -> Result<Seal, String>;

/// How `serve` is run once its flags parse: [`serve_with`] on a host. It returns when the run
/// stops.
type Serve<'a> = &'a dyn Fn(&ServeFlags) -> Result<(), String>;

/// Parses `args` and runs the verb through `fetch` or `serve`; returns the exit status.
fn run(
    args: Vec<OsString>,
    fetch: Fetch<'_>,
    serve: Serve<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let text = error.render().to_string();
            drop(match error.use_stderr() {
                true => err.write_all(text.as_bytes()),
                false => out.write_all(text.as_bytes()),
            });
            return u8::try_from(error.exit_code()).unwrap_or(2);
        }
    };
    match cli.verb {
        Verb::Fetch(flags) => match fetch(&flags) {
            Ok(seal) => {
                drop(writeln!(out, "{}", sealed_line(&flags.dir, &seal)));
                0
            }
            Err(why) => {
                drop(writeln!(err, "hostkeeper-pxe fetch: {why}"));
                1
            }
        },
        Verb::Serve(flags) => match serve(&flags) {
            Ok(()) => 0,
            Err(why) => {
                drop(writeln!(err, "hostkeeper-pxe serve: {why}"));
                1
            }
        },
    }
}

/// The line that names a sealed directory.
fn sealed_line(dir: &Path, seal: &Seal) -> String {
    let platforms: Vec<&str> = seal.platforms.keys().map(String::as_str).collect();
    format!(
        "sealed {}: hostkeeper {}, Ubuntu {}, {}",
        dir.display(),
        seal.version,
        seal.ubuntu,
        platforms.join(" and ")
    )
}

/// Loads the daemon config and the CA bundle, makes the host's ports and fetches.
fn fetch_with(flags: &FetchFlags, owner: u32) -> Result<Seal, String> {
    let config = DaemonConfig::load(&flags.daemon_config, owner)
        .map_err(|e| format!("{}: {e}", flags.daemon_config.display()))?;
    let roots = tls_roots(&flags.ca_bundle)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new(Box::new(ProcBootId::host())));
    let transport = UreqTransport::following_redirects(&roots, MAX_REDIRECTS);
    let verifier = verifier_of(
        flags,
        Box::new(SystemRunner::new(Arc::clone(&clock))),
        Box::new(OwnedBy { uid: owner }),
    );
    let ports = Ports {
        transport: &transport,
        verifier: &verifier,
        clock: &*clock,
    };
    fetch(&ports, &request_of(flags, config)).map_err(|e| e.to_string())
}

/// The daemon's verifier as the flags name it: `--gh` run by `runner` against `--trusted-root`,
/// in `gh`'s home under the new directory, each file checked by `trust` before each run.
fn verifier_of(
    flags: &FetchFlags,
    runner: Box<dyn Runner>,
    trust: Box<dyn FileTrust>,
) -> GhVerifier {
    GhVerifier::new(
        flags.gh.clone(),
        flags.trusted_root.clone(),
        flags.dir.join(VERIFY_DIR).join(GH_HOME),
        runner,
        trust,
    )
}

/// The fetch the flags and the loaded daemon config ask for.
fn request_of(flags: &FetchFlags, config: DaemonConfig) -> Request {
    Request {
        release: flags.release.clone(),
        dir: flags.dir.clone(),
        platforms: platforms_of(&flags.platforms),
        signers: config.trusted_signers,
        attestation_timeout_secs: config.bounds.attestation_timeout_secs,
        max_deb_bytes: config.bounds.max_package_bytes,
        timeouts: Timeouts {
            connect: Duration::from_secs(CONNECT_TIMEOUT_SECS),
            total: Duration::from_secs(flags.download_timeout_secs),
        },
    }
}

/// The platforms a fetch of `named` (the `--platform` flags) takes: both when none is named.
fn platforms_of(named: &[BootPlatform]) -> BTreeSet<BootPlatform> {
    match named.is_empty() {
        true => BootPlatform::ALL.into_iter().collect(),
        false => named.iter().copied().collect(),
    }
}

/// The DER certificates of the PEM bundle at `path`; a bundle that holds none is refused, since
/// no download could succeed.
fn tls_roots(path: &Path) -> Result<Vec<Vec<u8>>, String> {
    let roots: Vec<Vec<u8>> = CertificateDer::pem_file_iter(path)
        .map(|sections| {
            sections
                .filter_map(Result::ok)
                .map(|der| der.as_ref().to_vec())
                .collect()
        })
        .unwrap_or_default();
    match roots.is_empty() {
        true => Err(format!(
            "--ca-bundle {}: no certificate could be read",
            path.display()
        )),
        false => Ok(roots),
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
