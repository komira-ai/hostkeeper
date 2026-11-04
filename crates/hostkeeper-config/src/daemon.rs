//! The daemon config: `/etc/hostkeeper/hostkeeper.toml`, the host's only configuration and only
//! trust root (docs/design/hostkeeper-design.md, section 4.1).
//!
//! ```toml
//! coordinator     = "https://coordinator.example:8443"
//! ca_sha256       = "<SHA-256 of the farm CA certificate>"
//! release_channel = "https://releases.example"
//!
//! [trusted_signers]
//! organisation  = "komira-ai"
//! workflow      = ".github/workflows/release.yml"
//! refs          = ["refs/tags/*"]
//! hosted_runner = true
//!
//! [bounds]
//! max_package_bytes        = 536870912
//! max_unpacked_bytes       = 2147483648
//! max_entries              = 20000
//! fetch_timeout_secs       = 2100
//! attestation_timeout_secs = 60
//! force_start_secs         = 60
//! ```
//!
//! Every table refuses a key it does not know, and the error names the key and its table. Every
//! key is required except those of `[bounds]`, which default to the values above; a missing
//! `[trusted_signers]` is refused, because there is no unsigned mode. There is no list of
//! services and no per-service key: a host runs what the coordinator assigns it.

use std::path::Path;

use crate::table::{self, Fields, Problem, Shape};
use crate::trust::{TrustError, read_trusted};
use crate::url;

/// The slowest rate a package fetch is allowed to need: 256 KiB/s. A config whose
/// `max_package_bytes` cannot be read in `fetch_timeout_secs` at this rate is refused.
pub const MIN_FETCH_BYTES_PER_SEC: u64 = 262_144;

const SIGNERS: &[(&str, Shape)] = &[
    ("organisation", Shape::Value),
    ("workflow", Shape::Value),
    ("refs", Shape::Value),
    ("hosted_runner", Shape::Value),
];

const BOUNDS: &[(&str, Shape)] = &[
    ("max_package_bytes", Shape::Value),
    ("max_unpacked_bytes", Shape::Value),
    ("max_entries", Shape::Value),
    ("fetch_timeout_secs", Shape::Value),
    ("attestation_timeout_secs", Shape::Value),
    ("force_start_secs", Shape::Value),
];

const TOP: &[(&str, Shape)] = &[
    ("coordinator", Shape::Value),
    ("ca_sha256", Shape::Value),
    ("release_channel", Shape::Value),
    ("trusted_signers", Shape::Table(SIGNERS)),
    ("bounds", Shape::Table(BOUNDS)),
];

/// Why the daemon config was refused. The daemon does not start on any of them, and nothing on
/// the host changes.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("the daemon config is not a file the host trusts: {0}")]
    Untrusted(#[from] TrustError),
    #[error("the daemon config is not UTF-8 text")]
    Unreadable,
    #[error("the daemon config is not TOML: line {line}: {message}")]
    Syntax { line: usize, message: String },
    /// `table` is `""` for the top level.
    #[error("the daemon config has the unknown key {key:?} in the table {table:?}")]
    UnknownKey { table: String, key: String },
    /// `key` is dotted (`bounds.max_entries`).
    #[error("the daemon config's {key} {why}")]
    Invalid { key: String, why: String },
    #[error(
        "bounds.max_package_bytes {max_package_bytes} cannot be fetched in bounds.fetch_timeout_secs {fetch_timeout_secs} at 256 KiB/s"
    )]
    FetchBoundTooSmall {
        max_package_bytes: u64,
        fetch_timeout_secs: u64,
    },
}

impl From<Problem> for ConfigError {
    fn from(problem: Problem) -> ConfigError {
        match problem {
            Problem::Unknown { table, key } => ConfigError::UnknownKey { table, key },
            Problem::Invalid { key, why } => ConfigError::Invalid { key, why },
        }
    }
}

/// An `https://` URL: a host, an optional port and an optional path; no user name, query or
/// fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpsUrl(String);

impl HttpsUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A SHA-256 as 64 hex digits, kept in lowercase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sha256Hex(String);

impl Sha256Hex {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whose attestation a package must carry: one organisation-level entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedSigners {
    /// The GitHub organisation the attested repository belongs to.
    pub organisation: String,
    /// The signing workflow, as a path in the repository.
    pub workflow: String,
    /// The refs the workflow may run on, as globs (`refs/tags/*`); at least one.
    pub refs: Vec<String>,
    /// Whether the workflow must have run on a hosted runner.
    pub hosted_runner: bool,
}

/// The bounds of a fetch and an unpack (design section 5.5). Each is from 1 up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub max_package_bytes: u64,
    pub max_unpacked_bytes: u64,
    pub max_entries: u64,
    /// The bound of the whole fetch.
    pub fetch_timeout_secs: u64,
    pub attestation_timeout_secs: u64,
    pub force_start_secs: u64,
}

impl Default for Bounds {
    /// The values of the design's example.
    fn default() -> Bounds {
        Bounds {
            max_package_bytes: 536_870_912,
            max_unpacked_bytes: 2_147_483_648,
            max_entries: 20_000,
            fetch_timeout_secs: 2_100,
            attestation_timeout_secs: 60,
            force_start_secs: 60,
        }
    }
}

impl Bounds {
    /// The rules between the bounds: none is 0, the unpacked bound is at least the package
    /// bound, and `max_package_bytes` can be fetched in `fetch_timeout_secs` at
    /// [`MIN_FETCH_BYTES_PER_SEC`]. The product is computed with checked arithmetic; a timeout
    /// whose product does not fit in 64 bits is refused.
    pub fn check(&self) -> Result<(), ConfigError> {
        let invalid = |key: &str, why: &str| ConfigError::Invalid {
            key: format!("bounds.{key}"),
            why: why.to_owned(),
        };
        let all = [
            ("max_package_bytes", self.max_package_bytes),
            ("max_unpacked_bytes", self.max_unpacked_bytes),
            ("max_entries", self.max_entries),
            ("fetch_timeout_secs", self.fetch_timeout_secs),
            ("attestation_timeout_secs", self.attestation_timeout_secs),
            ("force_start_secs", self.force_start_secs),
        ];
        if let Some((key, _)) = all.iter().find(|(_, value)| *value == 0) {
            return Err(invalid(key, "is 0; every bound is from 1 up"));
        }
        if self.max_unpacked_bytes < self.max_package_bytes {
            return Err(invalid(
                "max_unpacked_bytes",
                "is less than max_package_bytes",
            ));
        }
        let fetchable = self
            .fetch_timeout_secs
            .checked_mul(MIN_FETCH_BYTES_PER_SEC)
            .ok_or_else(|| {
                invalid(
                    "fetch_timeout_secs",
                    "is too large: the bytes it allows at 256 KiB/s do not fit in 64 bits",
                )
            })?;
        if self.max_package_bytes > fetchable {
            return Err(ConfigError::FetchBoundTooSmall {
                max_package_bytes: self.max_package_bytes,
                fetch_timeout_secs: self.fetch_timeout_secs,
            });
        }
        Ok(())
    }
}

/// The daemon config, loaded and checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfig {
    /// Where the heartbeat goes.
    pub coordinator: HttpsUrl,
    /// The SHA-256 of the CA certificate the coordinator's certificate chains to.
    pub ca_sha256: Sha256Hex,
    /// Where packages are fetched from.
    pub release_channel: HttpsUrl,
    pub trusted_signers: TrustedSigners,
    pub bounds: Bounds,
}

fn https(fields: &Fields<'_>, key: &str) -> Result<HttpsUrl, Problem> {
    let text = fields.string(key)?;
    let parts = url::parts(text, false).map_err(|why| fields.invalid(key, why))?;
    if parts.scheme != "https" {
        return Err(fields.invalid(key, "is not an https:// URL"));
    }
    Ok(HttpsUrl(text.to_owned()))
}

fn sha256(fields: &Fields<'_>, key: &str) -> Result<Sha256Hex, Problem> {
    let text = fields.string(key)?;
    let hex = (text.len() == 64) & text.bytes().all(|b| b.is_ascii_hexdigit());
    hex.then(|| Sha256Hex(text.to_ascii_lowercase()))
        .ok_or_else(|| fields.invalid(key, "is not a SHA-256 of 64 hex digits"))
}

/// A string from 1 to 256 bytes with no control character or white space.
fn word<'a>(fields: &Fields<'a>, key: &str) -> Result<&'a str, Problem> {
    let text = fields.string(key)?;
    let ok = (1..=256).contains(&text.len()) & text.bytes().all(|b| b.is_ascii_graphic());
    ok.then_some(text).ok_or_else(|| {
        fields.invalid(
            key,
            "is not 1 to 256 printable characters without white space",
        )
    })
}

fn signers(top: &Fields<'_>) -> Result<TrustedSigners, Problem> {
    let table = top
        .table("trusted_signers")?
        .ok_or_else(|| top.invalid("trusted_signers", "is missing; there is no unsigned mode"))?;
    let fields = Fields::new("trusted_signers", table);
    let organisation = word(&fields, "organisation")?.to_owned();
    let workflow = word(&fields, "workflow")?.to_owned();
    let refs = fields.strings("refs")?;
    if refs.is_empty() {
        return Err(fields.invalid("refs", "is empty; at least one ref glob is needed"));
    }
    if let Some(bad) = refs.iter().find(|text| {
        !((1..=256).contains(&text.len()) & text.bytes().all(|b| b.is_ascii_graphic()))
    }) {
        return Err(fields.invalid(
            "refs",
            format!(
                "holds {bad:?}, which is not 1 to 256 printable characters without white space"
            ),
        ));
    }
    Ok(TrustedSigners {
        organisation,
        workflow,
        refs: refs.into_iter().map(str::to_owned).collect(),
        hosted_runner: fields.boolean("hosted_runner")?,
    })
}

fn bounds(top: &Fields<'_>) -> Result<Bounds, Problem> {
    let empty = toml::Table::new();
    let table = top.table("bounds")?.unwrap_or(&empty);
    let fields = Fields::new("bounds", table);
    let default = Bounds::default();
    Ok(Bounds {
        max_package_bytes: fields.number_or("max_package_bytes", default.max_package_bytes)?,
        max_unpacked_bytes: fields.number_or("max_unpacked_bytes", default.max_unpacked_bytes)?,
        max_entries: fields.number_or("max_entries", default.max_entries)?,
        fetch_timeout_secs: fields.number_or("fetch_timeout_secs", default.fetch_timeout_secs)?,
        attestation_timeout_secs: fields
            .number_or("attestation_timeout_secs", default.attestation_timeout_secs)?,
        force_start_secs: fields.number_or("force_start_secs", default.force_start_secs)?,
    })
}

fn read(document: &toml::Table) -> Result<DaemonConfig, Problem> {
    let top = Fields::new("", document);
    Ok(DaemonConfig {
        coordinator: https(&top, "coordinator")?,
        ca_sha256: sha256(&top, "ca_sha256")?,
        release_channel: https(&top, "release_channel")?,
        trusted_signers: signers(&top)?,
        bounds: bounds(&top)?,
    })
}

impl DaemonConfig {
    /// Reads the config at `path`: a regular file, not a link, owned by `trusted_uid` (root, 0,
    /// on a host) and not writable by the group or others, checked on the open file; then
    /// [`DaemonConfig::parse`].
    pub fn load(path: &Path, trusted_uid: u32) -> Result<DaemonConfig, ConfigError> {
        let bytes = read_trusted(path, trusted_uid)?;
        let text = String::from_utf8(bytes).map_err(|_| ConfigError::Unreadable)?;
        DaemonConfig::parse(&text)
    }

    /// Reads the text of a config, with no check of the file it came from: the syntax, then
    /// every key against the tables that may hold it, then each value, then the bounds.
    pub fn parse(text: &str) -> Result<DaemonConfig, ConfigError> {
        let document =
            table::parse(text).map_err(|(line, message)| ConfigError::Syntax { line, message })?;
        if let Some(problem) = table::unknown_key("", &document, TOP) {
            return Err(problem.into());
        }
        let config = read(&document)?;
        config.bounds.check()?;
        Ok(config)
    }
}
