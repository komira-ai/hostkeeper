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
    /// One to five whole numbers from 1 to 3650 that fall, separated by commas (`60,14`).
    FallingDays,
    /// One whole number between `min` and `max`, of seconds, bytes or a count.
    Whole { min: u64, max: u64 },
    /// One of the values listed.
    OneOf(&'static [&'static str]),
    /// A DNS name of two or more labels, kept in lower case; empty clears it.
    DnsName,
    /// An `https://` URL with a host and no space, up to 512 bytes.
    HttpsUrl,
    /// An email address: one `@`, text each side, no space, up to 254 bytes; empty clears it.
    Email,
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
pub const SETTINGS: [Setting; 23] = [
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
    // The DNS zone the MDM name lives in at the DNS provider, for the ACME challenge record
    // (docs/design/coordinator-mdm.md, 4.10). Never worked out from the name: `mdm.internal.example.net`
    // is in the zone `example.net`. Empty: the certificate is not ordered.
    Setting {
        name: "dns_zone",
        default: "",
        shape: Shape::DnsName,
    },
    // The ACME directory the certificate is ordered at; the staging directory is a choice for a
    // first try, because the production CA rate-limits failures.
    Setting {
        name: "acme_directory",
        default: "https://acme-v02.api.letsencrypt.org/directory",
        shape: Shape::HttpsUrl,
    },
    // The contact address of a new ACME account. Empty: none.
    Setting {
        name: "acme_contact",
        default: "",
        shape: Shape::Email,
    },
    // The time to live, in seconds, of the challenge record. The provider's shortest is not
    // measured; GoDaddy documents 600 for a TXT record.
    Setting {
        name: "dns_ttl_s",
        default: "600",
        shape: Shape::Whole {
            min: 60,
            max: 86_400,
        },
    },
    // The seconds between two looks at whether the provider shows the challenge record, and
    // between two looks at the CA's state of the order.
    Setting {
        name: "dns_poll_s",
        default: "15",
        shape: Shape::Whole { min: 1, max: 300 },
    },
    // How long, in seconds, the challenge record may take to show at the provider.
    Setting {
        name: "dns_timeout_s",
        default: "600",
        shape: Shape::Whole { min: 30, max: 3600 },
    },
    // The seconds the coordinator waits after the provider shows the record and before it asks the
    // CA to check it, for the zone's servers to answer with it. Not measured.
    Setting {
        name: "dns_settle_s",
        default: "60",
        shape: Shape::Whole { min: 0, max: 600 },
    },
    // How long, in seconds, the CA may take to check the challenge and to issue.
    Setting {
        name: "acme_timeout_s",
        default: "300",
        shape: Shape::Whole { min: 30, max: 3600 },
    },
    // The seconds between two looks at whether the certificate for the MDM name is due.
    Setting {
        name: "tls_check_s",
        default: "300",
        shape: Shape::Whole {
            min: 10,
            max: 86_400,
        },
    },
    // The seconds between two attempts to renew the certificate after one failed.
    Setting {
        name: "tls_retry_s",
        default: "3600",
        shape: Shape::Whole {
            min: 60,
            max: 604_800,
        },
    },
    // The package proxy's cache (docs/design/package-proxy.md, section 4).
    // How long a waiter for a file another request is fetching waits for it before it is passed
    // through to the origin on its own.
    Setting {
        name: "proxy_wait_s",
        default: "10",
        shape: Shape::Whole { min: 1, max: 3600 },
    },
    // How many redirects the proxy follows from an origin.
    Setting {
        name: "proxy_redirects",
        default: "3",
        shape: Shape::Whole { min: 0, max: 10 },
    },
    // How long a cached index is served without asking the origin.
    Setting {
        name: "proxy_index_ttl_s",
        default: "300",
        shape: Shape::Whole {
            min: 0,
            max: 86_400,
        },
    },
    // The most bytes of committed objects the cache keeps (20 GiB).
    Setting {
        name: "proxy_cache_max_bytes",
        default: "21474836480",
        shape: Shape::Whole {
            min: 1,
            max: 1 << 50,
        },
    },
    // How many origin fetches run at once.
    Setting {
        name: "proxy_fetches",
        default: "8",
        shape: Shape::Whole { min: 1, max: 1024 },
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
                let whole = parse_whole(value)?;
                (min..=max).contains(&whole).then(|| whole.to_string())
            }
            Shape::OneOf(values) => values.contains(&value).then(|| value.to_owned()),
            Shape::DnsName => Some(String::new())
                .filter(|_| value.is_empty())
                .or_else(|| dns_name(value)),
            Shape::HttpsUrl => https_url(value).then(|| value.to_owned()),
            Shape::Email => (value.is_empty() || email(value)).then(|| value.to_owned()),
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

/// `text` as a DNS name in lower case: two or more labels of letters, digits and `-` that neither
/// start nor end with `-`, each 1 to 63 long, at most 253 in all.
fn dns_name(text: &str) -> Option<String> {
    let name = text.to_ascii_lowercase();
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let labels: Vec<&str> = name.split('.').collect();
    (name.len() <= 253 && labels.len() >= 2 && labels.iter().all(|label| label_ok(label)))
        .then_some(name)
}

fn https_url(text: &str) -> bool {
    let plain = text.bytes().all(|b| b.is_ascii_graphic());
    let host = text
        .strip_prefix("https://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or("");
    plain && text.len() <= 512 && !host.is_empty() && !host.contains('@')
}

fn email(text: &str) -> bool {
    let plain = text.bytes().all(|b| b.is_ascii_graphic());
    let mut parts = text.split('@');
    let (local, domain, rest) = (parts.next(), parts.next(), parts.next());
    plain
        && text.len() <= 254
        && rest.is_none()
        && local.is_some_and(|part| !part.is_empty())
        && domain.is_some_and(|part| {
            part.contains('.') && !part.starts_with('.') && !part.ends_with('.')
        })
}

/// The text value of the setting `name` among the `set` ones: what was set, or its default; empty
/// for a name this release does not know.
#[must_use]
pub fn text_of(set: &BTreeMap<String, String>, name: &str) -> String {
    find(name).map_or_else(String::new, |setting| value_of(set, setting).to_owned())
}

/// A whole number of days written with digits only.
fn parse_days(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A whole number written with digits only.
fn parse_whole(text: &str) -> Option<u64> {
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

/// The whole number of the setting `name` among the `set` ones: `fallback` for a name this release
/// does not know or a value that is not one (a store edited by hand).
#[must_use]
pub fn whole_of(set: &BTreeMap<String, String>, name: &str, fallback: u64) -> u64 {
    find(name)
        .and_then(|setting| parse_whole(value_of(set, setting)))
        .unwrap_or(fallback)
}

#[cfg(test)]
#[path = "mdm_settings_tests.rs"]
mod tests;
