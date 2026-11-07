//! The settings of `hostkeeper-mdm migrate` (docs/design/hostkeeper-mdm.md section 8.3): the
//! old kit's settings file read, and the new kit's written from it.
//!
//! The old file is one of kbf-deploy's `mdm` module, of kbf's `mdm-core` or of jobscheduler's
//! `js-mdm-core`: the keys of [`KEYS`] and kbf-deploy's `MDM_ERASE_NAMESPACE`. [`Old`] reads the
//! few values the move needs; the old kit's other checks are not repeated, because the old file
//! is the one the running stack was installed from.
//!
//! The new file ([`new_settings`]) keeps what names the site and what the stack's state was made
//! under, and takes the new kit's names for the rest:
//!
//! * kept ([`CARRIED`]): the DNS name, the bind address and the LAN, the ports, the slice's caps,
//!   the DEP name (NanoDEP keeps the ABM token, its key pair and its server under it), the
//!   organisation and the profiles' texts, and the ACME settings;
//! * renamed ([`RENAMED`]): the user, the directories, the unit prefix and the slice, the profile
//!   identifier, the SCEP CA's names (a fresh CA is made under its final name), `org_magic`, the
//!   DEP certificate's name and the signature namespace, which take the values [`NEW_USER`],
//!   [`NEW_STATE`], [`NEW_ETC`] and [`NEW_BIN`], the given profile identifier and SCEP CA name,
//!   and the defaults of the new prefix;
//! * dropped ([`DROPPED`]): the keys of the erase path, which no verb of the new kit reads.

use crate::acme::Acme;
use crate::config::{
    ConfigError, KEYS, MdmConfig, bad, check, checked_path, is_name, is_unit_stem, is_user_name,
};
use crate::path::AbsPath;
use crate::settings::Settings;

/// The key kbf-deploy's file has that the new kit replaced with `MDM_SIGNATURE_NAMESPACE`.
pub const ERASE_NAMESPACE: &str = "MDM_ERASE_NAMESPACE";
/// The old kit's unit prefix when its file sets none: kbf-deploy's, which the file of that kit
/// could not set.
pub const OLD_DEFAULT_PREFIX: &str = "kbf-mdm";
/// The new kit's service user (design section 8.2).
pub const NEW_USER: &str = "hostkeeper-mdm";
/// The new kit's state directory.
pub const NEW_STATE: &str = "/var/lib/hostkeeper-mdm";
/// The new kit's directory of unit inputs and secrets.
pub const NEW_ETC: &str = "/etc/hostkeeper-mdm";
/// The new kit's directory of binaries.
pub const NEW_BIN: &str = "/opt/hostkeeper-mdm/bin";

/// The keys whose value the new file keeps.
pub const CARRIED: [&str; 23] = [
    "MDM_DNS_NAME",
    "MDM_BIND",
    "MDM_LAN_CIDR",
    "MDM_NANOHUB_PORT",
    "MDM_SCEP_PORT",
    "MDM_DEP_PORT",
    "MDM_MEMORY_MAX",
    "MDM_CPU_QUOTA",
    "MDM_TASKS_MAX",
    "MDM_DEP_NAME",
    "MDM_ORG",
    "MDM_PROFILE_DISPLAY_NAME",
    "MDM_ADE_PROFILE_NAME",
    "MDM_ADE_SKIP_ITEMS",
    "MDM_LANGUAGE",
    "MDM_REGION",
    "MDM_SCEP_CA_YEARS",
    "MDM_DEP_CERT_DAYS",
    "MDM_ACME_EMAIL",
    "MDM_ACME_SERVER",
    "MDM_ACME_DNS_PROVIDER",
    "MDM_ACME_RESOLVERS",
    "MDM_ACME_PROPAGATION_WAIT",
];

/// The keys the new file sets to the new kit's names, or leaves to the new kit's defaults.
pub const RENAMED: [&str; 13] = [
    "MDM_USER",
    "MDM_STATE",
    "MDM_ETC",
    "MDM_BIN",
    "MDM_PKG_DIR",
    "MDM_UNIT_PREFIX",
    "MDM_SLICE",
    "MDM_PROFILE_ID",
    "MDM_ADE_ORG_MAGIC",
    "MDM_SCEP_CA_CN",
    "MDM_SCEP_CA_OU",
    "MDM_DEP_CERT_CN",
    "MDM_SIGNATURE_NAMESPACE",
];

/// The keys the new file leaves out: the erase path's, which no verb of the new kit reads.
pub const DROPPED: [&str; 4] = [
    "MDM_ERASE_STATE",
    ERASE_NAMESPACE,
    "MDM_ERASE_PRINCIPAL",
    "MDM_ERASE_MAX_AGE",
];

/// Why the move's settings were refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MigrateError {
    /// The old settings file was refused.
    #[error("the old settings: {0}")]
    Old(ConfigError),
    /// The new settings would not be accepted by the new kit.
    #[error("the new settings: {0}")]
    New(ConfigError),
    /// The new settings' ACME keys would not be accepted by `tls` and `verify`.
    #[error(
        "the new settings: {0} (the old file sets ACME keys; --acme-resolvers and --acme-propagation-wait give the two a kbf-deploy file may lack)"
    )]
    Acme(ConfigError),
}

/// What the move reads of the old kit's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Old {
    settings: Settings,
    user: String,
    state: AbsPath,
    etc: AbsPath,
    dep_name: String,
    unit_prefix: String,
}

impl Old {
    /// Check the old kit's settings in `text`: the closed key set of the old kits, and the
    /// values the move reads.
    ///
    /// # Errors
    /// A line that is not `KEY=VALUE` of a known key, a key given twice, or a value the move
    /// reads that is missing or malformed.
    pub fn parse(text: &str) -> Result<Old, MigrateError> {
        Old::read(text).map_err(MigrateError::Old)
    }

    fn read(text: &str) -> Result<Old, ConfigError> {
        let mut keys = KEYS.to_vec();
        keys.push(ERASE_NAMESPACE);
        let settings = Settings::parse(text, &keys)?;
        let user = settings.require("MDM_USER")?.to_owned();
        check("USER", &user, is_user_name(&user), "not a user name")?;
        let state = checked_path("STATE", settings.require("MDM_STATE")?)?;
        let etc = checked_path("ETC", settings.require("MDM_ETC")?)?;
        let dep_name = settings.require("MDM_DEP_NAME")?.to_owned();
        check(
            "DEP_NAME",
            &dep_name,
            is_name(&dep_name, "._-"),
            "not a name of letters, digits, . _ - that starts with a letter or digit",
        )?;
        let unit_prefix = settings.get_or("MDM_UNIT_PREFIX", OLD_DEFAULT_PREFIX)?;
        if !is_unit_stem(&unit_prefix) {
            return Err(bad(
                "UNIT_PREFIX",
                &unit_prefix,
                "not lower-case letters, digits and - from a letter",
            ));
        }
        Ok(Old {
            settings,
            user,
            state,
            etc,
            dep_name,
            unit_prefix,
        })
    }

    /// The old service user, which owns the old state.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The old state directory, which the move reads and never changes.
    pub fn state(&self) -> &AbsPath {
        &self.state
    }

    /// The old directory of secrets and unit inputs.
    pub fn etc(&self) -> &AbsPath {
        &self.etc
    }

    /// NanoDEP's name for the ABM token, kept by the new settings.
    pub fn dep_name(&self) -> &str {
        &self.dep_name
    }

    /// The old kit's unit prefix: its file's, else [`OLD_DEFAULT_PREFIX`].
    pub fn unit_prefix(&self) -> &str {
        &self.unit_prefix
    }

    /// The value the old file gives `key`, if it sets one.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.settings.get(key)
    }
}

/// What the new settings take that the old file does not settle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// The final profile identifier (`MDM_PROFILE_ID`).
    pub profile_id: String,
    /// The SCEP CA's common name; the new kit's default when `None`.
    pub scep_ca_cn: Option<String>,
    /// `MDM_ACME_RESOLVERS` in place of the old file's (kbf-deploy's file may lack it).
    pub acme_resolvers: Option<String>,
    /// `MDM_ACME_PROPAGATION_WAIT` in place of the old file's.
    pub acme_propagation_wait: Option<String>,
}

/// The new settings: their text and the config it parses to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migrated {
    /// The text of the new settings file.
    pub text: String,
    /// That text, parsed.
    pub config: MdmConfig,
    /// The ACME settings of that text, when it has any.
    pub acme: Option<Acme>,
}

/// `KEY=value`, the value in double quotes when it holds white space or starts or ends with a
/// quote, so [`Settings::parse`] reads back exactly `value`.
fn line(key: &str, value: &str) -> String {
    let quote =
        value.contains(char::is_whitespace) || value.starts_with('"') || value.ends_with('"');
    match quote {
        true => format!("{key}=\"{value}\"\n"),
        false => format!("{key}={value}\n"),
    }
}

/// The new kit's settings from `old` and `target`, checked by the new kit's own parsers: the
/// whole file by [`MdmConfig::parse`], and its ACME keys, when it has any, by [`Acme::parse`].
///
/// # Errors
/// The new settings are refused by either parser.
pub fn new_settings(old: &Old, target: &Target) -> Result<Migrated, MigrateError> {
    let mut text = String::from(
        "# hostkeeper-mdm: the MDM kit's settings, written by `hostkeeper-mdm migrate` from the\n# old kit's. config.example in hostkeeper-mdm-core documents every key.\n",
    );
    text.push_str(&line("MDM_USER", NEW_USER));
    text.push_str(&line("MDM_STATE", NEW_STATE));
    text.push_str(&line("MDM_ETC", NEW_ETC));
    text.push_str(&line("MDM_BIN", NEW_BIN));
    text.push_str(&line("MDM_PROFILE_ID", &target.profile_id));
    if let Some(cn) = &target.scep_ca_cn {
        text.push_str(&line("MDM_SCEP_CA_CN", cn));
    }
    for key in CARRIED {
        let given = match key {
            "MDM_ACME_RESOLVERS" => target.acme_resolvers.as_deref(),
            "MDM_ACME_PROPAGATION_WAIT" => target.acme_propagation_wait.as_deref(),
            _ => None,
        };
        if let Some(value) = given.or(old.get(key)) {
            text.push_str(&line(key, value));
        }
    }
    let config = MdmConfig::parse(&text).map_err(MigrateError::New)?;
    let acme = match text.lines().any(|l| l.starts_with("MDM_ACME_")) {
        true => Some(Acme::parse(&text).map_err(MigrateError::Acme)?),
        false => None,
    };
    Ok(Migrated { text, config, acme })
}
