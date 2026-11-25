//! The alerts of one host that cannot reach a service's desired version: its update or install of
//! the version failed (`failed_update`), it holds a halt of the service of its own (`host_halt`),
//! or it declined the version several times in a row for one reason (`repeated_deferral`). Each is
//! one alert per host and service, with the reason and what the host said ([`Said`]), and each
//! clears by itself: a failed update when the host reports another outcome (it reached the
//! version, or a new try an operator asked for is under way; a failure the coordinator retries by
//! itself stays through its tries) or the desired version changes, a host's halt when the host no
//! longer reports it, a repeated deferral when the host updates, gives another reason, or the
//! desired version changes.

use std::collections::BTreeMap;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Outcome};

use super::{Alert, At, Kind, Said, Subject};
use crate::defer::REPEATED_DEFERRALS;
use crate::failed_hosts::clears_by_itself;
use crate::fleet::{Fleet, Host};
use crate::host_groups::{GroupName, HostGroups};
use crate::ops::OpsState;
use crate::services::Services;

/// A service the alerts read the hosts of, in the group it is wanted in (none for a service of
/// the rollout file), with its desired version when it has one.
struct Wanted<'a> {
    group: Option<&'a GroupName>,
    service: &'a str,
    version: Option<&'a Version>,
}

/// Every package of every group of `groups`, with the group's desired version of it; with no group,
/// every service of `services`, at its desired version in `desired`.
fn wanted<'a>(
    services: &'a Services,
    groups: &'a HostGroups,
    desired: &'a BTreeMap<String, Version>,
) -> Vec<Wanted<'a>> {
    if groups.is_empty() {
        return services
            .defs()
            .map(|def| Wanted {
                group: None,
                service: def.name.as_str(),
                version: desired.get(&def.name),
            })
            .collect();
    }
    groups
        .0
        .iter()
        .flat_map(|(name, group)| {
            group.packages.iter().map(move |package| Wanted {
                group: Some(name),
                service: package.as_str(),
                version: group.desired.get(package).map(|want| &want.version),
            })
        })
        .collect()
}

/// The hosts of `want`'s service the fleet has a word of from their last heartbeat, in its group
/// alone, excluded hosts left out.
fn hosts<'a>(
    fleet: &'a Fleet,
    want: &'a Wanted<'_>,
) -> impl Iterator<Item = (&'a HostId, &'a Host)> {
    fleet.occupants_of(want.service).filter(move |(id, host)| {
        want.group
            .is_none_or(|group| host.role_name() == group.as_str())
            && crate::decide::heard(fleet, want.service, id).is_some()
            && !fleet.is_excluded(want.service, id)
    })
}

/// " in host group g", or nothing for a service of the rollout file.
fn in_group(group: Option<&GroupName>) -> String {
    group.map_or_else(String::new, |group| format!(" in host group {group}"))
}

/// The alerts of the hosts of every service of `services`, or package of every group of `groups`:
/// [`failed_updates`], [`host_halts`] and [`repeated_deferrals`], `desired` the coordinator's
/// desired versions of the rollout file's services.
pub(super) fn of_hosts(
    fleet: &Fleet,
    ops: &OpsState,
    services: &Services,
    groups: &HostGroups,
    desired: &BTreeMap<String, Version>,
    at: At,
) -> Vec<Alert> {
    let mut alerts = failed_updates(fleet, services, groups, desired);
    alerts.extend(host_halts(fleet, ops, services, groups, at));
    alerts.extend(repeated_deferrals(fleet, services, groups, desired));
    alerts
}

/// `failed_update`: each host whose last report of a service is a failure of the version its
/// group wants of it, or, for a failure the coordinator retries by itself (a package of a group),
/// whose last such failure is set aside while the host has reported nothing else
/// ([`Fleet::failing`]). A `halted` refusal is the host's own halt, which `host_halt` says.
fn failed_updates(
    fleet: &Fleet,
    services: &Services,
    groups: &HostGroups,
    desired: &BTreeMap<String, Version>,
) -> Vec<Alert> {
    let mut alerts = Vec::new();
    for want in wanted(services, groups, desired) {
        let Some(version) = want.version else {
            continue;
        };
        for (id, host) in hosts(fleet, &want) {
            let standing = host.beat.last_update.as_ref().filter(|report| {
                report.outcome == Outcome::Failed
                    && report.target == *version
                    && !report.is_halted_refusal()
            });
            let retried = || {
                fleet
                    .failing(want.service, id)
                    .map(|run| run.report)
                    .filter(|report| report.target == *version)
            };
            let Some(report) = standing.cloned().or_else(retried) else {
                continue;
            };
            let reason = report.reason.as_ref().map_or_else(
                || "unspecified".to_owned(),
                |token| token.as_str().to_owned(),
            );
            let detail = report.detail.clone().unwrap_or_default();
            let said = if detail.is_empty() {
                String::new()
            } else {
                format!(" ({detail:?})")
            };
            let next = if want.group.is_some() && clears_by_itself(&report) {
                let failures = fleet.failures_in_a_row(want.service, id).unwrap_or(1);
                format!(
                    "; the host can clear it by itself ({failures} in a row): it is ordered \
                     {version} again after the group's defer_backoff, longer after each failure"
                )
            } else {
                format!(
                    "; it is not ordered {version} again until an operator retries it \
                     (Hosts.Retry), holds or excludes it, or another version is set"
                )
            };
            alerts.push(Alert {
                subject: Subject {
                    service: want.service.to_owned(),
                    host: id.to_string(),
                    version: version.as_str().to_owned(),
                    ..Subject::of(Kind::FailedUpdate)
                },
                message: format!(
                    "{id} failed {} {version}{}: {reason}{said}{next}",
                    want.service,
                    in_group(want.group),
                ),
                said: Said {
                    reason,
                    detail,
                    tries: report.order_try.unwrap_or_default(),
                    since_unix: 0,
                },
            });
        }
    }
    alerts
}

/// `repeated_deferral`: each host that declined the version its group wants of a service
/// [`REPEATED_DEFERRALS`] times or more in a row for the same reason ([`crate::defer`]), and does
/// not run it.
fn repeated_deferrals(
    fleet: &Fleet,
    services: &Services,
    groups: &HostGroups,
    desired: &BTreeMap<String, Version>,
) -> Vec<Alert> {
    let mut alerts = Vec::new();
    for want in wanted(services, groups, desired) {
        let Some(version) = want.version else {
            continue;
        };
        // A host that runs the version declines it no longer, though no beat said it updated.
        let runs =
            |host: &Host| host.beat.health != Health::Absent && host.beat.version == *version;
        for (id, _) in hosts(fleet, &want).filter(|(_, host)| !runs(host)) {
            let Some((reason, times)) = fleet
                .declined_in_a_row(want.service, id, version)
                .filter(|(_, times)| *times >= REPEATED_DEFERRALS)
            else {
                continue;
            };
            alerts.push(Alert {
                subject: Subject {
                    service: want.service.to_owned(),
                    host: id.to_string(),
                    version: version.as_str().to_owned(),
                    ..Subject::of(Kind::RepeatedDeferral)
                },
                message: format!(
                    "{id} declined {} {version}{} {times} times in a row: {}; it is offered \
                     the version again after each wait, longer each time, and holds no other \
                     host or service, but its reason may not clear by itself",
                    want.service,
                    in_group(want.group),
                    reason.as_str(),
                ),
                said: Said {
                    reason: reason.as_str().to_owned(),
                    detail: String::new(),
                    tries: u64::from(times),
                    since_unix: 0,
                },
            });
        }
    }
    alerts
}

/// `host_halt`: each host that holds a halt of a service of its own (its beat's `local_halt`),
/// stale or not: stale when the generation it holds is above any the coordinator issued for the
/// service in its group, so that no generation the coordinator sends lifts it but the one a
/// `Releases.ClearHalt` issues past it.
fn host_halts(
    fleet: &Fleet,
    ops: &OpsState,
    services: &Services,
    groups: &HostGroups,
    at: At,
) -> Vec<Alert> {
    let none = BTreeMap::new();
    let mut alerts = Vec::new();
    for want in wanted(services, groups, &none) {
        let issued = ops.halt_generation_in(want.group, want.service);
        for (id, host) in hosts(fleet, &want) {
            let Some(halt) = host.beat.local_halt.as_ref() else {
                continue;
            };
            let reason = halt.reason.as_ref().map_or_else(
                || "unspecified".to_owned(),
                |token| token.as_str().to_owned(),
            );
            let held = halt
                .age_secs
                .map_or_else(String::new, |age| format!(", written {} ago", ago(age)));
            let stale = if halt.generation > issued {
                "; its generation is above any this coordinator issued, so it is a halt of an \
                 earlier coordinator, whose cause may be gone"
            } else {
                ""
            };
            alerts.push(Alert {
                subject: Subject {
                    service: want.service.to_owned(),
                    host: id.to_string(),
                    version: host.beat.version.as_str().to_owned(),
                    ..Subject::of(Kind::HostHalt)
                },
                message: format!(
                    "{id} holds its own halt of {}{} ({reason}, generation {}{held}){stale}: \
                     it refuses every order of the service until an operator clears the halt \
                     (Releases.ClearHalt)",
                    want.service,
                    in_group(want.group),
                    halt.generation,
                ),
                said: Said {
                    reason,
                    detail: String::new(),
                    tries: 0,
                    since_unix: halt.age_secs.map_or(0, |age| at.unix.saturating_sub(age)),
                },
            });
        }
    }
    alerts
}

/// `secs` in words: the largest two units of days, hours, minutes and seconds that it holds.
fn ago(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{secs} s"),
        (0, 0, _) => format!("{minutes} min"),
        (0, _, _) => format!("{hours} h {minutes} min"),
        _ => format!("{days} d {hours} h"),
    }
}
