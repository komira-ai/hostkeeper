//! The two profiles the gate installs on a Mac (docs/design/hostkeeper-mdm.md sections 4 and
//! 5.3), each one `com.apple.ManagedClient.preferences` payload in a `Configuration` profile of
//! system scope:
//!
//! - the config profile, `<identifier>.config`, the same for every Mac: the preference domain
//!   `dev.hostkeeper.config`, whose keys are those of `hostkeeper.toml` (the daemon's config,
//!   design 4.1). The postinstall's `mac-config` writes the file from it. It holds no secret.
//! - the nonce profile, `<identifier>.nonce`, one per device: the domain
//!   `dev.hostkeeper.enrollment`, with `nonce` (the 32 bytes, as data) and `request_id` (the hex
//!   key hash of the pending request it is bound to). A profile with the same identifier
//!   replaces the one on the Mac.
//!
//! In a payload the keys sit where Apple's schema of `com.apple.ManagedClient.preferences`
//! puts them: under the domain, then `Forced`, then `mcx_preference_settings`.
//!
//! The identifier is the farm's (`com.komira.hostkeeper.mdm`), a flag's value and not a
//! constant (design, the decided points): [`ProfileIdentifier`]. The config's keys come from a
//! TOML file the daemon's own loader accepts ([`DaemonProfile::from_toml`]), so a profile the
//! loader would refuse on the Mac is refused when the coordinator starts.

use hostkeeper_config::daemon::{ConfigError, DaemonConfig};
use hostkeeper_enrollment::names::KeyHash;
use plist::{Dictionary, Value};

use crate::ids::CommandUuid;

/// The preference domain of the daemon's config.
pub const CONFIG_DOMAIN: &str = "dev.hostkeeper.config";

/// The preference domain of the nonce.
pub const ENROLLMENT_DOMAIN: &str = "dev.hostkeeper.enrollment";

/// The payload type both profiles carry.
pub const PREFERENCES_PAYLOAD: &str = "com.apple.ManagedClient.preferences";

/// The flag of the profile identifier.
pub const IDENTIFIER_FLAG: &str = "--mdm-profile-identifier";

/// The flag of the daemon config file.
pub const CONFIG_FLAG: &str = "--mdm-daemon-config";

/// The farm's profile identifier: a reverse-DNS name of two to sixteen labels, each 1 to 63
/// ASCII letters, digits and `-`, not starting or ending with `-`, at most 200 characters in
/// all. The two profiles are named under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileIdentifier(String);

/// Why a profile identifier is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{IDENTIFIER_FLAG}: {0:?} is not a reverse-DNS name (two or more labels of letters, digits and -, as com.example.hostkeeper.mdm)"
)]
pub struct IdentifierError(String);

impl ProfileIdentifier {
    /// `text` as the farm's profile identifier.
    ///
    /// # Errors
    /// [`IdentifierError`], naming the text (at most its first 200 characters).
    pub fn parse(text: &str) -> Result<Self, IdentifierError> {
        let label = |label: &str| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        };
        let labels = text.split('.').count();
        let fits = text.len() <= 200 && (2..=16).contains(&labels) && text.split('.').all(label);
        if fits {
            Ok(Self(text.to_owned()))
        } else {
            Err(IdentifierError(text.chars().take(200).collect()))
        }
    }

    /// The identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The config profile's identifier, `<identifier>.config`.
    #[must_use]
    pub fn config(&self) -> String {
        format!("{}.config", self.0)
    }

    /// The nonce profile's identifier, `<identifier>.nonce`.
    #[must_use]
    pub fn nonce(&self) -> String {
        format!("{}.nonce", self.0)
    }
}

/// Why the daemon config file is refused: the daemon's loader refuses it, as it would on the
/// Mac.
#[derive(Debug, thiserror::Error)]
#[error("{CONFIG_FLAG}: {0}")]
pub struct DaemonProfileError(#[from] ConfigError);

/// The daemon config the config profile carries: the keys of `hostkeeper.toml`, as a property
/// list dictionary.
#[derive(Clone, Debug, PartialEq)]
pub struct DaemonProfile(Dictionary);

impl DaemonProfile {
    /// The config in the TOML `text`, checked by the daemon's own loader
    /// ([`DaemonConfig::parse`]) and rendered as a dictionary: a table is a dictionary, and a
    /// string, an integer, a boolean and an array are their property-list forms, the
    /// forms `mac-config` renders back as TOML.
    ///
    /// # Errors
    /// [`DaemonProfileError`]: the loader refuses the text.
    pub fn from_toml(text: &str) -> Result<Self, DaemonProfileError> {
        DaemonConfig::parse(text)?;
        // The loader parsed the same text and read every key of it, so it is a TOML document
        // of tables, strings, numbers, booleans and arrays.
        let table: toml::Table = text.parse().unwrap_or_default();
        Ok(Self(dictionary_of(&table)))
    }

    /// The dictionary.
    #[must_use]
    pub fn settings(&self) -> &Dictionary {
        &self.0
    }

    /// The config profile, identified `identifier.config`, its profile and payload UUIDs
    /// `uuids`.
    #[must_use]
    pub fn profile(&self, identifier: &ProfileIdentifier, uuids: [&CommandUuid; 2]) -> Vec<u8> {
        profile(
            &identifier.config(),
            "hostkeeper config",
            CONFIG_DOMAIN,
            self.0.clone(),
            uuids,
        )
    }
}

fn dictionary_of(table: &toml::Table) -> Dictionary {
    table
        .iter()
        .map(|(key, value)| (key.clone(), value_of(value)))
        .collect()
}

fn value_of(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::from(text.as_str()),
        toml::Value::Integer(number) => Value::Integer((*number).into()),
        toml::Value::Boolean(flag) => Value::Boolean(*flag),
        toml::Value::Array(items) => Value::Array(items.iter().map(value_of).collect()),
        toml::Value::Table(table) => Value::Dictionary(dictionary_of(table)),
        // The loader reads no float and no date, and refuses a key it does not read; kept as
        // its text.
        other @ (toml::Value::Float(_) | toml::Value::Datetime(_)) => {
            Value::from(other.to_string())
        }
    }
}

/// The nonce profile, identified `identifier.nonce`: the `nonce` bytes and the hex of
/// `request_id`, its profile and payload UUIDs `uuids`.
#[must_use]
pub fn nonce_profile(
    identifier: &ProfileIdentifier,
    nonce: &[u8],
    request_id: &KeyHash,
    uuids: [&CommandUuid; 2],
) -> Vec<u8> {
    let mut settings = Dictionary::new();
    settings.insert("nonce".to_owned(), Value::Data(nonce.to_vec()));
    settings.insert("request_id".to_owned(), Value::from(request_id.to_hex()));
    profile(
        &identifier.nonce(),
        "hostkeeper enrollment",
        ENROLLMENT_DOMAIN,
        settings,
        uuids,
    )
}

/// A `Configuration` profile of system scope, identified `identifier`, holding one
/// `com.apple.ManagedClient.preferences` payload that forces `settings` in `domain`.
fn profile(
    identifier: &str,
    name: &str,
    domain: &str,
    settings: Dictionary,
    [profile_uuid, payload_uuid]: [&CommandUuid; 2],
) -> Vec<u8> {
    let mut forced = Dictionary::new();
    forced.insert(
        "mcx_preference_settings".to_owned(),
        Value::Dictionary(settings),
    );
    let mut domains = Dictionary::new();
    domains.insert(
        "Forced".to_owned(),
        Value::Array(vec![Value::Dictionary(forced)]),
    );
    let mut content = Dictionary::new();
    content.insert(domain.to_owned(), Value::Dictionary(domains));
    let mut payload = Dictionary::new();
    payload.insert("PayloadContent".to_owned(), Value::Dictionary(content));
    payload.insert("PayloadDisplayName".to_owned(), Value::from(name));
    payload.insert(
        "PayloadIdentifier".to_owned(),
        Value::from(format!("{identifier}.preferences")),
    );
    payload.insert("PayloadType".to_owned(), Value::from(PREFERENCES_PAYLOAD));
    payload.insert("PayloadUUID".to_owned(), Value::from(payload_uuid.as_str()));
    payload.insert("PayloadVersion".to_owned(), Value::Integer(1.into()));
    let mut root = Dictionary::new();
    root.insert(
        "PayloadContent".to_owned(),
        Value::Array(vec![Value::Dictionary(payload)]),
    );
    root.insert("PayloadDisplayName".to_owned(), Value::from(name));
    root.insert("PayloadIdentifier".to_owned(), Value::from(identifier));
    root.insert("PayloadScope".to_owned(), Value::from("System"));
    root.insert("PayloadType".to_owned(), Value::from("Configuration"));
    root.insert("PayloadUUID".to_owned(), Value::from(profile_uuid.as_str()));
    root.insert("PayloadVersion".to_owned(), Value::Integer(1.into()));
    let mut bytes = Vec::new();
    // Writing a dictionary of strings, integers, reals, booleans, data and arrays into a Vec
    // cannot fail: the writer fails only on the sink's errors, and on a date or a non-string
    // key, neither of which is here.
    let _ = Value::Dictionary(root).to_writer_xml(&mut bytes);
    bytes
}
