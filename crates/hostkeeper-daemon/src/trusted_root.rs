//! The Sigstore trusted root that every package's attestation is checked against (`gh
//! attestation verify --custom-trusted-root`), and how old it is.
//!
//! Two files can hold the root. Hostkeeper's own package carries `files/trusted_root.json`, so
//! each update of hostkeeper brings the root its release was built with. On the host that file
//! is `<packages-dir>/hostkeeper/current/files/trusted_root.json` ([`PACKAGED_TRUSTED_ROOT`]).
//! The second file sits at the host path `crate::cli::SIGSTORE_TRUSTED_ROOT`, which the deb and
//! the rpm install and where an operator can place a root by hand. When both are present, the
//! daemon uses the one written last; a tie in the same second goes to the packaged file. So a
//! root an operator places after a key rotation beats the one an older package carried, and the
//! next update's root beats both. With neither present, `gh` is given the host path, and the
//! check is refused there as it was before any package carried a root.
//!
//! The root's age is the number of seconds since the chosen file's modification time, counted on
//! the daemon's unix clock. A trusted root has no field that says when it was made, so the
//! file's time stands in for that: it is when the host received the root, either the unpack of
//! the hostkeeper version that carries it or an operator's copy. [`TrustedRoot::report`] gives
//! the age and, when the root needs attention, one reason ([`RootProblem`]):
//!
//! - `trusted_root_stale`: the age is over the limit (`--trusted-root-max-age-days`). An age
//!   equal to the limit is not over it.
//! - `trusted_root_missing`: neither file is present.
//! - `trusted_root_unparseable`: the chosen file cannot be read, or is not a stream of JSON
//!   trusted roots. This reason wins over `trusted_root_stale`.
//!
//! The report is a signal for the people who read the admin `Status` and nothing more. No
//! decision of the daemon reads it, and a stale root does not stop a package check.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use hostkeeper_link::UnixClock;

/// Where hostkeeper's own package puts its trusted root, from the packages directory:
/// the `files/trusted_root.json` of the package, through the service's `current` link.
pub const PACKAGED_TRUSTED_ROOT: &str = "hostkeeper/current/files/trusted_root.json";

/// The media type every trusted root of the file starts with; `gh attestation trusted-root`
/// writes it with a `;version=` suffix.
pub const TRUSTED_ROOT_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.trustedroot+json";

/// Seconds in a day, the unit of the age limit's flag.
const DAY_SECS: u64 = 86_400;

/// Why the trusted root needs attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootProblem {
    /// The root is older than the limit.
    Stale,
    /// Neither file of the root is present.
    Missing,
    /// The chosen file cannot be read, or is not trusted roots.
    Unparseable,
}

impl RootProblem {
    /// The reason token the admin `Status` reports.
    pub fn token(self) -> &'static str {
        match self {
            RootProblem::Stale => "trusted_root_stale",
            RootProblem::Missing => "trusted_root_missing",
            RootProblem::Unparseable => "trusted_root_unparseable",
        }
    }
}

/// What [`TrustedRoot::report`] says of the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootReport {
    /// The file the root is read from: the one `gh` is given.
    pub path: PathBuf,
    /// The seconds since that file was last written. `None` when no file is present, and `0`
    /// when the file's time is after the clock's.
    pub age_secs: Option<u64>,
    /// Why the root needs attention; `None` when it does not.
    pub problem: Option<RootProblem>,
}

/// The two files of the trusted root, the age limit, and the clock that ages are counted on.
pub struct TrustedRoot {
    packaged: PathBuf,
    installed: PathBuf,
    max_age_secs: u64,
    clock: Arc<dyn UnixClock>,
}

impl TrustedRoot {
    /// The root of hostkeeper's package at `packaged` and the host's at `installed`; a root older
    /// than `max_age_days` days on `clock` is stale.
    pub fn new(
        packaged: PathBuf,
        installed: PathBuf,
        max_age_days: u64,
        clock: Arc<dyn UnixClock>,
    ) -> TrustedRoot {
        TrustedRoot {
            packaged,
            installed,
            max_age_secs: max_age_days.saturating_mul(DAY_SECS),
            clock,
        }
    }

    /// The file and its modification time of the root in use: of the files present, the one
    /// written last, the packaged one on a tie.
    fn chosen(&self) -> Option<(&Path, u64)> {
        let packaged = written(&self.packaged).map(|at| (self.packaged.as_path(), at));
        let installed = written(&self.installed).map(|at| (self.installed.as_path(), at));
        match (packaged, installed) {
            (Some(packaged), Some(installed)) if installed.1 > packaged.1 => Some(installed),
            (Some(packaged), _) => Some(packaged),
            (None, installed) => installed,
        }
    }

    /// The file `gh` is given: the chosen one ([`TrustedRoot::report`]'s `path`), or the host's
    /// when neither is present.
    pub fn path(&self) -> PathBuf {
        self.chosen()
            .map_or(self.installed.as_path(), |(path, _)| path)
            .to_path_buf()
    }

    /// The root's file, its age and the reason it needs attention, read now.
    pub fn report(&self) -> RootReport {
        let Some((path, at)) = self.chosen() else {
            return RootReport {
                path: self.installed.clone(),
                age_secs: None,
                problem: Some(RootProblem::Missing),
            };
        };
        let age = self.clock.unix_secs().saturating_sub(at);
        let parses = std::fs::read(path).is_ok_and(|bytes| is_trusted_root(&bytes));
        let problem = if !parses {
            Some(RootProblem::Unparseable)
        } else if age > self.max_age_secs {
            Some(RootProblem::Stale)
        } else {
            None
        };
        RootReport {
            path: path.to_path_buf(),
            age_secs: Some(age),
            problem,
        }
    }
}

/// The modification time of the file at `path` in unix seconds (`0` before 1970); `None` when
/// it is not present or its time cannot be read.
fn written(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(|at| at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
}

/// Whether `bytes` are one or more JSON values, each a trusted root: an object whose `mediaType`
/// starts with [`TRUSTED_ROOT_MEDIA_TYPE`]. `gh attestation trusted-root` writes one per line.
pub fn is_trusted_root(bytes: &[u8]) -> bool {
    let mut values = serde_json::Deserializer::from_slice(bytes)
        .into_iter::<serde_json::Value>()
        .peekable();
    values.peek().is_some()
        && values.all(|value| {
            value.ok().is_some_and(|value| {
                value
                    .get("mediaType")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|media| media.starts_with(TRUSTED_ROOT_MEDIA_TYPE))
            })
        })
}
