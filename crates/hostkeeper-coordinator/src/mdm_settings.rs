//! The settings of the MDM setup page (`docs/design/coordinator-mdm.md`, section 4.9): each has a
//! default from the start, is changed by `Mdm.PutSetting` and audited as `mdm_setting_set`. A
//! store holds only the settings that were set ([`crate::store::mdm_setup::SetupStore`]); the rest
//! have their default.
//!
//! Each setting says what it takes, so a value is judged once, when it is set: a number of days,
//! a list of them that falls, a number of seconds, or one of a few values. A later release adds names and a store keeps a name this release
//! does not know as it was written.

use std::collections::BTreeMap;

/// What a setting's value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// One whole number between `min` and `max`.
    Days { min: u32, max: u32 },
    /// A name of 1 to 64 printable characters, as Apple shows it.
    Name,
    /// One to five whole numbers from 1 to 3650 that fall, separated by commas (`60,14`).
    FallingDays,
    /// One whole number (seconds, or a count) between `min` and `max`.
    Whole { min: u32, max: u32 },
    /// One of the values listed.
    OneOf(&'static [&'static str]),
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
pub const SETTINGS: [Setting; 19] = [
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
    // The floor, in seconds, between pushes to a Mac that has not connected since the last one
    // (docs/design/coordinator-mdm.md, section 4.3).
    Setting {
        name: "push_retry_s",
        default: "3600",
        shape: Shape::Whole {
            min: 1,
            max: 2_592_000,
        },
    },
    // How long, in seconds, APNs keeps a push for a Mac that is asleep: `apns-expiration` is this
    // long after the push; 0 asks APNs to try once and keep nothing.
    Setting {
        name: "push_expiry_s",
        default: "86400",
        shape: Shape::Whole {
            min: 0,
            max: 2_592_000,
        },
    },
    // `apns-priority` of a push: 10 sends at once, 5 when the power allows.
    Setting {
        name: "push_priority",
        default: "10",
        shape: Shape::OneOf(&["5", "10"]),
    },
    // How long, in seconds, a connection to APNs or to mdmcert.download, and its answer, may take.
    Setting {
        name: "push_timeout_s",
        default: "30",
        shape: Shape::Whole { min: 1, max: 300 },
    },
    // Seconds between two passes of the ABM sync (`docs/design/coordinator-mdm.md`, 4.5): one
    // request when nothing changed.
    Setting {
        name: "abm_sync_s",
        default: "300",
        shape: Shape::Whole {
            min: 60,
            max: 86_400,
        },
    },
    // Seconds one request to ABM may take, connecting to the last byte of the answer.
    Setting {
        name: "abm_request_timeout_s",
        default: "30",
        shape: Shape::Whole { min: 5, max: 300 },
    },
    // How many times a request ABM answered 429 is made again, each after the wait its
    // `Retry-After` names (never longer than one sync interval).
    Setting {
        name: "abm_retries",
        default: "3",
        shape: Shape::Whole { min: 0, max: 10 },
    },
    // The name of the ADE profile the sync defines at ABM.
    Setting {
        name: "ade_profile_name",
        default: "Komira MDM",
        shape: Shape::Name,
    },
    // How many days an identity certificate of a Mac is valid (docs/design/coordinator-mdm.md,
    // section 4.2). A change applies to the identities issued after it.
    Setting {
        name: "identity_days",
        default: "1095",
        shape: Shape::Whole { min: 1, max: 3650 },
    },
    // The days before an identity certificate or the CA expires at which an alert lists them.
    Setting {
        name: "renew_alert_days",
        default: "90",
        shape: Shape::Whole { min: 1, max: 3650 },
    },
    // How long, in seconds, the one-time challenge of an enrollment profile can be redeemed.
    Setting {
        name: "challenge_ttl_s",
        default: "3600",
        shape: Shape::Whole {
            min: 60,
            max: 86_400,
        },
    },
    // How many days the CA of the identities is valid from the day it is made.
    Setting {
        name: "ca_days",
        default: "3650",
        shape: Shape::Whole {
            min: 365,
            max: 7300,
        },
    },
    // The organisation named in the subject of the CA and of each identity, and in the profile.
    Setting {
        name: "identity_org",
        default: "hostkeeper",
        shape: Shape::Name,
    },
    // How many requests to `/enroll` and `/scep` one peer address may make in a minute.
    Setting {
        name: "enroll_rate_per_min",
        default: "60",
        shape: Shape::Whole { min: 1, max: 6000 },
    },
    // How many unverified enrollment requests wait for an operator at once.
    Setting {
        name: "enroll_pending_max",
        default: "256",
        shape: Shape::Whole { min: 1, max: 4096 },
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
            Shape::Whole { min, max } => {
                let number = parse_days(value)?;
                (min..=max).contains(&number).then(|| number.to_string())
            }
            Shape::OneOf(values) => values.contains(&value).then(|| value.to_owned()),
            Shape::Name => {
                let printable = value.chars().all(|c| !c.is_control());
                ((1..=64).contains(&value.chars().count()) && printable).then(|| value.to_owned())
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

/// The value of the setting `name` among the `set` ones, as a number (days, seconds or a
/// count): `fallback` for a name this release does not know or a value that is not one (a store edited by hand).
#[must_use]
pub fn number_of(set: &BTreeMap<String, String>, name: &str, fallback: u32) -> u32 {
    find(name)
        .and_then(|setting| parse_days(value_of(set, setting)))
        .unwrap_or(fallback)
}

/// The numbers of the list setting `name` (`60,14`) among the `set` ones, in order: the default
/// for a name this release does not know, or a value that is not a list (a store edited by hand),
/// and none for a name with no default list.
#[must_use]
pub fn list_of(set: &BTreeMap<String, String>, name: &str) -> Vec<u32> {
    let parse = |text: &str| -> Option<Vec<u32>> {
        text.split(',').map(|d| parse_days(d.trim())).collect()
    };
    find(name)
        .and_then(|setting| parse(value_of(set, setting)).or_else(|| parse(setting.default)))
        .unwrap_or_default()
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
