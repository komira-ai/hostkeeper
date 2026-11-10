//! The `halt` alerts once the coordinator holds a host group (`docs/design/hostgroups.md`,
//! section 2.3): a halt stands for a (group, package), so it is read from the group's marks
//! ([`OpsState::halt_in`]) for each package a group lists.
//!
//! Groups whose halt of a package has the same host and version are one alert (its subject), which
//! names them all and says who raised the halt of the first of them, in name order; the alert of a
//! halt that differs in host or version stands apart.

use std::collections::BTreeMap;

use super::{Alert, Kind, Subject, halted_by};
use crate::host_groups::{GroupName, HostGroups};
use crate::ops::{Halt, OpsState};

/// The `halt` alerts of the packages `groups` list, in subject order.
pub(super) fn halts_in(ops: &OpsState, groups: &HostGroups) -> Vec<Alert> {
    let mut found: BTreeMap<Subject, (&Halt, Vec<&GroupName>)> = BTreeMap::new();
    for (name, group) in &groups.0 {
        for package in &group.packages {
            let Some(halt) = ops.halt_in(Some(name), package) else {
                continue;
            };
            let subject = Subject {
                service: package.clone(),
                host: halt.node_id.clone(),
                version: halt.target.to_string(),
                ..Subject::of(Kind::Halt)
            };
            found
                .entry(subject)
                .or_insert_with(|| (halt, Vec::new()))
                .1
                .push(name);
        }
    }
    found
        .into_iter()
        .map(|(subject, (halt, named))| {
            let places = named
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let message = format!(
                "{} is halted at {} by {} in host group {places}: nothing is ordered for it \
                 there until an operator clears the halt",
                subject.service,
                halt.target,
                halted_by(halt)
            );
            Alert { subject, message }
        })
        .collect()
}
