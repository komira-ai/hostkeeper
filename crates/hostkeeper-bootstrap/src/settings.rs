//! The bootstrap's settings: the farm's two values and every tunable, each a key with a default
//! (docs/design/mac-bootstrap.md, section 4).
//!
//! The values come from one of two places, the first that is there:
//!
//! 1. The managed preferences of the domain `dev.hostkeeper.config` ([`MANAGED_PREFERENCES`]), a
//!    property list that a Blueprint's configuration writes, whose top-level dictionary holds the
//!    keys below.
//! 2. The file [`FALLBACK_FILE`], TOML with the same keys, for a manual install.
//!
//! Either is read as a file root owns that only its owner can write
//! ([`hostkeeper_config::trust::read_trusted`]), so a file another account could change is never a
//! source of the trust root. A source that is there and is refused is an error, never a reason to
//! try the next one: a managed file that is refused must not be hidden by a fallback.
//!
//! ```toml
//! coordinator       = "https://coordinator.example"   # required
//! ca_sha256         = "<64 hex digits>"                     # required
//! release_channel   = "https://packages.komira.ai"
//! team_id           = "U7RTX4XD3X"
//! macos_pkg_signers = []          # leaf SHA-256s; empty: the leaf of the pkg that was checked
//!
//! [trusted_signers]
//! organisation  = "komira-ai"
//! workflow      = ".github/workflows/release.yml"
//! refs          = ["refs/tags/*"]
//! hosted_runner = true
//!
//! # The update account (crate::update_account): all four or none.
//! update_account_name     = "hkupdate"
//! update_account_password = "<its password>"
//! admin_name              = "<the administrator made at Setup Assistant>"
//! admin_password          = "<that administrator's password>"
//!
//! [bounds]
//! fetch_timeout_secs     = 1800
//! connect_timeout_secs   = 30
//! max_package_bytes      = 536870912
//! tool_timeout_secs      = 60
//! installer_timeout_secs = 600
//! wait_secs              = 900
//! retry_secs             = 30
//! ```
//!
//! Every key but the first two has the default shown, but the four keys of the update account,
//! which have none and are given together or not at all ([`crate::update_account`]). The text a source holds is also made into
//! the daemon's config ([`Settings::daemon_config`]) and parsed by the daemon's own loader, so a
//! value the daemon would refuse stops the bootstrap before anything is downloaded.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::{ConfigError, DaemonConfig};
use hostkeeper_config::trust::{TrustError, read_trusted};
use serde::Deserialize;

use crate::update_account::{self, UpdateAccountKeys};

/// Where macOS writes the managed preferences of the domain `dev.hostkeeper.config`.
pub const MANAGED_PREFERENCES: &str = "/Library/Managed Preferences/dev.hostkeeper.config.plist";
/// The file of a manual install.
pub const FALLBACK_FILE: &str = "/Library/Application Support/hostkeeper/bootstrap.toml";
/// The repository the bootstrap fetches hostkeeper's pkg from.
pub const DEFAULT_RELEASE_CHANNEL: &str = "https://packages.komira.ai";
/// The Team ID of the Developer ID Installer identity that signs hostkeeper's pkgs. A default,
/// overridable with the key `team_id`.
pub const DEFAULT_TEAM_ID: &str = "U7RTX4XD3X";

/// The most bytes of a settings source.
const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// The bounds of the bootstrap, in seconds and bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The longest one download may take, its body included.
    pub fetch_timeout_secs: u64,
    /// The longest a connection is waited for.
    pub connect_timeout_secs: u64,
    /// The largest pkg, and so the most bytes written for one.
    pub max_package_bytes: u64,
    /// The longest `pkgutil` is waited for.
    pub tool_timeout_secs: u64,
    /// The longest `installer` is waited for.
    pub installer_timeout_secs: u64,
    /// How long the bootstrap keeps retrying a failed attempt before it exits and lets launchd
    /// start it again: the sum of its pauses.
    pub wait_secs: u64,
    /// The pause between two attempts.
    pub retry_secs: u64,
}

impl Default for Bounds {
    fn default() -> Bounds {
        Bounds {
            fetch_timeout_secs: 1800,
            connect_timeout_secs: 30,
            max_package_bytes: 536_870_912,
            tool_timeout_secs: 60,
            installer_timeout_secs: 600,
            wait_secs: 900,
            retry_secs: 30,
        }
    }
}

/// Whose attestation the daemon's config names; the daemon reads it no more, but its loader
/// requires it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signers {
    pub organisation: String,
    pub workflow: String,
    pub refs: Vec<String>,
    pub hosted_runner: bool,
}

impl Default for Signers {
    fn default() -> Signers {
        Signers {
            organisation: "komira-ai".to_owned(),
            workflow: ".github/workflows/release.yml".to_owned(),
            refs: vec!["refs/tags/*".to_owned()],
            hosted_runner: true,
        }
    }
}

/// The settings of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The coordinator's heartbeat listener, an `https://` URL.
    pub coordinator: String,
    /// The SHA-256 of the farm CA, 64 lower-case hex digits.
    pub ca_sha256: String,
    /// The repository's base URL, without a trailing `/`.
    pub release_channel: String,
    /// The Team ID the pkg's signer must have.
    pub team_id: String,
    /// The leaf fingerprints written to the signers file; empty means the checked pkg's leaf.
    pub macos_pkg_signers: Vec<String>,
    pub trusted_signers: Signers,
    pub bounds: Bounds,
    /// The update account and its administrator; `None` when the source names neither.
    pub update_account: Option<UpdateAccountKeys>,
}

/// Why no settings could be made.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// Neither source is there.
    #[error("no farm values: neither {} nor {} is there", managed.display(), fallback.display())]
    NoSource { managed: PathBuf, fallback: PathBuf },
    /// A source is there and is not one root trusts, or cannot be read.
    #[error("{} is refused: {source}", path.display())]
    Refused {
        path: PathBuf,
        #[source]
        source: TrustError,
    },
    /// A source is larger than the bound.
    #[error("{} is larger than {MAX_SOURCE_BYTES} bytes", path.display())]
    TooLarge { path: PathBuf },
    /// The managed preferences are not a property list with a dictionary at its top.
    #[error("{} is not a property list of one dictionary: {why}", path.display())]
    NotPlist { path: PathBuf, why: String },
    /// A value of the property list has no form in the settings.
    #[error("{} holds {key}, which is {kind}: the settings take strings, numbers, booleans, lists and dictionaries", path.display())]
    Unsupported {
        path: PathBuf,
        key: String,
        kind: &'static str,
    },
    /// The text is not the settings' TOML, or holds a key they do not know.
    #[error("{} is not settings: {why}", path.display())]
    Syntax { path: PathBuf, why: String },
    /// A required key is missing.
    #[error("{} has no {key}", path.display())]
    Missing { path: PathBuf, key: &'static str },
    /// Some of the update account's four keys are given, and `key` is not.
    #[error("{} has no {key}: the update account's four keys are given together or not at all", path.display())]
    Incomplete { path: PathBuf, key: &'static str },
    /// A value breaks its rule.
    #[error("{} {key} {why}", path.display())]
    Invalid {
        path: PathBuf,
        key: &'static str,
        why: String,
    },
    /// The daemon's loader refuses the config the settings make.
    #[error("{} makes a daemon config the daemon would refuse: {source}", path.display())]
    DaemonConfig {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    coordinator: Option<String>,
    ca_sha256: Option<String>,
    release_channel: Option<String>,
    team_id: Option<String>,
    macos_pkg_signers: Option<Vec<String>>,
    trusted_signers: Option<SignersFile>,
    bounds: Option<BoundsFile>,
    // Values, not strings: a value of another type is refused by its key's name alone, never by an
    // error of the TOML reader, which would quote it.
    update_account_name: Option<toml::Value>,
    update_account_password: Option<toml::Value>,
    admin_name: Option<toml::Value>,
    admin_password: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignersFile {
    organisation: Option<String>,
    workflow: Option<String>,
    refs: Option<Vec<String>>,
    hosted_runner: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundsFile {
    fetch_timeout_secs: Option<u64>,
    connect_timeout_secs: Option<u64>,
    max_package_bytes: Option<u64>,
    tool_timeout_secs: Option<u64>,
    installer_timeout_secs: Option<u64>,
    wait_secs: Option<u64>,
    retry_secs: Option<u64>,
}

/// The two sources and the account that must own them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sources {
    pub managed: PathBuf,
    pub fallback: PathBuf,
    /// The owner both files must have: root, 0, on a Mac.
    pub trusted_uid: u32,
}

impl Sources {
    /// The Mac's own sources, owned by root.
    pub fn host() -> Sources {
        Sources {
            managed: PathBuf::from(MANAGED_PREFERENCES),
            fallback: PathBuf::from(FALLBACK_FILE),
            trusted_uid: 0,
        }
    }

    /// The settings of the first source that is there.
    pub fn load(&self) -> Result<Settings, SettingsError> {
        for (path, plist) in [(&self.managed, true), (&self.fallback, false)] {
            let bytes = match read_trusted(path, self.trusted_uid) {
                Ok(bytes) => bytes,
                Err(TrustError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(SettingsError::Refused {
                        path: path.clone(),
                        source,
                    });
                }
            };
            if bytes.len() > MAX_SOURCE_BYTES {
                return Err(SettingsError::TooLarge { path: path.clone() });
            }
            let text = match plist {
                true => plist_as_toml(path, &bytes)?,
                false => String::from_utf8(bytes).map_err(|_| SettingsError::Syntax {
                    path: path.clone(),
                    why: "not UTF-8 text".to_owned(),
                })?,
            };
            return Settings::parse(path, &text);
        }
        Err(SettingsError::NoSource {
            managed: self.managed.clone(),
            fallback: self.fallback.clone(),
        })
    }
}

/// A property list's top-level dictionary as TOML text.
fn plist_as_toml(path: &Path, bytes: &[u8]) -> Result<String, SettingsError> {
    let not_plist = |why: String| SettingsError::NotPlist {
        path: path.to_path_buf(),
        why,
    };
    let value =
        plist::Value::from_reader(Cursor::new(bytes)).map_err(|e| not_plist(e.to_string()))?;
    let plist::Value::Dictionary(dict) = value else {
        return Err(not_plist("the top level is not a dictionary".to_owned()));
    };
    Ok(table(path, &dict, "")?.to_string())
}

fn table(path: &Path, dict: &plist::Dictionary, at: &str) -> Result<toml::Table, SettingsError> {
    dict.iter()
        .map(|(name, value)| {
            let key = match at.is_empty() {
                true => name.clone(),
                false => format!("{at}.{name}"),
            };
            Ok((name.clone(), convert(path, value, &key)?))
        })
        .collect()
}

fn convert(path: &Path, value: &plist::Value, key: &str) -> Result<toml::Value, SettingsError> {
    let unsupported = |kind| SettingsError::Unsupported {
        path: path.to_path_buf(),
        key: key.to_owned(),
        kind,
    };
    match value {
        plist::Value::Dictionary(dict) => table(path, dict, key).map(toml::Value::Table),
        plist::Value::Array(items) => items
            .iter()
            .map(|item| convert(path, item, key))
            .collect::<Result<Vec<_>, _>>()
            .map(toml::Value::Array),
        plist::Value::String(text) => Ok(toml::Value::String(text.clone())),
        plist::Value::Boolean(flag) => Ok(toml::Value::Boolean(*flag)),
        plist::Value::Integer(int) => int
            .as_signed()
            .map(toml::Value::Integer)
            .ok_or_else(|| unsupported("an integer that does not fit 64 bits")),
        plist::Value::Real(_) => Err(unsupported("a real")),
        plist::Value::Date(_) => Err(unsupported("a date")),
        plist::Value::Data(_) => Err(unsupported("data")),
        _ => Err(unsupported("a uid")),
    }
}

impl Settings {
    /// The settings of `text`, read from `path` (which names it in an error).
    pub fn parse(path: &Path, text: &str) -> Result<Settings, SettingsError> {
        let file: File = toml::from_str(text).map_err(|e| SettingsError::Syntax {
            path: path.to_path_buf(),
            why: e.message().to_owned(),
        })?;
        let invalid = |key: &'static str, why: &str| SettingsError::Invalid {
            path: path.to_path_buf(),
            key,
            why: why.to_owned(),
        };
        let coordinator = file.coordinator.ok_or_else(|| SettingsError::Missing {
            path: path.to_path_buf(),
            key: "coordinator",
        })?;
        let ca_sha256 = file.ca_sha256.ok_or_else(|| SettingsError::Missing {
            path: path.to_path_buf(),
            key: "ca_sha256",
        })?;
        let update_account = update_account::keys(
            path,
            [
                file.update_account_name,
                file.update_account_password,
                file.admin_name,
                file.admin_password,
            ],
        )?;
        let team_id = file.team_id.unwrap_or_else(|| DEFAULT_TEAM_ID.to_owned());
        if !is_team_id(&team_id) {
            return Err(invalid(
                "team_id",
                "is not 10 upper-case letters and digits",
            ));
        }
        let macos_pkg_signers = file.macos_pkg_signers.unwrap_or_default();
        if macos_pkg_signers.len() > hostkeeper_install::macos_pkg::signers::MAX_SIGNERS {
            return Err(invalid("macos_pkg_signers", "names more than 4 signers"));
        }
        let signers = file.trusted_signers.map_or_else(Signers::default, |given| {
            let default = Signers::default();
            Signers {
                organisation: given.organisation.unwrap_or(default.organisation),
                workflow: given.workflow.unwrap_or(default.workflow),
                refs: given.refs.unwrap_or(default.refs),
                hosted_runner: given.hosted_runner.unwrap_or(default.hosted_runner),
            }
        });
        let default = Bounds::default();
        let bounds = file.bounds.map_or(default, |given| Bounds {
            fetch_timeout_secs: given
                .fetch_timeout_secs
                .unwrap_or(default.fetch_timeout_secs),
            connect_timeout_secs: given
                .connect_timeout_secs
                .unwrap_or(default.connect_timeout_secs),
            max_package_bytes: given.max_package_bytes.unwrap_or(default.max_package_bytes),
            tool_timeout_secs: given.tool_timeout_secs.unwrap_or(default.tool_timeout_secs),
            installer_timeout_secs: given
                .installer_timeout_secs
                .unwrap_or(default.installer_timeout_secs),
            wait_secs: given.wait_secs.unwrap_or(default.wait_secs),
            retry_secs: given.retry_secs.unwrap_or(default.retry_secs),
        });
        if bounds.retry_secs == 0 {
            return Err(invalid(
                "bounds.retry_secs",
                "is 0; it is at least 1 second",
            ));
        }
        let release_channel = file
            .release_channel
            .unwrap_or_else(|| DEFAULT_RELEASE_CHANNEL.to_owned())
            .trim_end_matches('/')
            .to_owned();
        let settings = Settings {
            coordinator,
            ca_sha256,
            release_channel,
            team_id,
            macos_pkg_signers,
            trusted_signers: signers,
            bounds,
            update_account,
        };
        DaemonConfig::parse(&settings.daemon_config()).map_err(|source| {
            SettingsError::DaemonConfig {
                path: path.to_path_buf(),
                source,
            }
        })?;
        for signer in &settings.macos_pkg_signers {
            if hostkeeper_install::macos_pkg::signers::Fingerprint::parse(signer).is_none() {
                return Err(invalid(
                    "macos_pkg_signers",
                    "names a value that is not 64 hex digits",
                ));
            }
        }
        Ok(settings)
    }

    /// The text of `/etc/hostkeeper/hostkeeper.toml` these settings make: the farm's two values,
    /// the release channel and the signers; every bound is the daemon's own default.
    pub fn daemon_config(&self) -> String {
        let signers = &self.trusted_signers;
        let mut top = toml::Table::new();
        top.insert("coordinator".into(), self.coordinator.clone().into());
        top.insert("ca_sha256".into(), self.ca_sha256.clone().into());
        top.insert(
            "release_channel".into(),
            self.release_channel.clone().into(),
        );
        let mut trusted = toml::Table::new();
        trusted.insert("organisation".into(), signers.organisation.clone().into());
        trusted.insert("workflow".into(), signers.workflow.clone().into());
        trusted.insert(
            "refs".into(),
            toml::Value::Array(signers.refs.iter().cloned().map(Into::into).collect()),
        );
        trusted.insert("hosted_runner".into(), signers.hosted_runner.into());
        top.insert("trusted_signers".into(), trusted.into());
        format!("{HEADER}{top}")
    }
}

/// The comment the written config starts with.
pub const HEADER: &str = "# Written by hostkeeper-bootstrap (docs/design/mac-bootstrap.md).\n";

fn is_team_id(text: &str) -> bool {
    text.len() == 10
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() | b.is_ascii_digit())
}
