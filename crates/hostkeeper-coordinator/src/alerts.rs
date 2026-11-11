//! Alerts: the conditions that need a person, derived from what the coordinator holds
//! (`Alerts.List`, design section 6.7).
//!
//! The coordinator has no pager. Every condition that needs a person is derived here, one
//! derivation per kind ([`Kind`]), by [`derive`]: a pure function of the fleet, the ops state, the
//! services, what the enrollment store says ([`EnrollmentFacts`]) and the time ([`At`]). The same
//! derivation answers `Alerts.List` and writes the error log lines with an `alert=` field (each
//! kind's [`Kind::log_token`]), so the two cannot disagree: an alert is logged once when an
//! observation first finds it ([`Book`]), at the next observation after its condition appears.
//! The coordinator observes every [`OBSERVE_EVERY`] ([`observe_every`]) and at every
//! `Alerts.List`.
//!
//! A condition that ends is forgotten: if it comes back it is logged again and first observed
//! anew. Nothing of the book is stored, so a restarted coordinator observes and logs every
//! standing condition anew.
//!
//! What each kind reads:
//! - `halt`: a halt of a service the rollout file defines (the ops state); once the coordinator
//!   holds a host group, a halt of a package in a group that lists it, the groups that share a
//!   halt named in one alert ([`crate::group_alerts`]);
//! - `waiting_for_operator`: a pending enrollment request that is no duplicate claim;
//! - `duplicate`: a host heard since the start whose identity two live daemons report (the
//!   enrollment store's mark, the same the rollout gate reads);
//! - `duplicate_claim_pending`: a pending request that is a duplicate claim, with the enrolled
//!   host it collides with when one is known (its renewal's host, or a recorded approval with its
//!   machine id or one of its universally administered MACs);
//! - `failed_canary`: a canary's failure noted by the reconcile ([`Fleet::failed_canaries`]),
//!   while its version stays marked bad; kept in memory, like the fleet's orders;
//! - `held_on_bad_version`: a held host running a version marked bad ([`crate::holds`]);
//! - `abandoned_while_updating`: an `exclude_abandon` among the changes the ops state keeps (its
//!   last 64), while the host stays excluded from the service;
//! - `silent_host`: a host heard since the start whose last heartbeat is at least
//!   `--alert-silent-host-secs` old;
//! - `cert_near_expiry`: the certificate the coordinator serves its hosts (with an enrollment
//!   store), when it expires within `--alert-cert-window-secs`, or has expired;
//! - `push_cert_near_expiry`: the MDM push certificate (the setup's `push_cert`, in use) at the
//!   smallest of the `push_alert_days` thresholds its expiry has come within (60, then 14 by
//!   default), or expired; the alert's version is the threshold in days, or `expired`, so that
//!   crossing the next threshold is a new alert, logged again;
//! - `mdm_identity_near_expiry`: an issued identity of a live Mac enrollment, or the CA that issues
//!   them, expires within the setting `renew_alert_days`, or has; the message lists the serials;
//! - `store_unreachable`: the store's link to its database is lost (`Fleet::store_lost`); it
//!   stands until the link is back. While it stands, an enrollment store that cannot be read
//!   gives no facts rather than refusing the list, so the alert is seen;
//! - `trusted_root`: a host heard since the start whose last heartbeat said its Sigstore trusted
//!   root is stale, missing or unparseable ([`crate::trusted_root`]), one alert per host; a host
//!   whose last heartbeat said nothing of the root (unknown) raises none.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_enrollment::enroll::{RecordedApproval, RequestState, RequestView, StoreError};
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::holds::bad_version;
use crate::host_groups::HostGroups;
use crate::mdm_secrets::SecretKind;
use crate::mdm_settings;
use crate::operator::Caller;
use crate::ops::{AuditEntry, Halt, HaltOrigin, OpsState};
use crate::service::Coordinator;
use crate::services::Services;

/// How often the running coordinator observes the alerts: one heartbeat interval.
pub const OBSERVE_EVERY: Duration = Duration::from_secs(15);

/// The default of `--alert-silent-host-secs`: twenty heartbeat intervals.
pub const DEFAULT_SILENT_HOST_SECS: u64 = 300;

/// The default of `--alert-cert-window-secs`: 14 days.
pub const DEFAULT_CERT_WINDOW_SECS: u64 = 14 * 24 * 3600;

/// The thresholds of the alerts that are a matter of time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    /// A host whose last heartbeat is this old, or older, is silent.
    pub silent_after: Duration,
    /// A certificate that expires within this many seconds is near its expiry.
    pub cert_window_secs: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            silent_after: Duration::from_secs(DEFAULT_SILENT_HOST_SECS),
            cert_window_secs: DEFAULT_CERT_WINDOW_SECS,
        }
    }
}

/// What an alert is about, in the order `Alerts.List` answers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A service is halted.
    Halt,
    /// A host waits for an operator to approve its enrollment.
    WaitingForOperator,
    /// Two live daemons report one host's identity.
    Duplicate,
    /// A pending request claims an identity another host or request has.
    DuplicateClaimPending,
    /// A canary failed a release.
    FailedCanary,
    /// A held host runs a version marked bad.
    HeldOnBadVersion,
    /// An operator abandoned a host that may be mid-update.
    AbandonedWhileUpdating,
    /// A host has not heartbeated for the silent-host threshold.
    SilentHost,
    /// The coordinator's certificate for its hosts expires soon.
    CertNearExpiry,
    /// The store's link to its database is lost.
    StoreUnreachable,
    /// A host's trusted root is stale, missing or unparseable.
    TrustedRoot,
    /// The MDM push certificate expires within a threshold of `push_alert_days`, or has expired.
    PushCertNearExpiry,
    /// A Mac's identity certificate or the CA that issues them expires within `renew_alert_days`.
    MdmIdentityNearExpiry,
}

impl Kind {
    /// The `alert=` field of its log line. The four kinds the coordinator logged before
    /// `Alerts.List` keep the tokens they had (`canary_failed`, `exclude_abandon`,
    /// `duplicate_claim`, `halt`), so a log search keeps finding them.
    #[must_use]
    pub const fn log_token(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::WaitingForOperator => "waiting_for_operator",
            Self::Duplicate => "duplicate",
            Self::DuplicateClaimPending => "duplicate_claim",
            Self::FailedCanary => "canary_failed",
            Self::HeldOnBadVersion => "held_on_bad_version",
            Self::AbandonedWhileUpdating => "exclude_abandon",
            Self::SilentHost => "silent_host",
            Self::CertNearExpiry => "cert_near_expiry",
            Self::StoreUnreachable => "store_unreachable",
            Self::TrustedRoot => "trusted_root",
            Self::PushCertNearExpiry => "push_cert_near_expiry",
            Self::MdmIdentityNearExpiry => "mdm_identity_near_expiry",
        }
    }

    /// How soon it needs a person: a stop, or a risk now, is critical.
    #[must_use]
    pub const fn severity(self) -> proto::AlertSeverity {
        match self {
            Self::Halt | Self::Duplicate | Self::FailedCanary | Self::StoreUnreachable => {
                proto::AlertSeverity::Critical
            }
            Self::WaitingForOperator
            | Self::DuplicateClaimPending
            | Self::HeldOnBadVersion
            | Self::AbandonedWhileUpdating
            | Self::SilentHost
            | Self::CertNearExpiry
            | Self::TrustedRoot
            | Self::PushCertNearExpiry
            | Self::MdmIdentityNearExpiry => proto::AlertSeverity::Warning,
        }
    }

    /// Its value in `hostkeeper.operator.v1`.
    #[must_use]
    pub const fn proto(self) -> proto::AlertKind {
        match self {
            Self::Halt => proto::AlertKind::Halt,
            Self::WaitingForOperator => proto::AlertKind::WaitingForOperator,
            Self::Duplicate => proto::AlertKind::Duplicate,
            Self::DuplicateClaimPending => proto::AlertKind::DuplicateClaimPending,
            Self::FailedCanary => proto::AlertKind::FailedCanary,
            Self::HeldOnBadVersion => proto::AlertKind::HeldOnBadVersion,
            Self::AbandonedWhileUpdating => proto::AlertKind::AbandonedWhileUpdating,
            Self::SilentHost => proto::AlertKind::SilentHost,
            Self::CertNearExpiry => proto::AlertKind::CertNearExpiry,
            Self::StoreUnreachable => proto::AlertKind::StoreUnreachable,
            Self::TrustedRoot => proto::AlertKind::TrustedRoot,
            Self::PushCertNearExpiry => proto::AlertKind::PushCertNearExpiry,
            Self::MdmIdentityNearExpiry => proto::AlertKind::MdmIdentityNearExpiry,
        }
    }
}

/// What makes an alert the same alert from one observation to the next: its kind and what it
/// concerns, each empty when it concerns none. The order of the answer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Subject {
    /// Its kind.
    pub kind: Kind,
    /// The service.
    pub service: String,
    /// The host, by node id.
    pub host: String,
    /// The version.
    pub version: String,
    /// The pending enrollment request, by its id.
    pub request: String,
    /// The enrolled host a duplicate claim collides with.
    pub other_host: String,
}

impl Subject {
    fn of(kind: Kind) -> Self {
        Self {
            kind,
            service: String::new(),
            host: String::new(),
            version: String::new(),
            request: String::new(),
            other_host: String::new(),
        }
    }
}

/// A condition that needs a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    /// What it is about.
    pub subject: Subject,
    /// What happened and what a person does about it.
    pub message: String,
}

/// A pending enrollment request, as the alerts read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    /// Its id, in hex.
    pub request_id: String,
    /// When the coordinator received it, unix seconds.
    pub posted_unix: u64,
    /// The host it renews, if it is a renewal.
    pub renewal_of: Option<String>,
    /// Whether it is a duplicate claim.
    pub duplicate_claim: bool,
    /// For a duplicate claim, the enrolled host it collides with, when one is known.
    pub other_host: Option<String>,
}

/// What the enrollment store holds that the alerts read, read once per observation (an empty
/// one for a coordinator without `--enrollment-dir`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnrollmentFacts {
    /// The pending requests, duplicate claims included.
    pub waiting: Vec<Waiting>,
    /// The hosts heard since the start that the store marks duplicate.
    pub duplicates: BTreeSet<HostId>,
    /// When the certificate the coordinator serves its hosts expires, unix seconds.
    pub server_not_after: Option<u64>,
    /// When the MDM push certificate in use expires, unix seconds; none when no push certificate
    /// is held.
    pub push_cert_not_after: Option<u64>,
    /// The days before that expiry at which the alert is raised, falling (the setting
    /// `push_alert_days`).
    pub push_alert_days: Vec<u32>,
    /// The serials of the live Mac enrollments whose identity certificate expires within the
    /// window, with the unix second it does, soonest first.
    pub mdm_identities: Vec<(String, u64)>,
    /// When the CA of the identities expires, unix seconds, when one is held.
    pub mdm_ca_not_after: Option<u64>,
    /// The window in days (the setting `renew_alert_days`); none when the setup was not read.
    pub mdm_renew_alert_days: Option<u32>,
}

/// When an observation is made: by the coordinator's clock (since its start, the clock the
/// fleet keeps its heartbeats by) and in unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    /// The coordinator's clock.
    pub now: Duration,
    /// Unix seconds.
    pub unix: u64,
}

/// Every alert of `fleet`, `ops`, `services` and `enrollment` at `at`, by [`Subject`]: one
/// derivation per kind. Pure: it reads nothing else, and nothing changes.
#[must_use]
pub fn derive(
    fleet: &Fleet,
    ops: &OpsState,
    services: &Services,
    enrollment: &EnrollmentFacts,
    at: At,
    thresholds: &Thresholds,
) -> Vec<Alert> {
    derive_in(
        fleet,
        ops,
        services,
        &HostGroups::default(),
        enrollment,
        at,
        thresholds,
    )
}

/// [`derive`] for a coordinator that may hold the host groups `groups`: with none it is
/// [`derive`]; with some, the halts are the groups' ([`crate::group_alerts`]) and a package a group
/// lists is a service for the alerts that read a service's marks and hosts.
pub(crate) fn derive_in(
    fleet: &Fleet,
    ops: &OpsState,
    services: &Services,
    groups: &HostGroups,
    enrollment: &EnrollmentFacts,
    at: At,
    thresholds: &Thresholds,
) -> Vec<Alert> {
    let packages = managed(services, groups);
    let mut alerts = if groups.is_empty() {
        halts(ops, services)
    } else {
        group_alerts::halts_in(ops, groups)
    };
    alerts.extend(waiting_for_operator(enrollment));
    alerts.extend(duplicates(enrollment));
    alerts.extend(duplicate_claims(enrollment));
    alerts.extend(failed_canaries(fleet, ops, &packages));
    alerts.extend(held_on_bad_versions(fleet, ops, &packages));
    alerts.extend(abandoned(ops, &packages));
    alerts.extend(silent_hosts(fleet, at.now, thresholds.silent_after));
    alerts.extend(cert_near_expiry(
        enrollment,
        at.unix,
        thresholds.cert_window_secs,
    ));
    alerts.extend(push_cert_near_expiry(enrollment, at.unix));
    alerts.extend(mdm_identity_near_expiry(enrollment, at.unix));
    alerts.extend(store_unreachable(fleet));
    alerts.extend(trusted_roots(fleet));
    alerts.sort_by(|a, b| a.subject.cmp(&b.subject));
    alerts.dedup_by(|a, b| a.subject == b.subject);
    alerts
}

/// Who raised `halt`: an operator, or the host and the check that reported it.
fn halted_by(halt: &Halt) -> String {
    if halt.origin == HaltOrigin::Operator {
        "an operator".to_owned()
    } else {
        format!("{}, check {}", halt.node_id, halt.check)
    }
}

/// The services the alerts read the marks and hosts of: those the rollout file defines and those a
/// host group lists.
fn managed(services: &Services, groups: &HostGroups) -> BTreeSet<String> {
    services
        .names()
        .map(str::to_owned)
        .chain(
            groups
                .0
                .values()
                .flat_map(|group| group.packages.iter().cloned()),
        )
        .collect()
}

/// `halt`: each service the rollout file defines whose halt stands.
fn halts(ops: &OpsState, services: &Services) -> Vec<Alert> {
    services
        .defs()
        .filter_map(|def| {
            let halt = ops.services.get(&def.name)?.halt.as_ref()?;
            let by = halted_by(halt);
            Some(Alert {
                subject: Subject {
                    service: def.name.clone(),
                    host: halt.node_id.clone(),
                    version: halt.target.to_string(),
                    ..Subject::of(Kind::Halt)
                },
                message: format!(
                    "{} is halted at {} by {by}: nothing is ordered for it until an operator \
                     clears the halt",
                    def.name, halt.target
                ),
            })
        })
        .collect()
}

/// `waiting_for_operator`: each pending request that is no duplicate claim.
fn waiting_for_operator(enrollment: &EnrollmentFacts) -> Vec<Alert> {
    enrollment
        .waiting
        .iter()
        .filter(|request| !request.duplicate_claim)
        .map(|request| {
            let what = request.renewal_of.as_ref().map_or_else(
                || "a host's enrollment".to_owned(),
                |host| format!("the renewal of {host}"),
            );
            Alert {
                subject: Subject {
                    host: request.renewal_of.clone().unwrap_or_default(),
                    request: request.request_id.clone(),
                    ..Subject::of(Kind::WaitingForOperator)
                },
                message: format!(
                    "request {} ({what}, received at unix {}) waits for an operator's approval",
                    request.request_id, request.posted_unix
                ),
            }
        })
        .collect()
}

/// `duplicate`: each host the enrollment store marks duplicate.
fn duplicates(enrollment: &EnrollmentFacts) -> Vec<Alert> {
    enrollment
        .duplicates
        .iter()
        .map(|host| Alert {
            subject: Subject {
                host: host.to_string(),
                ..Subject::of(Kind::Duplicate)
            },
            message: format!(
                "two live daemons report the identity of {host} (a cloned disk): it is ordered \
                 nothing until a new key replaces it"
            ),
        })
        .collect()
}

/// `duplicate_claim_pending`: each pending request that is a duplicate claim.
fn duplicate_claims(enrollment: &EnrollmentFacts) -> Vec<Alert> {
    enrollment
        .waiting
        .iter()
        .filter(|request| request.duplicate_claim)
        .map(|request| {
            let whose = request
                .other_host
                .as_ref()
                .map_or_else(|| "another host or request".to_owned(), Clone::clone);
            Alert {
                subject: Subject {
                    host: request.renewal_of.clone().unwrap_or_default(),
                    request: request.request_id.clone(),
                    other_host: request.other_host.clone().unwrap_or_default(),
                    ..Subject::of(Kind::DuplicateClaimPending)
                },
                message: format!(
                    "request {} claims an identity {whose} has: approve it only with replace, \
                     or leave it",
                    request.request_id
                ),
            }
        })
        .collect()
}

/// `failed_canary`: each canary failure the fleet noted whose version is still marked bad.
fn failed_canaries(fleet: &Fleet, ops: &OpsState, packages: &BTreeSet<String>) -> Vec<Alert> {
    packages
        .iter()
        .flat_map(|name| {
            let bad = ops
                .services
                .get(name)
                .map(|marks| marks.bad.as_slice())
                .unwrap_or_default();
            fleet
                .failed_canaries(name)
                .filter(|(version, _)| bad.iter().any(|b| b.as_str() == *version))
                .map(|(version, node)| Alert {
                    subject: Subject {
                        service: name.clone(),
                        host: node.to_owned(),
                        version: version.to_owned(),
                        ..Subject::of(Kind::FailedCanary)
                    },
                    message: format!(
                        "{node}, a canary of {name}, failed {version}: it is marked bad and rolled \
                         back"
                    ),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `held_on_bad_version`: each held host of a service that runs a version of it marked bad.
fn held_on_bad_versions(fleet: &Fleet, ops: &OpsState, packages: &BTreeSet<String>) -> Vec<Alert> {
    packages
        .iter()
        .flat_map(|name| {
            fleet
                .hosts_of(name)
                .filter_map(|(id, _)| {
                    let version = bad_version(fleet, ops, name, id)?;
                    Some(Alert {
                        subject: Subject {
                            service: name.clone(),
                            host: id.to_string(),
                            version: version.to_string(),
                            ..Subject::of(Kind::HeldOnBadVersion)
                        },
                        message: format!(
                            "an operator holds {id} on {name} {version}, which is marked bad: it is \
                             not ordered off it until the hold is released"
                        ),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `abandoned_while_updating`: each pair (a service the rollout file defines and a host) whose
/// last exclusion change the ops state keeps (`exclude`, `exclude_abandon` or `include`) is an
/// `exclude_abandon`, while the host is still excluded from it.
fn abandoned(ops: &OpsState, packages: &BTreeSet<String>) -> Vec<Alert> {
    // The last exclusion change of each pair the ring keeps: an abandon that a later include
    // or exclude of the pair followed no longer says why the host is excluded.
    let mut last: BTreeMap<(&str, &str), &AuditEntry> = BTreeMap::new();
    for entry in &ops.audit {
        if let ("exclude" | "exclude_abandon" | "include", Some(service), Some(node)) = (
            entry.action.as_str(),
            entry.service.as_deref(),
            entry.node_id.as_deref(),
        ) {
            last.insert((service, node), entry);
        }
    }
    last.into_iter()
        .filter(|(_, entry)| entry.action == "exclude_abandon")
        .filter_map(|((service, node), entry)| {
            packages.contains(service).then_some(())?;
            ops.excluded
                .iter()
                .any(|e| e.service == service && e.node_id == node)
                .then(|| Alert {
                    subject: Subject {
                        service: service.to_owned(),
                        host: node.to_owned(),
                        ..Subject::of(Kind::AbandonedWhileUpdating)
                    },
                    message: format!(
                        "an operator abandoned {node} while it may have been updating {service} \
                         ({}): its update slot was freed, and it stays excluded until included",
                        entry.reason.as_deref().unwrap_or("no reason")
                    ),
                })
        })
        .collect()
}

/// `silent_host`: each host heard since the start whose last heartbeat is `silent_after` old
/// or older at `now`.
fn silent_hosts(fleet: &Fleet, now: Duration, silent_after: Duration) -> Vec<Alert> {
    fleet
        .nodes()
        .filter_map(|(id, node)| {
            let silent = now.saturating_sub(node.seen);
            (silent >= silent_after).then(|| Alert {
                subject: Subject {
                    host: id.to_string(),
                    ..Subject::of(Kind::SilentHost)
                },
                message: format!(
                    "{id} has not heartbeated for {} s (the threshold is {} s)",
                    silent.as_secs(),
                    silent_after.as_secs()
                ),
            })
        })
        .collect()
}

/// `store_unreachable`: the store's link to its database, while it is lost.
fn store_unreachable(fleet: &Fleet) -> Option<Alert> {
    fleet.store_lost.then(|| Alert {
        subject: Subject::of(Kind::StoreUnreachable),
        message: "the session with the database is lost: nothing is ordered, a change that lowers protection is refused, and one that raises it stands in memory until the session is back".to_owned(),
    })
}

/// `trusted_root`: each host heard since the start whose last heartbeat said its trusted root
/// needs attention, with the reason and the age it said.
fn trusted_roots(fleet: &Fleet) -> Vec<Alert> {
    fleet
        .nodes()
        .filter_map(|(id, node)| {
            let reason = node.trusted_root.attention()?;
            let age = node
                .trusted_root
                .age_secs
                .map_or_else(|| "no age".to_owned(), |age| format!("{age} s old"));
            let then = if reason == "stale" {
                "its package checks go on, and refuse every package signed after a Sigstore key \
                 rotation the root does not hold"
            } else {
                "its package checks fail"
            };
            Some(Alert {
                subject: Subject {
                    host: id.to_string(),
                    ..Subject::of(Kind::TrustedRoot)
                },
                message: format!(
                    "{id} reports its Sigstore trusted root {reason} ({age}): {then}; update \
                     hostkeeper on it, or copy a current root to it"
                ),
            })
        })
        .collect()
}

/// `cert_near_expiry`: the certificate the coordinator serves its hosts, when it expires
/// before `unix` plus `window` seconds.
fn cert_near_expiry(enrollment: &EnrollmentFacts, unix: u64, window: u64) -> Vec<Alert> {
    enrollment
        .server_not_after
        .filter(|not_after| *not_after < unix.saturating_add(window))
        .map(|not_after| {
            let when = if not_after <= unix {
                format!("expired at unix {not_after}")
            } else {
                format!("expires at unix {not_after}, in {} s", not_after - unix)
            };
            Alert {
                subject: Subject::of(Kind::CertNearExpiry),
                message: format!(
                    "the certificate the coordinator serves its hosts {when}: its renewal is \
                     failing (see the coordinator's log)"
                ),
            }
        })
        .into_iter()
        .collect()
}

/// `push_cert_near_expiry`: the push certificate in use, when it expires before `unix` plus the
/// days of a threshold: the smallest threshold reached is the alert's version, `expired` once it
/// has expired.
fn push_cert_near_expiry(enrollment: &EnrollmentFacts, unix: u64) -> Vec<Alert> {
    let Some(not_after) = enrollment.push_cert_not_after else {
        return Vec::new();
    };
    let reached = enrollment
        .push_alert_days
        .iter()
        .copied()
        .filter(|days| not_after < unix.saturating_add(u64::from(*days) * 86_400))
        .min();
    let (version, when) = if not_after <= unix {
        ("expired".to_owned(), format!("expired at unix {not_after}"))
    } else if let Some(days) = reached {
        (
            days.to_string(),
            format!("expires at unix {not_after}, in {} s", not_after - unix),
        )
    } else {
        return Vec::new();
    };
    let mut subject = Subject::of(Kind::PushCertNearExpiry);
    subject.version = version;
    vec![Alert {
        subject,
        message: format!(
            "the MDM push certificate {when}: renew it from the MDM setup (a push certificate \
             that expires stops every push to the Macs)"
        ),
    }]
}

/// `mdm_identity_near_expiry`: the identities that expire within the window and the CA, when it
/// does. Its version is the window in days, or `expired` once any of them has.
fn mdm_identity_near_expiry(enrollment: &EnrollmentFacts, unix: u64) -> Vec<Alert> {
    let Some(days) = enrollment.mdm_renew_alert_days else {
        return Vec::new();
    };
    let window = unix.saturating_add(u64::from(days) * 86_400);
    let ca = enrollment
        .mdm_ca_not_after
        .filter(|not_after| *not_after < window);
    if enrollment.mdm_identities.is_empty() && ca.is_none() {
        return Vec::new();
    }
    let expired = ca.is_some_and(|at| at <= unix)
        || enrollment.mdm_identities.iter().any(|(_, at)| *at <= unix);
    let mut subject = Subject::of(Kind::MdmIdentityNearExpiry);
    subject.version = if expired {
        "expired".to_owned()
    } else {
        days.to_string()
    };
    let mut parts = Vec::new();
    if !enrollment.mdm_identities.is_empty() {
        let serials: Vec<&str> = enrollment
            .mdm_identities
            .iter()
            .map(|(serial, _)| serial.as_str())
            .collect();
        parts.push(format!(
            "the identity certificates of {} expire within {days} days (or have): enroll each Mac again before they do",
            serials.join(", ")
        ));
    }
    if let Some(at) = ca {
        parts.push(format!(
            "the CA that issues the Macs' identities expires at unix {at}: no identity issued after that is valid"
        ));
    }
    vec![Alert {
        subject,
        message: parts.join("; "),
    }]
}

/// For a duplicate claim `view`, the enrolled host it collides with: the host it renews, else a
/// host whose recorded approval has its machine id or one of its universally administered MACs.
fn collides_with(view: &RequestView, approvals: &[RecordedApproval]) -> Option<String> {
    if let Some(host) = &view.renewal_of {
        return Some(host.to_string());
    }
    let macs: Vec<MacAddress> = view
        .claims
        .all_macs()
        .into_iter()
        .filter(MacAddress::is_universal_unicast)
        .collect();
    let machine = view.claims.machine_id.as_str();
    approvals
        .iter()
        .filter(|approval| approval.request_id != view.request_id)
        .find(|approval| {
            (!machine.is_empty() && approval.claims.machine_id == machine)
                || approval
                    .claims
                    .all_macs()
                    .iter()
                    .any(|mac| macs.contains(mac))
        })
        .map(|approval| approval.host_id.clone())
}

/// An alert as an observation found it: with when it was first observed, unix seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// The alert.
    pub alert: Alert,
    /// When it was first observed.
    pub first_unix: u64,
}

/// The alerts the last observation found, each with when it was first observed: in memory only.
#[derive(Debug, Default)]
pub struct Book {
    thresholds: Thresholds,
    seen: Mutex<BTreeMap<Subject, u64>>,
}

impl Book {
    /// A book that derives with `thresholds`.
    #[must_use]
    pub fn new(thresholds: Thresholds) -> Self {
        Self {
            thresholds,
            seen: Mutex::new(BTreeMap::new()),
        }
    }

    /// Notes the alerts `find` finds, with the unix time it found them at, under the book's lock,
    /// so that two observations (the tick and an `Alerts.List`) are made one after the other and
    /// an older finding never follows a newer one: an alert the last observation did not find is
    /// first observed now and logged; one it found keeps its time; one it found and this did not
    /// is forgotten. When `find` fails nothing is noted.
    fn observe_with<E>(
        &self,
        find: impl FnOnce() -> Result<(Vec<Alert>, u64), E>,
    ) -> Result<Vec<Observed>, E> {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let (alerts, unix) = find()?;
        let observed: Vec<Observed> = alerts
            .into_iter()
            .map(|alert| {
                let first_unix = seen.get(&alert.subject).copied().unwrap_or_else(|| {
                    log(&alert);
                    unix
                });
                Observed { alert, first_unix }
            })
            .collect();
        *seen = observed
            .iter()
            .map(|one| (one.alert.subject.clone(), one.first_unix))
            .collect();
        Ok(observed)
    }
}

/// The error log line of `alert`, with its `alert=` field.
fn log(alert: &Alert) {
    let subject = &alert.subject;
    tracing::error!(
        alert = subject.kind.log_token(),
        service = %subject.service,
        node_id = %subject.host,
        version = %subject.version,
        request_id = %subject.request,
        other_host_id = %subject.other_host,
        "{}",
        alert.message
    );
}

/// `observed` as `Alerts.List` answers it.
fn alert_proto(observed: Observed) -> proto::Alert {
    let Observed { alert, first_unix } = observed;
    let subject = alert.subject;
    proto::Alert {
        kind: subject.kind.proto().into(),
        severity: subject.kind.severity().into(),
        host_id: subject.host,
        service: subject.service,
        version: subject.version,
        message: alert.message,
        first_observed_unix: first_unix,
        request_id: subject.request,
        other_host_id: subject.other_host,
    }
}

impl Coordinator {
    /// The same coordinator deriving its alerts with `thresholds` (`--alert-silent-host-secs`,
    /// `--alert-cert-window-secs`) instead of the defaults.
    #[must_use]
    pub fn with_alert_thresholds(mut self, thresholds: Thresholds) -> Self {
        self.alerts = Book::new(thresholds);
        self
    }

    /// Observes the alerts now: reads the enrollment store, derives every alert ([`derive`])
    /// and logs each the last observation did not find ([`Book`]).
    ///
    /// # Errors
    /// [`StoreError`]: the enrollment store could not be read; nothing is noted or logged.
    pub fn observe_alerts(&self) -> Result<Vec<Observed>, StoreError> {
        self.alerts.observe_with(|| {
            let heard: Vec<HostId> = self
                .lock_fleet()
                .nodes()
                .map(|(id, _)| id.clone())
                .collect();
            let mut enrollment = match self.enrollment_facts(&heard) {
                Ok(facts) => facts,
                // The alert that says the database is lost is not hidden by it.
                Err(why) if self.lock_fleet().store_lost => {
                    tracing::warn!(%why, "the enrollment store is not read while the database is lost");
                    EnrollmentFacts::default()
                }
                Err(why) => return Err(why),
            };
            self.note_push_certificate(&mut enrollment);
            self.note_identities(&mut enrollment);
            let at = At {
                now: (self.clock)(),
                unix: self.ops.unix_now(),
            };
            let ops = self.ops.state();
            let alerts = derive_in(
                &self.lock_fleet(),
                &ops,
                &self.services,
                &self.desired.host_groups(),
                &enrollment,
                at,
                &self.alerts.thresholds,
            );
            Ok((alerts, at.unix))
        })
    }

    /// Notes in `facts` when the push certificate in use expires and the days of the alert, from
    /// the MDM setup; a setup that cannot be read is logged and raises no alert of its own.
    fn note_push_certificate(&self, facts: &mut EnrollmentFacts) {
        let setup = &self.mdm_setup;
        match (setup.list_secrets(), setup.settings()) {
            (Ok(secrets), Ok(settings)) => {
                facts.push_cert_not_after = secrets
                    .iter()
                    .filter(|info| {
                        info.kind == SecretKind::PushCert && info.superseded_unix.is_none()
                    })
                    .find_map(|info| info.not_after_unix)
                    .and_then(|at| u64::try_from(at).ok());
                facts.push_alert_days = mdm_settings::list_of(&settings, "push_alert_days");
            }
            (Err(why), _) | (_, Err(why)) => {
                tracing::warn!(%why, "the push certificate's expiry is not read: the MDM setup could not be read");
            }
        }
    }

    /// Notes in `facts` the identities of the Macs and the CA that expire within the window of
    /// `renew_alert_days`; a store or a setup that cannot be read is logged and raises no alert of
    /// its own.
    fn note_identities(&self, facts: &mut EnrollmentFacts) {
        let Some(server) = self.mdm_server() else {
            return;
        };
        let setup = &self.mdm_setup;
        let (Ok(secrets), Ok(settings)) = (setup.list_secrets(), setup.settings()) else {
            tracing::warn!(
                "the MDM identities' expiry is not read: the MDM setup could not be read"
            );
            return;
        };
        let days = mdm_settings::number_of(&settings, "renew_alert_days", 90);
        facts.mdm_renew_alert_days = Some(days);
        facts.mdm_ca_not_after = secrets
            .iter()
            .filter(|info| info.kind == SecretKind::MdmCaKey && info.superseded_unix.is_none())
            .find_map(|info| info.not_after_unix)
            .and_then(|at| u64::try_from(at).ok());
        let window = i64::try_from(self.ops.unix_now())
            .unwrap_or(i64::MAX)
            .saturating_add(i64::from(days) * 86_400);
        match server.store.identities_expiring(window) {
            Ok(rows) => {
                facts.mdm_identities = rows
                    .into_iter()
                    .filter_map(|(serial, at)| Some((serial, u64::try_from(at).ok()?)))
                    .collect();
            }
            Err(why) => tracing::warn!(%why, "the MDM identities' expiry is not read"),
        }
    }

    /// `Alerts.List`: every alert now, as an observation finds it ([`Self::observe_alerts`]).
    /// Changes nothing but the book of alerts.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `store_failed` (the enrollment store could not be read).
    pub fn list_alerts(
        &self,
        caller: &Caller,
        _request: proto::ListAlertsRequest,
    ) -> Result<proto::ListAlertsResponse, proto::AlertsRefusal> {
        caller.operator()?;
        let observed = self.observe_alerts().map_err(|why| {
            tracing::error!(%why, "the alerts could not be listed: the enrollment store could not be read");
            proto::AlertsRefusal::StoreFailed
        })?;
        Ok(proto::ListAlertsResponse {
            alerts: observed.into_iter().map(alert_proto).collect(),
        })
    }

    /// What the enrollment store says that the alerts read, of the hosts `heard`: nothing
    /// without one.
    fn enrollment_facts(&self, heard: &[HostId]) -> Result<EnrollmentFacts, StoreError> {
        let Some(enrollment) = self.enrollment() else {
            return Ok(EnrollmentFacts::default());
        };
        let store = enrollment.store();
        let pending = store.pending()?;
        let approvals = if pending
            .iter()
            .any(|view| view.state == RequestState::DuplicateClaim)
        {
            store.recorded_approvals()?
        } else {
            Vec::new()
        };
        let waiting = pending
            .iter()
            .map(|view| {
                let duplicate_claim = view.state == RequestState::DuplicateClaim;
                Waiting {
                    request_id: view.request_id.to_hex(),
                    posted_unix: view.posted_unix,
                    renewal_of: view.renewal_of.as_ref().map(ToString::to_string),
                    duplicate_claim,
                    other_host: duplicate_claim
                        .then(|| collides_with(view, &approvals))
                        .flatten(),
                }
            })
            .collect();
        Ok(EnrollmentFacts {
            waiting,
            duplicates: heard
                .iter()
                .filter(|host| enrollment.is_duplicate(host))
                .cloned()
                .collect(),
            server_not_after: Some(enrollment.server_certificate().not_after),
            ..EnrollmentFacts::default()
        })
    }
}

/// Observes the alerts of `coordinator` every `interval`, for as long as the future is polled:
/// the first look one `interval` after the start. An observation that cannot read the
/// enrollment store is logged at error. It never returns.
pub async fn observe_every(coordinator: Arc<Coordinator>, interval: Duration) -> Infallible {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticks.tick().await;
    loop {
        ticks.tick().await;
        // The enrollment store's request files are read here, on the runtime, as `Alerts.List`
        // reads them.
        if let Err(why) = coordinator.observe_alerts() {
            tracing::error!(%why, "the alerts could not be observed: the enrollment store could not be read");
        }
    }
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "alerts_coordinator_tests.rs"]
mod coordinator_tests;

#[path = "group_alerts.rs"]
mod group_alerts;
