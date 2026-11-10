//! What the tests of `host-groups migrate` share: a store of enrolled hosts, a rollout file and
//! the plan made of them.

use std::collections::BTreeMap;

use hostkeeper_enrollment::enroll::records::pb;

use crate::assignment::{Entry, ServiceAssignments};
use crate::desired::{Desired, RoleLimits};
use crate::enrollment::MacAllowlist;
use crate::migrate_plan::{Inputs, Plan, plan};
use crate::services::Services;
use crate::store::postgres::transfer::Moved;
use crate::testutil::version;

/// The time of every plan of the tests.
pub(crate) const NOW: u64 = 1_900_000_000;

/// An enrolled host `id` of role `role`.
pub(crate) fn host(id: &str, role: &str) -> pb::Host {
    pb::Host {
        schema: 1,
        host_id: id.to_owned(),
        key_hash: vec![7; 32],
        role: role.to_owned(),
        ..pb::Host::default()
    }
}

/// A desired state at `at`.
pub(crate) fn desired(at: &str) -> Desired {
    Desired {
        version: version(at),
        max_updating: RoleLimits::default(),
        min_up: RoleLimits::default(),
        release_digest: None,
        release_index: None,
    }
}

/// A store of enrolled hosts, `(host id, role)` each, and what the tests set on it.
#[derive(Clone, Default)]
pub(crate) struct Store {
    pub(crate) moved: Moved,
}

impl Store {
    /// A store of the hosts `hosts`.
    pub(crate) fn of(hosts: &[(&str, &str)]) -> Self {
        let mut store = Self::default();
        store.moved.records.hosts = hosts.iter().map(|(id, role)| host(id, role)).collect();
        store
    }

    /// The label `key` = `value` an operator gave the host `id`.
    pub(crate) fn label(&mut self, id: &str, key: &str, value: &str) {
        self.moved
            .loaded
            .ops
            .hosts
            .entry(id.to_owned())
            .or_default()
            .labels
            .insert(key.to_owned(), value.to_owned());
    }

    /// The assignments of `service`, edited by `edit`.
    pub(crate) fn assignments(
        &mut self,
        service: &str,
        edit: impl FnOnce(&mut ServiceAssignments),
    ) {
        edit(
            self.moved
                .loaded
                .ops
                .assignments
                .entry(service.to_owned())
                .or_default(),
        );
    }

    /// The host `id` taken out of `service`, or put in it.
    pub(crate) fn entry(&mut self, service: &str, id: &str, entry: Entry) {
        self.assignments(service, |a| {
            a.hosts.insert(id.to_owned(), entry);
        });
    }

    /// The desired state of `service`.
    pub(crate) fn want(&mut self, service: &str, state: Desired) {
        self.moved
            .loaded
            .desired
            .desired
            .insert(service.to_owned(), state);
    }

    /// The plan of the store over the rollout file `rollout`, with no allowlist.
    pub(crate) fn plan(&self, rollout: &str) -> Plan {
        self.plan_with(rollout, None)
    }

    /// The plan of the store over the rollout file `rollout` and the allowlist `allowlist`.
    pub(crate) fn plan_with(&self, rollout: &str, allowlist: Option<&str>) -> Plan {
        let services = Services::parse(rollout).expect("a rollout file");
        self.plan_services(&services, allowlist)
    }

    /// The plan of the store over `services`, which a rollout file may not hold, and the
    /// allowlist `allowlist`.
    pub(crate) fn plan_services(&self, services: &Services, allowlist: Option<&str>) -> Plan {
        let allowlist = allowlist.map(|text| MacAllowlist::parse(text).expect("an allowlist"));
        plan(&Inputs {
            moved: &self.moved,
            rollout: services,
            allowlist: allowlist.as_ref(),
            live_rollouts: 1,
            now: NOW,
        })
    }
}

/// The hosts of `plan`'s groups, by group name.
pub(crate) fn members(plan: &Plan) -> BTreeMap<String, Vec<String>> {
    let groups = &plan.after.loaded.desired.host_groups.0;
    let mut out: BTreeMap<String, Vec<String>> = groups
        .keys()
        .map(|name| (name.to_string(), Vec::new()))
        .collect();
    for host in &plan.changed.hosts {
        out.entry(host.host_group.clone())
            .or_default()
            .push(host.host_id.clone());
    }
    out
}
