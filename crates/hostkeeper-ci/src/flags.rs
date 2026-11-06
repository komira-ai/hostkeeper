//! The flags of `hostkeeper ci set-desired` and their checks. Every flag is checked before the
//! first token is requested; a bad one is a usage error (exit 2) that requests nothing and
//! writes no output.

use std::io::Read as _;
use std::path::{Path, PathBuf};

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
    /// The SHA-256 of the one package CI registered for the version: 64 lower-case hex
    /// characters. Every host is ordered that package, so it is for a fleet of one platform. Given
    /// either this, or `--release-index-file` with `--release-index-digest`; an empty value is
    /// the same as the flag absent.
    #[arg(long)]
    pub package_digest: Option<String>,
    /// The file that holds the release index of the version (`index.json`), at most 3 KiB of
    /// UTF-8 (so that the request, the index in it, fits the coordinator's 4 KiB), sent byte for byte: each host is ordered the package its platform has in it. Comes
    /// with `--release-index-digest`; an empty value is the same as the flag absent.
    #[arg(long)]
    pub release_index_file: Option<PathBuf>,
    /// The SHA-256 of the release index: 64 lower-case hex characters. The coordinator checks it
    /// against the index; this client does not hash or parse the index. An empty value is the
    /// same as the flag absent.
    #[arg(long)]
    pub release_index_digest: Option<String>,
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

/// The most bytes of a release index the client reads.
pub(crate) const MAX_INDEX_BYTES: usize = 3 * 1024;

/// What the desired version names as its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Release {
    /// One package for every host: its SHA-256.
    Package(String),
    /// The release index, byte for byte, and its SHA-256 as the caller states it.
    Index { text: String, digest: String },
}

/// The flags, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub service: String,
    pub version: String,
    pub release: Release,
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
    #[error(
        "give either --package-digest, or --release-index-file together with --release-index-digest"
    )]
    Release,
    #[error("--package-digest must be 64 lower-case hexadecimal characters")]
    PackageDigest,
    #[error("--release-index-digest must be 64 lower-case hexadecimal characters")]
    IndexDigest,
    #[error("--release-index-file {}: {reason}", path.display())]
    IndexFile {
        path: PathBuf,
        reason: IndexFileError,
    },
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

/// Why the release index file cannot be used.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IndexFileError {
    #[error("cannot be read ({0})")]
    Read(std::io::Error),
    #[error("is larger than {MAX_INDEX_BYTES} bytes")]
    TooLarge,
    #[error("is not UTF-8")]
    NotUtf8,
}

/// Whether `text` is 64 lower-case hexadecimal characters.
fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The text of the file at `path`, byte for byte: nothing is added or dropped.
fn read_index(path: &Path) -> Result<String, IndexFileError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_INDEX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(IndexFileError::Read)?;
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(IndexFileError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| IndexFileError::NotUtf8)
}

/// The release the flags name: exactly one of a package digest, or an index file with its digest.
/// A flag given empty is a flag absent. The index file is read here, before any token.
fn release(flags: &SetDesiredFlags) -> Result<Release, UsageError> {
    fn present(value: Option<&String>) -> Option<&String> {
        value.filter(|v| !v.is_empty())
    }
    let package = present(flags.package_digest.as_ref());
    let file = flags
        .release_index_file
        .as_deref()
        .filter(|path| !path.as_os_str().is_empty());
    let digest = present(flags.release_index_digest.as_ref());
    match (package, file, digest) {
        (Some(package), None, None) if is_digest(package) => Ok(Release::Package(package.clone())),
        (Some(_), None, None) => Err(UsageError::PackageDigest),
        (None, Some(_), Some(digest)) if !is_digest(digest) => Err(UsageError::IndexDigest),
        (None, Some(path), Some(digest)) => {
            let text = read_index(path).map_err(|reason| UsageError::IndexFile {
                path: path.to_owned(),
                reason,
            })?;
            Ok(Release::Index {
                text,
                digest: digest.clone(),
            })
        }
        _ => Err(UsageError::Release),
    }
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
    let release = release(flags)?;
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
        release,
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
