//! The settings of the MDM setup page (`docs/design/coordinator-mdm.md`, section 4.9): each has a
//! default from the start, is changed by `Mdm.PutSetting` and audited as `mdm_setting_set`. A
//! store holds only the settings that were set ([`crate::store::mdm_setup::SetupStore`]); the rest
//! have their default.
//!
//! Each setting says what it takes, so a value is judged once, when it is set: a number of days,
//! or a list of them that falls. A later release adds names and a store keeps a name this release
//! does not know as it was written.

use std::collections::BTreeMap;

/// What a setting's value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// One whole number between `min` and `max`.
    Days { min: u32, max: u32 },
    /// One whole number between `min` and `max`, which is not a number of days (seconds, tries).
    Count { min: u32, max: u32 },
    /// A name of 1 to 64 printable characters, as Apple shows it.
    Name,
    /// One to five whole numbers from 1 to 3650 that fall, separated by commas (`60,14`).
    FallingDays,
    /// Nothing, or the name of a host group (the rule of a group's name).
    GroupOrNone,
    /// The start of a host id: 0 to 16 characters that, before a serial, make a host id.
    HostIdPrefix,
}

/// One setting: its name, its default and what it takes.
#[derive(Clone, Copy, Debug)]
pub struct Setting {
    /// The name, as the rpcs and the table spell it.
    pub name: &'static str,
    /// The value when none was set.
    pub default: &'static str,
    shape: Shape,
}

/// Every setting this release knows, in the order the status lists them.
pub const SETTINGS: [Setting; 10] = [
    // The days before the push certificate expires at which an alert is raised.
    Setting {
        name: "push_alert_days",
        default: "60,14",
        shape: Shape::FallingDays,
    },
    // The days before the ABM token expires at which an alert is raised.
    Setting {
        name: "abm_alert_days",
        default: "60",
        shape: Shape::FallingDays,
    },
    // The days before the TLS certificate expires at which it is renewed.
    Setting {
        name: "tls_renew_days",
        default: "30",
        shape: Shape::Days { min: 1, max: 365 },
    },
    // How long a replaced secret is kept, so that a wrong upload is undone by uploading again.
    Setting {
        name: "secret_grace_days",
        default: "7",
        shape: Shape::Days { min: 0, max: 365 },
    },
    // Seconds between two passes of the ABM sync (`docs/design/coordinator-mdm.md`, 4.5): one
    // request when nothing changed.
    Setting {
        name: "abm_sync_s",
        default: "300",
        shape: Shape::Count {
            min: 60,
            max: 86_400,
        },
    },
    // Seconds one request to ABM may take, connecting to the last byte of the answer.
    Setting {
        name: "abm_request_timeout_s",
        default: "30",
        shape: Shape::Count { min: 5, max: 300 },
    },
    // How many times a request ABM answered 429 is made again, each after the wait its
    // `Retry-After` names (never longer than one sync interval).
    Setting {
        name: "abm_retries",
        default: "3",
        shape: Shape::Count { min: 0, max: 10 },
    },
    // The name of the ADE profile the sync defines at ABM.
    Setting {
        name: "ade_profile_name",
        default: "Komira MDM",
        shape: Shape::Name,
    },
    // The host group a Mac of our ABM joins when its inventory record names none
    // (`docs/design/coordinator-mdm.md`, 4.6). Empty: none, and such a Mac waits for an operator
    // (`no_group`).
    Setting {
        name: "default_group",
        default: "",
        shape: Shape::GroupOrNone,
    },
    // What a Mac's host id starts with when its inventory record names no host id: the prefix
    // and the serial in lower case.
    Setting {
        name: "host_id_prefix",
        default: "mac-",
        shape: Shape::HostIdPrefix,
    },
];

/// The setting called `name`, if this release knows it.
#[must_use]
pub fn find(name: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|setting| setting.name == name)
}

impl Setting {
    /// `value` as the store keeps it, if the setting takes it: surrounding space dropped and no
    /// space inside.
    #[must_use]
    pub fn check(&self, value: &str) -> Option<String> {
        let value = value.trim();
        match self.shape {
            Shape::Days { min, max } => {
                let days = parse_days(value)?;
                (min..=max).contains(&days).then(|| days.to_string())
            }
            Shape::Count { min, max } => {
                let count = parse_days(value)?;
                (min..=max).contains(&count).then(|| count.to_string())
            }
            Shape::Name => {
                let printable = value.chars().all(|c| !c.is_control());
                ((1..=64).contains(&value.chars().count()) && printable).then(|| value.to_owned())
            }
            Shape::GroupOrNone => {
                let is_group = value.parse::<hostkeeper_wire::RoleName>().is_ok()
                    && hostkeeper_enrollment::names::Role::parse(value).is_ok();
                (value.is_empty() || is_group).then(|| value.to_owned())
            }
            Shape::HostIdPrefix => {
                let makes_id =
                    hostkeeper_enrollment::names::HostId::parse(&format!("{value}a")).is_ok();
                (value.chars().count() <= 16 && makes_id).then(|| value.to_owned())
            }
            Shape::FallingDays => {
                let days: Option<Vec<u32>> =
                    value.split(',').map(|d| parse_days(d.trim())).collect();
                let days = days?;
                let falls = days.windows(2).all(|pair| pair[0] > pair[1]);
                let in_range = days.iter().all(|d| (1..=3650).contains(d));
                (days.len() <= 5 && falls && in_range).then(|| {
                    days.iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                })
            }
        }
    }
}

/// A whole number of days written with digits only.
fn parse_days(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The value of the setting `name` among the `set` ones: what was set, or its default.
#[must_use]
pub fn value_of<'a>(set: &'a BTreeMap<String, String>, setting: &'a Setting) -> &'a str {
    set.get(setting.name)
        .map_or(setting.default, String::as_str)
}

/// The days of the setting `name` among the `set` ones, as a number: `fallback` for a name this
/// release does not know or a value that is not one (a store edited by hand).
#[must_use]
pub fn days_of(set: &BTreeMap<String, String>, name: &str, fallback: u32) -> u32 {
    find(name)
        .and_then(|setting| parse_days(value_of(set, setting)))
        .unwrap_or(fallback)
}

/// The number of the setting `name` among the `set` ones: [`days_of`] for a setting that counts
/// something other than days.
#[must_use]
pub fn number_of(set: &BTreeMap<String, String>, name: &str, fallback: u32) -> u32 {
    days_of(set, name, fallback)
}

/// The text of the setting `name` among the `set` ones: what was set or its default, and none for
/// a name this release does not know.
#[must_use]
pub fn text_of<'a>(set: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    find(name).map(|setting| value_of(set, setting))
}

#[cfg(test)]
#[path = "mdm_settings_tests.rs"]
mod tests;
