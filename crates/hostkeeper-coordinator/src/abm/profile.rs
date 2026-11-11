//! The ADE profile the sync defines at ABM (`docs/design/coordinator-mdm.md`, 4.5): a Mac that is
//! assigned it enrolls, supervised, at `https://<mdm name>/enroll`, and cannot leave the MDM.
//! The fields are the ones of the profile NanoDEP defined for the same server entry (spike abm,
//! 6): supervised, mandatory, not removable, pairing allowed, Setup Assistant not held for the
//! MDM, advancing by itself so that a wiped Mac on Ethernet enrolls with no one at it, and the
//! panes below skipped.
//!
//! The profile is rendered from two settings, so its [`digest`] says whether the one at ABM is the
//! one now wanted: the sync defines a new profile only when the digest differs from the one it
//! stored with the uuid.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// The Setup Assistant panes an enrolled Mac skips.
const SKIP_SETUP_ITEMS: [&str; 18] = [
    "Accessibility",
    "Appearance",
    "AppleID",
    "AppStore",
    "Biometric",
    "Diagnostics",
    "DisplayTone",
    "FileVault",
    "iCloudDiagnostics",
    "iCloudStorage",
    "Intelligence",
    "Privacy",
    "ScreenTime",
    "Siri",
    "TermsOfAddress",
    "TOS",
    "UnlockWithWatch",
    "Wallpaper",
];

/// The profile as ABM takes it. Fields in this order, so that the JSON is always the same bytes.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AdeProfile {
    profile_name: String,
    url: String,
    is_supervised: bool,
    is_mandatory: bool,
    is_mdm_removable: bool,
    await_device_configured: bool,
    auto_advance_setup: bool,
    allow_pairing: bool,
    anchor_certs: Vec<String>,
    skip_setup_items: Vec<&'static str>,
}

impl AdeProfile {
    /// The profile named `name` that sends a Mac to `https://<mdm_name>/enroll`.
    pub(crate) fn new(name: &str, mdm_name: &str) -> Self {
        Self {
            profile_name: name.to_owned(),
            url: format!("https://{mdm_name}/enroll"),
            is_supervised: true,
            is_mandatory: true,
            is_mdm_removable: false,
            await_device_configured: false,
            auto_advance_setup: true,
            allow_pairing: true,
            anchor_certs: Vec::new(),
            skip_setup_items: SKIP_SETUP_ITEMS.to_vec(),
        }
    }

    /// The JSON ABM is sent.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    /// The SHA-256 of the JSON in hex.
    pub(crate) fn digest(&self) -> String {
        hex::encode(Sha256::digest(serde_json::to_vec(self).unwrap_or_default()))
    }
}

/// Whether `name` is a host name an enrollment URL may carry: dot-separated labels of letters,
/// digits and `-` that do not begin or end with `-`, 253 characters at most.
pub(crate) fn is_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
