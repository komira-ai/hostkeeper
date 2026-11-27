//! The verbs root runs on the host itself, with no coordinator, no network and no daemon
//! (docs/admin.md, "Roll a service back on the host"; docs/design/hostkeeper-design.md, Q16):
//!
//! - `hostkeeper activate --service S --version V [--pin]`: switches `S` to `V`, a version the
//!   host keeps, with no fetch ([`crate::verbs::force::activate()`]). It takes the state
//!   directory's lock for its whole run, so it refuses to run beside the daemon and names the
//!   daemon's pid: stop the daemon first. A lock whose holder died is free, since the lock is the
//!   kernel's (`flock`), and is taken as any free lock. Its prune deletes nothing: the daemon's
//!   next confirmed switch prunes with the daemon's own `--keep-versions`.
//! - `hostkeeper pin --service S --version V`: pins `S` at `V`, a version the host keeps
//!   ([`crate::pin`]). The daemon holds the coordinator's orders for `S` while the pin stands.
//! - `hostkeeper unpin --service S`: removes whatever is at the pin's path of `S`; the daemon
//!   follows the coordinator again from its next order. Unpinning a service that is not pinned
//!   is no error.
//!
//! - `hostkeeper renew`: asks the daemon to renew the host's certificate at its next enrollment
//!   step (within 5 seconds while it runs, else at its start) rather than at the middle of its
//!   validity: it writes the pki directory's `RENEW` ([`hostkeeper_enroll::PkiDir::ask_renewal`])
//!   and exits. The daemon renews as it does at the middle, the same `Renew` over mutual TLS for
//!   the key it holds, so no new key and no approval; it removes the ask once the new
//!   generation is stored or the coordinator refuses the renewal, and logs either as it does at
//!   the middle (docs/operations/enrollment.md, "Renew a certificate now"). A host with no
//!   certificate is refused `not_enrolled`, and nothing is written.
//!
//! `pin`, `unpin` and `renew` write one file and take no lock, so they may run beside the daemon:
//! an order that has begun runs to its end, and the pin holds the orders that come after it.
//!
//! Exit status: 0 when the verb did what it says; 1 when it was refused or failed, with
//! `hostkeeper <verb>: refused: <token>: <why>` or `hostkeeper <verb>: failed: <token>` on
//! standard error (the tokens of [`crate::refusal::Refusal`] and of the force's verdict, and
//! [`NOT_ENROLLED`]); 2 for a
//! usage error, as clap reports it; and for `activate`, [`EXIT_CONFIG`] when the daemon config
//! does not load and [`EXIT_LOCKED`] when the daemon (or another `activate`) holds the state
//! directory.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Subcommand};
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_enroll::PkiDir;
use hostkeeper_hook::Verdict;
use hostkeeper_launchd::Platform;
use hostkeeper_steps::{Layout, Protected, PruneReport};

use crate::adapters::Adapters;
use crate::cli::{
    DEFAULT_CONFIG, DEFAULT_FACTS_FILE, DEFAULT_PACKAGE_LOCK_WAIT_SECS, DEFAULT_PACKAGES_DIR,
    DEFAULT_STATE_DIR, HostFiles, PKI_DIR, host_ports,
};
use crate::executor::{Daemon, Dirs, Ports, Reply};
use crate::installed::{self, CURRENT, PREVIOUS};
use crate::locks::ServiceGuard;
use crate::pin;
use crate::prune::{DEFAULT_KEEP_VERSIONS, Prune};
use crate::refusal::Refusal;
use crate::start::{self, StartError};
use crate::verbs::force::{self, SELF_SERVICE};

pub use crate::start::{EXIT_CONFIG, EXIT_LOCKED};

/// The exit status of a verb that was refused or failed.
pub const EXIT_REFUSED: u8 = 1;

/// The token of `renew` refused on a host with no certificate.
pub const NOT_ENROLLED: &str = "not_enrolled";

/// The verbs root runs on the host itself.
#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum LocalVerb {
    /// Switch a service to a version the host keeps, with no fetch and no network: root's
    /// rollback with the coordinator down. Stop the daemon first.
    Activate(ActivateArgs),
    /// Pin a service at a version the host keeps: the daemon holds the coordinator's orders for
    /// it until `unpin`, or a coordinator force to that version.
    Pin(PinArgs),
    /// Remove a service's pin: the daemon follows the coordinator again.
    Unpin(UnpinArgs),
    /// Ask the daemon to renew the host's certificate now, for the key it holds, as it does at
    /// the middle of the certificate's validity: no new key and no approval.
    Renew(RenewArgs),
}

/// The flags of `hostkeeper activate`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct ActivateArgs {
    /// The daemon config, as the daemon is given it: root-owned, not writable by group or others.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    pub config: PathBuf,
    /// The daemon's `--state-dir`: the step records and the lock. The pins are beside it.
    #[arg(long, default_value = DEFAULT_STATE_DIR)]
    pub state_dir: PathBuf,
    /// The daemon's `--packages-dir`: the versions the host keeps.
    #[arg(long, default_value = DEFAULT_PACKAGES_DIR)]
    pub packages_dir: PathBuf,
    /// The service to switch.
    #[arg(long, value_parser = service_name)]
    pub service: ServiceName,
    /// The version to switch it to: one the host keeps whole.
    #[arg(long, value_parser = version)]
    pub version: Version,
    /// Pin the service at the version first, so the daemon holds the coordinator's orders for it.
    #[arg(long)]
    pub pin: bool,
}

impl ActivateArgs {
    /// The daemon's settings these flags name; the daemon config is trusted when root owns it.
    pub fn settings(&self) -> start::Settings {
        start::Settings {
            config: self.config.clone(),
            state_dir: self.state_dir.clone(),
            packages_dir: self.packages_dir.clone(),
            facts_file: PathBuf::from(DEFAULT_FACTS_FILE),
            run_dir: Platform::host().run_dir,
            trusted_uid: 0,
            package_lock_wait_secs: DEFAULT_PACKAGE_LOCK_WAIT_SECS,
            keep_versions: DEFAULT_KEEP_VERSIONS,
        }
    }
}

/// The flags of `hostkeeper pin`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct PinArgs {
    /// The daemon's `--state-dir`. The pins are beside it.
    #[arg(long, default_value = DEFAULT_STATE_DIR)]
    pub state_dir: PathBuf,
    /// The daemon's `--packages-dir`: the versions the host keeps.
    #[arg(long, default_value = DEFAULT_PACKAGES_DIR)]
    pub packages_dir: PathBuf,
    /// The service to pin.
    #[arg(long, value_parser = service_name)]
    pub service: ServiceName,
    /// The version to pin it at: one the host keeps whole.
    #[arg(long, value_parser = version)]
    pub version: Version,
}

/// The flags of `hostkeeper unpin`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct UnpinArgs {
    /// The daemon's `--state-dir`. The pins are beside it.
    #[arg(long, default_value = DEFAULT_STATE_DIR)]
    pub state_dir: PathBuf,
    /// The service to unpin.
    #[arg(long, value_parser = service_name)]
    pub service: ServiceName,
}

/// The flags of `hostkeeper renew`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct RenewArgs {
    /// The daemon's pki directory: the host's key and certificate generations.
    #[arg(long, default_value = PKI_DIR)]
    pub pki_dir: PathBuf,
}

fn service_name(text: &str) -> Result<ServiceName, String> {
    ServiceName::new(text).map_err(|error| error.to_string())
}

fn version(text: &str) -> Result<Version, String> {
    Version::new(text).map_err(|error| error.to_string())
}

/// Runs `verb` on this host to its exit status, writing to standard output and error.
pub fn main(verb: LocalVerb) -> u8 {
    let (mut out, mut err) = (io::stdout().lock(), io::stderr().lock());
    match verb {
        LocalVerb::Activate(args) => activate_with(
            &args,
            &args.settings(),
            &HostFiles::host(),
            &|| Adapters::for_platform(&Platform::host()),
            &mut out,
            &mut err,
        ),
        LocalVerb::Pin(args) => pin(&args, &mut out, &mut err),
        LocalVerb::Unpin(args) => unpin(&args, &mut out, &mut err),
        LocalVerb::Renew(args) => renew(&args, &mut out, &mut err),
    }
}

/// The adapters of the host a one-verb daemon uses, made once its config loaded and its lock is
/// held.
pub type AdaptersOf<'a> = &'a dyn Fn() -> Result<Adapters, StartError>;

/// `hostkeeper activate` with the daemon of `settings` ([`open`]): its result said on `out` or
/// `err`, and its exit status.
pub fn activate_with(
    args: &ActivateArgs,
    settings: &start::Settings,
    files: &HostFiles,
    adapters: AdaptersOf<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let opened = HostkeeperVersion::parse(env!("CARGO_PKG_VERSION"))
        .map_err(StartError::from)
        .and_then(|own| open(settings, files, own, adapters));
    match opened {
        Ok(local) => activate_on(&local.daemon, args, out, err),
        Err(error) => said(&error, err),
    }
}

/// A daemon put together for one verb: no boot work, no loop, and its state directory's lock held
/// until this is dropped.
pub struct Local {
    pub daemon: Daemon,
    _lock: fs::File,
}

/// The daemon of `settings` put together for one verb: the daemon config loaded, the state
/// directory's lock taken, and the host's ports over `adapters`, with a prune that keeps every
/// version ([`KeepAll`]). The boot work of a start is not run: the daemon's next start runs it.
pub fn open(
    settings: &start::Settings,
    files: &HostFiles,
    own_version: HostkeeperVersion,
    adapters: AdaptersOf<'_>,
) -> Result<Local, StartError> {
    let config = start::load_config(settings)?;
    let lock = start::lock_state_dir(&settings.state_dir)?;
    let ports = adapters()
        .and_then(|adapters| host_ports(&config, settings, files, own_version, adapters))?;
    let ports = Ports {
        prune: Arc::new(KeepAll),
        ..ports
    };
    let dirs = Dirs {
        state: settings.state_dir.clone(),
        packages: settings.packages_dir.clone(),
        facts: settings.facts_file.clone(),
    };
    Ok(Local {
        daemon: Daemon::new(config, dirs, ports, own_version),
        _lock: lock,
    })
}

/// The prune of a one-verb daemon: it deletes nothing, since it does not know the daemon's
/// `--keep-versions`.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeepAll;

impl Prune for KeepAll {
    fn prune(
        &self,
        _layout: &Layout,
        _protect: &Protected,
        _held: &ServiceGuard<'_>,
    ) -> io::Result<PruneReport> {
        Ok(PruneReport::default())
    }
}

/// Says why `activate` could not begin, and gives its exit status: the daemon's pid, read from
/// the lock file, when the state directory is held.
fn said(error: &StartError, err: &mut dyn Write) -> u8 {
    let line = match error {
        StartError::Locked { path, .. } => format!(
            "{} holds {}: stop the daemon first",
            holder(path),
            path.display()
        ),
        other => other.to_string(),
    };
    drop(writeln!(err, "hostkeeper activate: {line}"));
    error.exit_code()
}

/// Who holds the lock file at `path`: the pid the daemon wrote in it, when it reads.
fn holder(path: &Path) -> String {
    let pid = fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    pid.map_or_else(
        || "another process".to_owned(),
        |pid| format!("the daemon (pid {pid})"),
    )
}

/// `hostkeeper activate` with `daemon`, which holds the state directory: its result said on `out`
/// or `err`, and its exit status.
pub fn activate_on(
    daemon: &Daemon,
    args: &ActivateArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let ctx = daemon.ctx();
    let service = &args.service;
    let reply = force::activate(ctx, service, &args.version, args.pin);
    let link = |name| {
        installed::linked_version(&ctx.dirs.packages, service, name)
            .map_or_else(|| "none".to_owned(), |version| version.to_string())
    };
    let (line, code) = match reply {
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        } => {
            let pinned = match args.pin {
                true => format!(", pinned at {}", args.version),
                false => String::new(),
            };
            let line = format!(
                "{service}: {} is current, {} previous{pinned}",
                link(CURRENT),
                link(PREVIOUS)
            );
            drop(writeln!(out, "{line}"));
            return 0;
        }
        Reply::Finished { reason, .. } => (
            format!(
                "failed: {}: {service} is on {}, and nothing is put back",
                reason.as_deref().unwrap_or("failed"),
                link(CURRENT)
            ),
            EXIT_REFUSED,
        ),
        Reply::Refused { refusal, detail } | Reply::Interrupted { refusal, detail } => (
            format!("refused: {}: {detail}", refusal.token()),
            EXIT_REFUSED,
        ),
        Reply::Running => (format!("refused: {}", Refusal::Busy.token()), EXIT_REFUSED),
    };
    drop(writeln!(err, "hostkeeper activate: {line}"));
    code
}

/// `hostkeeper pin`: the pin written when the host keeps the version, and the exit status.
pub fn pin(args: &PinArgs, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let service = &args.service;
    let mut refused = |refusal: Refusal, why: String| {
        drop(writeln!(
            err,
            "hostkeeper pin: refused: {}: {why}",
            refusal.token()
        ));
        EXIT_REFUSED
    };
    if service.as_str() == SELF_SERVICE {
        let why = format!("{service} updates itself by its own path, which no pin holds");
        return refused(Refusal::SelfService, why);
    }
    let layout = Layout::package(&args.packages_dir.join(service.as_str()));
    let kept = force::kept(&layout).unwrap_or_default();
    if !kept.iter().any(|kept| kept == args.version.as_str()) {
        let why = format!(
            "{service} {} is not kept on this host, which keeps {}",
            args.version,
            force::listing(&args.packages_dir, service)
        );
        return refused(Refusal::NotKept, why);
    }
    match pin::write(&args.state_dir, service, &args.version) {
        Ok(()) => {
            drop(writeln!(out, "{service}: pinned at {}", args.version));
            0
        }
        Err(error) => refused(Refusal::StoreFailed, format!("the pin: {error}")),
    }
}

/// `hostkeeper unpin`: whatever is at the pin's path removed, and the exit status.
pub fn unpin(args: &UnpinArgs, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let service = &args.service;
    match pin::remove(&args.state_dir, service) {
        Ok(true) => {
            drop(writeln!(out, "{service}: unpinned"));
            0
        }
        Ok(false) => {
            drop(writeln!(out, "{service}: not pinned"));
            0
        }
        Err(error) => {
            drop(writeln!(
                err,
                "hostkeeper unpin: refused: {}: the pin: {error}",
                Refusal::StoreFailed.token()
            ));
            EXIT_REFUSED
        }
    }
}

/// `hostkeeper renew`: the renewal asked of the daemon when the host holds a certificate, and the
/// exit status.
pub fn renew(args: &RenewArgs, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let pki = args.pki_dir.display();
    match PkiDir::new(&args.pki_dir).ask_renewal() {
        Ok(Some(generation)) => {
            drop(writeln!(
                out,
                "renewal asked: the daemon renews generation {generation} of {pki} at its next enrollment step"
            ));
            0
        }
        Ok(None) => renew_refused(
            err,
            NOT_ENROLLED,
            &format!("{pki} holds no certificate; the host's enrollment gives it its first"),
        ),
        Err(error) => renew_refused(
            err,
            Refusal::StoreFailed.token(),
            &format!("the renewal ask: {error}"),
        ),
    }
}

/// Says on `err` that `renew` was refused for `token`, and gives the exit status.
fn renew_refused(err: &mut dyn Write, token: &str, why: &str) -> u8 {
    drop(writeln!(err, "hostkeeper renew: refused: {token}: {why}"));
    EXIT_REFUSED
}
