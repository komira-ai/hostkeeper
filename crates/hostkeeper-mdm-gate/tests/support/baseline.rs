//! What the baseline tests share: the group and profile names, the baseline file's text, the
//! profile bytes, and a reader of the baseline as root's.

use std::path::Path;

use hostkeeper_mdm_gate::{Baseline, BaselineError, ProfileIdentifier};
use plist::Value;

use super::enrollment::{IDENTIFIER, StatFs};
use super::{dict, plist};

pub const GROUP: &str = "farm-macs";
pub const PROFILE_ID: &str = "com.example.fskit";

pub const DESKTOP: &str = "[[group.entry]]\nremote_desktop = true\n";

/// An unsigned `Configuration` profile `identifier`; `name` tells two versions apart.
pub fn profile_bytes(identifier: &str, name: &str) -> Vec<u8> {
    plist(dict(&[
        ("PayloadType", Value::from("Configuration")),
        ("PayloadIdentifier", Value::from(identifier)),
        (
            "PayloadUUID",
            Value::from("6C1F6A52-2B2E-4F55-9D6B-4B7B3D2F9A10"),
        ),
        ("PayloadVersion", Value::Integer(1.into())),
        ("PayloadDisplayName", Value::from(name)),
        ("PayloadContent", Value::Array(Vec::new())),
    ]))
}

pub fn identifier() -> ProfileIdentifier {
    ProfileIdentifier::parse(IDENTIFIER).expect("an identifier")
}

/// The baseline of `text`, its files read as root's.
pub fn parse(text: &str) -> Result<Baseline, BaselineError> {
    Baseline::parse(text, &StatFs::root(), &identifier())
}

/// A group `group` with the entries `entries`.
pub fn toml(group: &str, entries: &str) -> String {
    format!("[[group]]\nhost_group = \"{group}\"\n{entries}")
}

pub fn profile_entry(path: &Path) -> String {
    format!("[[group.entry]]\nprofile = \"{}\"\n", path.display())
}
