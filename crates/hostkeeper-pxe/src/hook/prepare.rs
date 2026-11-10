//! The `prepare` step: the settings file is read and checked, the daemon config loaded, the whole
//! `serve` command line they make parsed, and then the boot set of the target version is
//! fetched, checked and sealed into `<boot_dir>/<version>` by [`crate::fetch`].
//!
//! The order, each refusal named by its class and reason:
//!
//! 1. `<boot_dir>` is made and locked (`io_failed`, `locked`).
//! 2. The settings file ([`crate::settings`]), `[serve]` with `--dir <boot_dir>/<version>` as
//!    the parser of `serve` reads it, `[prepare]` as [`PrepareFlags`] reads it, and the daemon
//!    config `serve`'s `--daemon-config` names, loaded with the daemon's loader: `bad_settings`.
//! 3. A directory `<boot_dir>/<version>` that is sealed, of that version, and holds a boot set
//!    for every platform asked for is done. One sealed as another version is removed:
//!    `wrong_version`.
//! 4. The CA bundle is read (`bad_settings`), any other directory is removed, and the fetch runs
//!    on the release the params name, with the platforms, `gh` and trusted root of `[prepare]`
//!    and the signer and bounds of the daemon config. A refusal is `boot_set_refused` (the fetch
//!    has removed what it made), and so is a sealed version other than the target
//!    (`wrong_version`, the directory removed).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_package::gh::{FileTrust, GhVerifier, OwnedBy, Runner, SystemRunner};
use hostkeeper_package::net::{Timeouts, UreqTransport};
use hostkeeper_steps::{Clock, ProcBootId, SystemClock};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use super::boot::remove_tree;
use super::lock::Lock;
use super::params::HookParams;
use super::{Failure, Host};
use crate::cli::{
    CA_BUNDLE, CONNECT_TIMEOUT_SECS, DOWNLOAD_TIMEOUT_SECS, GH, MAX_REDIRECTS, TRUSTED_ROOT,
    one_line, serve_flags,
};
use crate::fetch::{GH_HOME, Ports, Request, VERIFY_DIR, fetch};
use crate::platform::BootPlatform;
use crate::seal::Seal;
use crate::serve::Sealed;
use crate::serve::platforms::linux_of_boot;
use crate::settings;

/// The `[prepare]` table of the settings file, read as flags.
#[derive(Debug, Parser)]
#[command(name = "[prepare]", no_binary_name = true)]
pub(crate) struct PrepareFlags {
    /// A platform to fetch (an array); both when none is named.
    #[arg(long = "platform")]
    pub(crate) platforms: Vec<BootPlatform>,
    /// The PEM bundle of the roots the HTTPS client trusts.
    #[arg(long, default_value = CA_BUNDLE)]
    pub(crate) ca_bundle: PathBuf,
    /// The pinned gh that checks the attestations.
    #[arg(long, default_value = GH)]
    pub(crate) gh: PathBuf,
    /// The Sigstore trusted root gh checks against.
    #[arg(long, default_value = TRUSTED_ROOT)]
    pub(crate) trusted_root: PathBuf,
    /// The bound of each download, in seconds.
    #[arg(long, default_value_t = DOWNLOAD_TIMEOUT_SECS)]
    pub(crate) download_timeout_secs: u64,
}

/// Everything a fetch is made from, once the settings are checked.
pub(crate) struct FetchJob {
    /// The request of [`fetch`].
    pub(crate) request: Request,
    /// The DER roots the HTTPS client trusts.
    pub(crate) roots: Vec<Vec<u8>>,
    /// The `gh` that checks the attestations.
    pub(crate) gh: PathBuf,
    /// The trusted root `gh` checks against.
    pub(crate) trusted_root: PathBuf,
}

/// Why a step's settings are refused: class `environment`, reason `bad_settings`.
fn bad_settings(text: String) -> Failure {
    Failure::environment("bad_settings", text)
}

/// Writes one line of the step to `err`.
fn say(err: &mut dyn Write, text: &str) {
    drop(writeln!(err, "pxe prepare: {text}"));
}

/// Runs the step.
pub(super) fn prepare(
    host: &Host<'_>,
    params: &HookParams,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let _lock = Lock::create(&params.boot_dir)?;
    let version = params.target.as_str();
    let dir = params.boot_dir.join(version);
    let file = settings::read(&params.settings, host.owner).map_err(bad_settings)?;
    let serve = serve_flags(&params.settings, &file.serve, &dir)
        .map_err(|e| bad_settings(format!("{}: [serve]: {e}", params.settings.display())))?;
    let flags = PrepareFlags::try_parse_from(&file.prepare).map_err(|e| {
        bad_settings(format!(
            "{}: [prepare]: {}",
            params.settings.display(),
            one_line(&e)
        ))
    })?;
    let config = DaemonConfig::load(&serve.daemon_config, host.owner)
        .map_err(|e| bad_settings(format!("{}: {e}", serve.daemon_config.display())))?;
    let platforms = platforms_of(&flags.platforms);
    let removal = |dir: &Path| remove_tree(dir).map_err(|e| Failure::io(dir, &e));
    if let Ok(sealed) = Sealed::open(&dir) {
        if sealed.version() != version {
            removal(&dir)?;
            return Err(wrong_version(&dir, sealed.version(), version));
        }
        if platforms
            .iter()
            .all(|p| sealed.set(linux_of_boot(*p)).is_some())
        {
            say(err, &format!("{} is already sealed", dir.display()));
            return Ok(());
        }
    }
    let roots = tls_roots(&flags.ca_bundle).map_err(bad_settings)?;
    removal(&dir)?;
    let job = job_of(&flags, &params.release, &dir, config, roots, platforms);
    let seal =
        (host.fetch)(&job, host.owner).map_err(|why| Failure::release("boot_set_refused", why))?;
    if seal.version != version {
        removal(&dir)?;
        return Err(wrong_version(&dir, &seal.version, version));
    }
    say(err, &sealed_line(&dir, &seal));
    Ok(())
}

/// The refusal of a boot set of another version than the one installed.
fn wrong_version(dir: &Path, found: &str, wanted: &str) -> Failure {
    Failure::release(
        "wrong_version",
        format!(
            "{} holds the boot sets of hostkeeper {found}, not {wanted}",
            dir.display()
        ),
    )
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

/// The fetch of `release` into `dir` that `flags` and the loaded daemon config ask for.
fn job_of(
    flags: &PrepareFlags,
    release: &str,
    dir: &Path,
    config: DaemonConfig,
    roots: Vec<Vec<u8>>,
    platforms: BTreeSet<BootPlatform>,
) -> FetchJob {
    FetchJob {
        request: Request {
            release: release.to_owned(),
            dir: dir.to_owned(),
            platforms,
            signers: config.trusted_signers,
            attestation_timeout_secs: config.bounds.attestation_timeout_secs,
            max_deb_bytes: config.bounds.max_package_bytes,
            timeouts: Timeouts {
                connect: Duration::from_secs(CONNECT_TIMEOUT_SECS),
                total: Duration::from_secs(flags.download_timeout_secs),
            },
        },
        roots,
        gh: flags.gh.clone(),
        trusted_root: flags.trusted_root.clone(),
    }
}

/// The platforms a fetch of `named` (the `platform` array) takes: both when none is named.
fn platforms_of(named: &[BootPlatform]) -> BTreeSet<BootPlatform> {
    match named.is_empty() {
        true => BootPlatform::ALL.into_iter().collect(),
        false => named.iter().copied().collect(),
    }
}

/// Makes the host's ports (HTTPS over `job.roots`, `gh` run as the host, the host's clock) and
/// fetches; `owner` must own `gh` and the trusted root.
pub(crate) fn fetch_with(job: &FetchJob, owner: u32) -> Result<Seal, String> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new(Box::new(ProcBootId::host())));
    let transport = UreqTransport::following_redirects(&job.roots, MAX_REDIRECTS);
    let verifier = verifier_of(
        job,
        Box::new(SystemRunner::new(Arc::clone(&clock))),
        Box::new(OwnedBy { uid: owner }),
    );
    let ports = Ports {
        transport: &transport,
        verifier: &verifier,
        clock: &*clock,
    };
    fetch(&ports, &job.request).map_err(|e| e.to_string())
}

/// The daemon's verifier as the job names it: its `gh` run by `runner` against its trusted root,
/// in `gh`'s home under the new directory, each file checked by `trust` before each run.
pub(super) fn verifier_of(
    job: &FetchJob,
    runner: Box<dyn Runner>,
    trust: Box<dyn FileTrust>,
) -> GhVerifier {
    GhVerifier::new(
        job.gh.clone(),
        job.trusted_root.clone(),
        job.request.dir.join(VERIFY_DIR).join(GH_HOME),
        runner,
        trust,
    )
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
            "ca-bundle {}: no certificate could be read",
            path.display()
        )),
        false => Ok(roots),
    }
}
