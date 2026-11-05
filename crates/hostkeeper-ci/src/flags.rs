//! The flags of `hostkeeper ci set-desired` and their checks. Every flag is checked before the
//! first token is requested; a bad one is a usage error (exit 2) that requests nothing and
//! writes no output.

use std::path::PathBuf;

use rustls_pki_types::CertificateDer;

use crate::token::{RootsError, read_roots};

/// The flags of `hostkeeper ci set-desired`.
#[derive(Debug, Clone, clap::Args)]
pub struct SetDesiredFlags {
    /// The service: 1 to 255 bytes of `A-Z a-z 0-9 . _ -`, not starting with `-` or `.`.
    #[arg(long)]
    pub service: String,
    /// The version to set: the service's grammar and `+`.
    #[arg(long = "version", id = "service_version")]
    pub service_version: String,
    /// The SHA-256 of the package CI registered for the version: 64 lower-case hex characters.
    #[arg(long)]
    pub package_digest: String,
    /// The coordinator's operator listener: `https://<host>[:<port>]`.
    #[arg(long)]
    pub endpoint: String,
    /// The PEM file of the CA the coordinator's certificate must chain to.
    #[arg(long)]
    pub ca_file: PathBuf,
    /// The audience of the OIDC token the coordinator expects.
    #[arg(long)]
    pub audience: String,
    /// Wait for the fleet to converge on the version.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub wait: bool,
    /// How many times to read the convergence at most: 1 to 1000.
    #[arg(long, default_value_t = 180)]
    pub max_polls: u32,
    /// The pause, in seconds, between two reads of the convergence.
    #[arg(long, default_value_t = 10)]
    pub pause_secs: u64,
    /// An existing directory (the runner's temp directory) for the token file and the request
    /// file.
    #[arg(long)]
    pub work_dir: PathBuf,
    /// The job's output file: `desired_version`, `converged` and `reason` are appended to it.
    #[arg(long)]
    pub github_output: Option<PathBuf>,
    /// A PEM file of the root certificates the OIDC token request trusts (the runner's system
    /// bundle).
    #[arg(long)]
    pub web_roots: PathBuf,
}

/// The flags, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub service: String,
    pub version: String,
    pub package_digest: String,
    pub endpoint: String,
    pub ca_file: PathBuf,
    pub audience: String,
    pub wait: bool,
    pub max_polls: u32,
    pub pause_secs: u64,
    pub work_dir: PathBuf,
    pub github_output: Option<PathBuf>,
}

/// Which flag is bad, and how.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UsageError {
    #[error("--service must be 1 to 255 bytes of A-Z a-z 0-9 . _ - and must not start with - or .")]
    Service,
    #[error(
        "--version must be 1 to 255 bytes of A-Z a-z 0-9 . _ - + and must not start with - or ."
    )]
    Version,
    #[error("--package-digest must be 64 lower-case hexadecimal characters")]
    PackageDigest,
    #[error("--endpoint must start with https://")]
    Endpoint,
    #[error("--audience must not be empty or hold a control character")]
    Audience,
    #[error("--max-polls must be from 1 to 1000")]
    MaxPolls,
    #[error("--work-dir {} is not an existing directory", .0.display())]
    WorkDir(PathBuf),
    #[error("--web-roots: {0}")]
    WebRoots(RootsError),
}

/// Whether `text` is a name: 1 to 255 bytes of letters, digits, `.`, `_` and `-` (and `+` when
/// `plus`), not starting with `-` or `.`.
fn is_name(text: &str, plus: bool) -> bool {
    (1..=255).contains(&text.len())
        && !text.starts_with(['-', '.'])
        && text.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') || (plus && b == b'+')
        })
}

/// Whether `text` is a service name.
pub(crate) fn is_service(text: &str) -> bool {
    is_name(text, false)
}

/// Whether `text` is a version.
pub(crate) fn is_version(text: &str) -> bool {
    is_name(text, true)
}

/// Checks the flags, and reads the root certificates of `--web-roots`.
pub(crate) fn validate(
    flags: &SetDesiredFlags,
) -> Result<(Settings, Vec<CertificateDer<'static>>), UsageError> {
    if !is_service(&flags.service) {
        return Err(UsageError::Service);
    }
    if !is_version(&flags.service_version) {
        return Err(UsageError::Version);
    }
    let digest = &flags.package_digest;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(UsageError::PackageDigest);
    }
    if !flags.endpoint.starts_with("https://") {
        return Err(UsageError::Endpoint);
    }
    if flags.audience.is_empty() || flags.audience.chars().any(char::is_control) {
        return Err(UsageError::Audience);
    }
    if !(1..=1000).contains(&flags.max_polls) {
        return Err(UsageError::MaxPolls);
    }
    if !flags.work_dir.is_dir() {
        return Err(UsageError::WorkDir(flags.work_dir.clone()));
    }
    let roots = read_roots(&flags.web_roots).map_err(UsageError::WebRoots)?;
    let settings = Settings {
        service: flags.service.clone(),
        version: flags.service_version.clone(),
        package_digest: flags.package_digest.clone(),
        endpoint: flags.endpoint.clone(),
        ca_file: flags.ca_file.clone(),
        audience: flags.audience.clone(),
        wait: flags.wait,
        max_polls: flags.max_polls,
        pause_secs: flags.pause_secs,
        work_dir: flags.work_dir.clone(),
        github_output: flags.github_output.clone(),
    };
    Ok((settings, roots))
}
