//! The command line of `pxe`, the boot server of network install: `serve` ([`crate::serve`])
//! and the six steps of the hook the `pxe` package ships ([`crate::hook`]).
//!
//! ```text
//! pxe serve --mode proxy|router --address <IPv4> --dir <sealed directory>
//!     --daemon-config <file> --ssh-authorized-keys <file> --coordinator <https URL>
//!     --ca-file <file> --token-file <file> --user-id <n> --group-id <n>
//!     [--interface <name>] [--dhcp-listen <a:p>] [--proxy-listen <a:p>] [--tftp-listen <a:p>]
//!     [--http-listen <a:p>] [--dhcp-reply-to <a:p>] [--install-disk <form>=<glob>]
//!     [--serial-console <port>] [--repositories <dir>] [--reinstall <host id>]...
//!     [--tftp-timeout-ms <n>] [--settings <file>] [--notify-systemd]
//! pxe update-hook prepare <staged release> --params <file>
//! pxe update-hook cutover|verify|rollback|cleanup|uninstall --params <file>
//! ```
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
//! - `--dir`: a directory the `prepare` step sealed. `--daemon-config` (root-owned, loaded with
//!   the daemon's loader), `--ssh-authorized-keys`, `--install-disk` and `--serial-console`: the
//!   inputs of the seeds (section 4).
//! - `--repositories`: a directory of the installed hosts' repository entries, `<name>.sources`
//!   files whose `Signed-By` names `/etc/hostkeeper/repositories/<keyring>`, and the keyrings
//!   they name (packages-repositories.md sections 3.1 and 3.2). It is checked as root at the
//!   start with the daemon's own loader, as if it were at that path, and a directory it would
//!   refuse, a missing one or one holding no entry refuses the start. Every seed then writes the
//!   files, byte for byte, to `/etc/hostkeeper/repositories/`. Without it a seed writes none.
//! - `--reinstall`: a host id whose record is installed even though a host or a pending request
//!   holds it (section 8), repeatable; it must name a record.
//! - `--user-id`, `--group-id`: the unprivileged user and group `serve` drops to once its
//!   sockets are bound, before it reads the token or a packet.
//! - `--tftp-timeout-ms`: how long a TFTP transfer waits for an ACK before it sends again.
//! - `--settings`: the settings file ([`crate::settings`]); its `[serve]` table is expanded into
//!   flags appended after the command line's, so the one parser judges both.
//! - `--notify-systemd`: send `READY=1` to the socket systemd names in `NOTIFY_SOCKET` once the
//!   start is complete; without that socket the start is refused before anything is bound.
//!
//! The steps are run by the host updater as `<exe> <step> [<staged release>] --params <file>`
//! ([`crate::hook`] says what each does). `--params` is the file of the package's `[params]` and
//! the updater's `updater.*` keys.
//!
//! Exit status: 0 when `serve` stops on SIGINT or SIGTERM or a step succeeds, 1 when the verb is
//! refused (one line on standard error says why, as `pxe <verb>: <why>`; a refused step also
//! ends its standard output with a JSON report), 75 for a step deferred, 2 for a command line
//! that does not parse. `serve` logs one line per event on standard error. Configuration is
//! flags and the settings file; the one thing read from the environment is systemd's
//! `NOTIFY_SOCKET`.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use hostkeeper_hook::Step;
use hostkeeper_pxe_seed::inputs::{InstallDisk, SerialConsole};

use crate::hook::{self, HostSystemctl};
use crate::serve::{ServePorts, serve_with};
use crate::settings;

/// The default of `--ca-bundle` in `[prepare]`: the Debian family's CA bundle.
pub const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The default of `gh` in `[prepare]`: the pinned `gh` the hostkeeper deb installs.
pub const GH: &str = "/usr/lib/hostkeeper/gh";
/// The default of `trusted-root` in `[prepare]`: the Sigstore trusted root the hostkeeper deb
/// installs.
pub const TRUSTED_ROOT: &str = "/usr/lib/hostkeeper/trusted_root.json";
/// The default of `download-timeout-secs` in `[prepare]`: four hours.
pub const DOWNLOAD_TIMEOUT_SECS: u64 = 4 * 60 * 60;
/// The longest a download waits to connect.
pub const CONNECT_TIMEOUT_SECS: u64 = 30;
/// The most redirects a download follows.
pub const MAX_REDIRECTS: u32 = 5;
/// The owner the daemon config, the settings file, `gh` and the trusted root must have: root.
const ROOT: u32 = 0;
/// The variable systemd sets for a `Type=notify` unit: the address of its notify socket.
const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";

#[derive(Debug, Parser)]
#[command(name = "pxe", about = "The boot server of network install", version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) verb: Verb,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Verb {
    /// Serve a sealed directory to the machines the coordinator's inventory may install: by
    /// proxyDHCP or behind the router, by TFTP and by HTTP, until SIGINT or SIGTERM.
    Serve(Box<ServeFlags>),
    /// Hook step: download the boot sets and debs of the release into
    /// <boot_dir>/<target version>, verify them, and seal them; nothing a client sees changes.
    Prepare(PrepareArgs),
    /// Hook step: point <boot_dir>/current at the target version and restart the unit.
    Cutover(StepArgs),
    /// Hook step: the unit is active, or the step is deferred.
    Verify(StepArgs),
    /// Hook step: put back the previous boot set, or stop the unit on a first install.
    Rollback(StepArgs),
    /// Hook step: keep only the current and the target boot sets.
    Cleanup(StepArgs),
    /// Hook step: remove <boot_dir> whole.
    Uninstall(StepArgs),
}

/// The arguments of the `prepare` step.
#[derive(Debug, clap::Args)]
pub(crate) struct PrepareArgs {
    /// The staged release directory of the package.
    pub(crate) artifact: PathBuf,
    /// The params file of the step.
    #[arg(long)]
    pub(crate) params: PathBuf,
}

/// The arguments of every other step.
#[derive(Debug, clap::Args)]
pub(crate) struct StepArgs {
    /// The params file of the step.
    #[arg(long)]
    pub(crate) params: PathBuf,
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
    /// A directory the prepare step sealed.
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
    /// A directory of the installed hosts' repository entries (<name>.sources) and the keyrings
    /// they name, written to /etc/hostkeeper/repositories/ by every seed.
    #[arg(long)]
    pub(crate) repositories: Option<PathBuf>,
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
    /// A settings file (root-owned, not writable by group or others) whose [serve] table gives
    /// flags of this command; the command line's flags come first, and a flag may not be given
    /// in both.
    #[arg(long)]
    pub(crate) settings: Option<PathBuf>,
    /// Send READY=1 to systemd's notify socket once the start is complete (a Type=notify unit).
    #[arg(long)]
    pub(crate) notify_systemd: bool,
}

/// The program: [`run`] over the host's ports, with root as the owner of the settings file, the
/// daemon config, `gh` and the trusted root, and `/usr/bin/systemctl` as the service manager;
/// `serve` logs to standard error, by the host's clock, until SIGINT or SIGTERM.
pub fn main(args: Vec<OsString>) -> ExitCode {
    let mut out = io::stdout();
    let mut err = io::stderr();
    let notify_socket = notify_socket();
    let systemctl = HostSystemctl::host();
    let host = hook::Host {
        owner: ROOT,
        systemctl: &systemctl,
        fetch: &hook::fetch_with,
    };
    ExitCode::from(run(
        args,
        &|flags| serve_with(flags, ROOT, ServePorts::host(notify_socket.clone())),
        &host,
        &mut out,
        &mut err,
    ))
}

/// The address of the notify socket systemd set for this process, if it did.
#[expect(
    clippy::disallowed_methods,
    reason = "NOTIFY_SOCKET is the platform-set marker of a Type=notify unit, which AGENTS.md S2 allows; the flag --notify-systemd decides whether it is used"
)]
fn notify_socket() -> Option<OsString> {
    std::env::var_os(NOTIFY_SOCKET)
}

/// How `serve` is run once its flags parse: [`serve_with`] on a host. It returns when the run
/// stops.
type Serve<'a> = &'a dyn Fn(&ServeFlags) -> Result<(), String>;

/// Parses `args` and runs the verb: `serve` through `serve`, a step through `host`; returns the
/// exit status.
fn run(
    args: Vec<OsString>,
    serve: Serve<'_>,
    host: &hook::Host<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let args = match with_settings(args, host.owner) {
        Ok(args) => args,
        Err(why) => {
            drop(writeln!(err, "pxe serve: {why}"));
            return 1;
        }
    };
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
        Verb::Serve(flags) => match serve(&flags) {
            Ok(()) => 0,
            Err(why) => {
                drop(writeln!(err, "pxe serve: {why}"));
                1
            }
        },
        Verb::Prepare(a) => hook::run(host, Step::Prepare, &a.params, out, err),
        Verb::Cutover(a) => hook::run(host, Step::Cutover, &a.params, out, err),
        Verb::Verify(a) => hook::run(host, Step::Verify, &a.params, out, err),
        Verb::Rollback(a) => hook::run(host, Step::Rollback, &a.params, out, err),
        Verb::Cleanup(a) => hook::run(host, Step::Cleanup, &a.params, out, err),
        Verb::Uninstall(a) => hook::run(host, Step::Uninstall, &a.params, out, err),
    }
}

/// `args` with, for a `serve` that names `--settings`, the file's `[serve]` table expanded into
/// flags after them. Any other command line is returned as it is.
fn with_settings(mut args: Vec<OsString>, owner: u32) -> Result<Vec<OsString>, String> {
    if args.get(1).map(OsString::as_os_str) != Some(OsStr::new("serve")) {
        return Ok(args);
    }
    let mut named = None;
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        if arg == "--settings" {
            named = rest.next().map(PathBuf::from);
        } else if let Some(path) = arg.to_str().and_then(|a| a.strip_prefix("--settings=")) {
            named = Some(PathBuf::from(path));
        }
    }
    let Some(path) = named else {
        return Ok(args);
    };
    let expanded = settings::read(&path, owner)?;
    args.extend(expanded.serve.into_iter().map(OsString::from));
    Ok(args)
}

/// The flags of `serve` as the settings file's `[serve]` table and `--dir` give them: the same
/// parser as the command line's, so a key it does not know, a flag it needs and a bad value are
/// refused as it refuses them.
///
/// # Errors
/// The parser's message.
pub(crate) fn serve_flags(
    settings: &Path,
    serve: &[String],
    dir: &Path,
) -> Result<ServeFlags, String> {
    let mut args: Vec<OsString> = vec!["pxe serve".into()];
    args.extend([
        "--settings".into(),
        settings.into(),
        "--dir".into(),
        dir.into(),
    ]);
    args.extend(serve.iter().map(OsString::from));
    ServeOnly::try_parse_from(args)
        .map(|only| only.flags)
        .map_err(|e| one_line(&e))
}

/// The message of the parser's refusal `error` on one line: its words up to the usage text.
pub(crate) fn one_line(error: &clap::Error) -> String {
    error
        .to_string()
        .lines()
        .take_while(|line| !line.starts_with("Usage:"))
        .map(|line| line.trim().trim_start_matches("error: "))
        .filter(|line| !line.is_empty() && !line.starts_with("For more information"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The flags of `serve` alone, for [`serve_flags`].
#[derive(Debug, Parser)]
#[command(name = "pxe serve")]
struct ServeOnly {
    #[command(flatten)]
    flags: ServeFlags,
}

/// `text` as the release URL: `https://`, then printable characters with no query or fragment.
pub(crate) fn release_url(text: &str) -> Result<String, String> {
    let rest = text.strip_prefix("https://").unwrap_or_default();
    let plain = rest
        .bytes()
        .all(|b| b.is_ascii_graphic() & !matches!(b, b'?' | b'#'));
    match !rest.is_empty() & plain {
        true => Ok(text.to_owned()),
        false => Err("not an https:// URL without a query or fragment".to_owned()),
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "runbook_tests.rs"]
mod runbook_tests;
