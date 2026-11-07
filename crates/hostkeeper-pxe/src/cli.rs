//! The command line of `hostkeeper-pxe`: one verb so far, `fetch` ([`crate::fetch`]).
//!
//! ```text
//! hostkeeper-pxe fetch --release <https URL> --dir <new directory> --daemon-config <file>
//!     [--platform linux-x86_64|linux-arm64]... [--ca-bundle <file>] [--gh <file>]
//!     [--trusted-root <file>] [--download-timeout-secs <n>]
//! ```
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
//! Exit status 0 when the directory is sealed (one line on standard output names it), 1 when the
//! fetch is refused (one line on standard error says why, and the directory is gone), 2 for a
//! command line that does not parse. Configuration is flags only; nothing is read from the
//! environment.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_package::gh::{GhVerifier, OwnedBy, SystemRunner};
use hostkeeper_package::net::{Timeouts, UreqTransport};
use hostkeeper_steps::{Clock, ProcBootId, SystemClock};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use crate::fetch::{GH_HOME, Ports, Request, VERIFY_DIR, fetch};
use crate::platform::BootPlatform;
use crate::seal::Seal;

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
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Debug, Subcommand)]
enum Verb {
    /// Download the boot sets and debs of a release into a new directory, verify them against
    /// the release's attestation and netboot.json, and seal the directory.
    Fetch(FetchFlags),
}

#[derive(Debug, clap::Args)]
struct FetchFlags {
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
/// `gh` and the trusted root.
pub fn main(args: Vec<OsString>) -> ExitCode {
    let mut out = io::stdout();
    let mut err = io::stderr();
    ExitCode::from(run(
        args,
        &|flags| fetch_with(flags, ROOT),
        &mut out,
        &mut err,
    ))
}

/// How the verb is run once its flags parse: [`fetch_with`] on a host.
type Fetch<'a> = &'a dyn Fn(&FetchFlags) -> Result<Seal, String>;

/// Parses `args` and runs the verb through `fetch`; returns the exit status.
fn run(args: Vec<OsString>, fetch: Fetch<'_>, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
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
    let Verb::Fetch(flags) = cli.verb;
    match fetch(&flags) {
        Ok(seal) => {
            drop(writeln!(out, "{}", sealed_line(&flags.dir, &seal)));
            0
        }
        Err(why) => {
            drop(writeln!(err, "hostkeeper-pxe fetch: {why}"));
            1
        }
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
    let verifier = GhVerifier::new(
        flags.gh.clone(),
        flags.trusted_root.clone(),
        flags.dir.join(VERIFY_DIR).join(GH_HOME),
        Box::new(SystemRunner::new(Arc::clone(&clock))),
        Box::new(OwnedBy { uid: owner }),
    );
    let ports = Ports {
        transport: &transport,
        verifier: &verifier,
        clock: &*clock,
    };
    let platforms = platforms_of(&flags.platforms);
    let request = Request {
        release: flags.release.clone(),
        dir: flags.dir.clone(),
        platforms,
        signers: config.trusted_signers,
        attestation_timeout_secs: config.bounds.attestation_timeout_secs,
        max_deb_bytes: config.bounds.max_package_bytes,
        timeouts: Timeouts {
            connect: Duration::from_secs(CONNECT_TIMEOUT_SECS),
            total: Duration::from_secs(flags.download_timeout_secs),
        },
    };
    fetch(&ports, &request).map_err(|e| e.to_string())
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
