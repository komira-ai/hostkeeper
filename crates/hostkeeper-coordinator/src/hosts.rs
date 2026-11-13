//! `Hosts.List`, `Hosts.Get` and `Hosts.Retire`, as plain functions over the messages of
//! `hostkeeper.operator.v1`.
//!
//! `Get` answers a host the coordinator has heard from since it started; `List` answers those and
//! every host the enrollment store holds as enrolled and not denied, by node id, a host not heard
//! since the start as its enrollment kept it ([`crate::running::unheard`]: not live, its daemon and
//! what it ran). The enrolled ids are read before the fleet's lock is taken, and the enrollment
//! of each host of the page after it. For the hosts heard, both read
//! what `Releases.Status` reads, in one snapshot under the fleet's lock
//! ([`Coordinator::status_in`]): every node a heartbeat noted, and for each how every service
//! that heard it, or does not count it, sees it ([`crate::grpc::report::host`]). They change
//! nothing.
//!
//! After that snapshot, and outside the fleet's lock, each host is filled in from the host
//! inventory ([`crate::inventory::show_inventory`]): when it was approved, and the record its
//! enrollment matches, if one does. A host the enrollment store does not hold as enrolled (or
//! a coordinator without enrollment) answers with those fields empty.
//!
//! `Retire` revokes an enrolled host (design 5.7 step 7 and 6.1): its deny row is written
//! ([`crate::enrollment::Enrollment::deny_host`]), so its heartbeats and its `Renew` are refused
//! at once; its own entries in the operator's service assignments are dropped
//! ([`crate::assignment::ServiceAssignments::forget_host`]), with the `host_retire` audit entry
//! in the same write of the ops file; and its update slot is freed now when no update or order
//! of it is in flight ([`crate::fleet::Fleet::release_if_final`]), else when the host reports the
//! update's record final ([`Coordinator::final_report`]). It sends the host no `uninstall`.
//!
//! All three take the operator token only, as every call of [`crate::operator`] does.
//!
//! Once the coordinator holds a host group, a host's `host_group`, and its `role` with it, name the
//! group its enrollment records.

use std::collections::BTreeSet;
use std::sync::PoisonError;

use hostkeeper_enrollment::enroll::EnrolledHost;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use crate::audit_file::{Filter, Query};
use crate::enrollment::{ACTION_APPROVE, ACTION_REPLACE, UNGROUPED_ROLE};
use crate::grpc::report;
use crate::holds::marks_of;
use crate::inventory::{RecordIndex, show_inventory, unreadable};
use crate::operator::{Caller, entry_proto, optional_reason};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

/// The audit action of a retirement.
pub const ACTION_RETIRE: &str = "host_retire";

/// The most hosts one `Hosts.List` answers: a `limit` of 0, or above it, is this.
pub const MAX_HOSTS_PAGE: usize = 256;

impl Coordinator {
    /// The same coordinator counting a host's heartbeat as skewed when the time it says is
    /// further than `after` from the coordinator's wall clock, either way (`--clock-skew-secs`),
    /// instead of [`crate::fleet::CLOCK_SKEW_AFTER`]. `Hosts.List` and `Hosts.Get` answer each
    /// host's count ([`crate::fleet::Fleet::note_node`]).
    #[must_use]
    pub fn with_clock_skew(mut self, after: Duration) -> Self {
        self.fleet
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .set_skew_after(after);
        self
    }

    /// `Hosts.List`: the hosts heard from since the coordinator started and the hosts enrolled and
    /// not denied that were not, by node id, after the
    /// node id `page_token` names, at most `limit` ([`MAX_HOSTS_PAGE`] for 0 or above it), and
    /// the token of the next page (the last node id of this one; empty on the last page).
    /// Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token that is no node id),
    /// `store_failed` (the enrollment store cannot be read).
    pub fn list_hosts(
        &self,
        caller: &Caller,
        request: proto::ListHostsRequest,
    ) -> Result<proto::ListHostsResponse, proto::HostsRefusal> {
        caller.operator()?;
        let after = match request.page_token.as_str() {
            "" => None,
            token => Some(
                token
                    .parse::<HostId>()
                    .map_err(|_| proto::HostsRefusal::InvalidRequest)?,
            ),
        };
        let size = match request.limit as usize {
            0 => MAX_HOSTS_PAGE,
            limit => limit.min(MAX_HOSTS_PAGE),
        };
        let enrolled_ids = self
            .enrolled_ids()
            .map_err(unreadable(proto::HostsRefusal::StoreFailed))?;
        let now = (self.clock)();
        let unix_now = self.ops.unix_now();
        let (page, more) = {
            let fleet = self.lock_fleet();
            let status = self.status_in(&fleet, now);
            let desired = self.desired.all();
            let past = |id: &HostId| after.as_ref().is_none_or(|after| id > after);
            let ids: BTreeSet<HostId> = fleet
                .nodes()
                .map(|(id, _)| id)
                .chain(&enrolled_ids)
                .filter(|id| past(id))
                .cloned()
                .collect();
            let mut rest = ids.into_iter();
            let page: Vec<(HostId, Option<proto::Host>)> = rest
                .by_ref()
                .take(size)
                .map(|id| {
                    let heard = fleet.node(&id).map(|node| {
                        let mut host = report::host(&id, node, &status, now, unix_now);
                        crate::picks::show(&fleet, &id, &mut host, &self.services, &desired);
                        self.show_repositories(&fleet, &id, &mut host);
                        host
                    });
                    (id, heard)
                })
                .collect();
            (page, rest.next().is_some())
        };
        let next_page_token = match page.last() {
            Some((last, _)) if more => last.to_string(),
            _ => String::new(),
        };
        let enrolled = page
            .iter()
            .map(|(id, _)| self.enrolled_host(id.as_str()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(unreadable(proto::HostsRefusal::StoreFailed))?;
        let (mut hosts, shown) = self.page_hosts(page, enrolled);
        self.show_inventory_with(&mut hosts, &shown);
        self.show_groups(&mut hosts);
        Ok(proto::ListHostsResponse {
            hosts,
            next_page_token,
        })
    }

    /// `Hosts.Get`: the host `node_id`, as `Hosts.List` answers it. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id), `unknown_host` (not heard
    /// from since the coordinator started), `store_failed` (the enrollment store cannot be
    /// read).
    pub fn get_host(
        &self,
        caller: &Caller,
        request: &proto::GetHostRequest,
    ) -> Result<proto::GetHostResponse, proto::HostsRefusal> {
        caller.operator()?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let now = (self.clock)();
        let unix_now = self.ops.unix_now();
        let host = {
            let fleet = self.lock_fleet();
            let node = fleet.node(&id).ok_or(proto::HostsRefusal::UnknownHost)?;
            let status = self.status_in(&fleet, now);
            let mut host = report::host(&id, node, &status, now, unix_now);
            crate::picks::show(&fleet, &id, &mut host, &self.services, &self.desired.all());
            self.show_repositories(&fleet, &id, &mut host);
            // The verdict of a `clear_record` the host reported, which no rule reads.
            for one in &mut host.services {
                if let Some(cleared) = fleet.clear_records.shown(&id, &one.service) {
                    one.last_update = Some(report::update_result(cleared));
                }
            }
            host
        };
        let enrolled = self
            .enrolled_host(&host.node_id)
            .map_err(unreadable(proto::HostsRefusal::StoreFailed))?;
        let mut hosts = [host];
        self.show_running(&mut hosts[0], &id, enrolled.as_ref());
        self.show_inventory_with(&mut hosts, &[enrolled]);
        self.show_groups(&mut hosts);
        let [host] = hosts;
        Ok(proto::GetHostResponse { host: Some(host) })
    }

    /// Names the host group of each of `hosts` as the enrollment store records it, in
    /// `host_group` and, for readers that predate groups, in `role`; a host in none is left as it
    /// is, but for the placeholder role of one approved in none, which is shown as none. Nothing, in a coordinator that holds no group.
    fn show_groups(&self, hosts: &mut [proto::Host]) {
        if !self.desired.in_group_mode() {
            return;
        }
        for host in hosts {
            let group = host
                .node_id
                .parse::<HostId>()
                .ok()
                .and_then(|id| self.membership_of(&id));
            if let Some((group, _)) = group {
                host.role = group.to_string();
                host.host_group = group.to_string();
            } else if host.role == UNGROUPED_ROLE {
                // Approved in no group: the placeholder role is no role.
                host.role.clear();
            }
        }
    }

    /// Fills in each of `hosts` from the host inventory, `enrolled` being what the enrollment
    /// store holds of each, under the ops file's lock.
    fn show_inventory_with(&self, hosts: &mut [proto::Host], enrolled: &[Option<EnrolledHost>]) {
        self.ops.read(|state| {
            let index = RecordIndex::new(&state.inventory);
            for (host, enrolled) in hosts.iter_mut().zip(enrolled) {
                show_inventory(host, enrolled.as_ref(), &index);
            }
        });
    }

    /// `Hosts.Retire`: revokes the enrolled host `node_id`, as the module says. The fleet's
    /// lock is held from the deny row to the slot, so two calls at once audit one retirement,
    /// and no assignment is written between the deny row and the drop. The first retirement is
    /// audited. A repeat of a host already denied writes and audits nothing more, unless the
    /// audit log shows the retirement unfinished ([`Coordinator::retire_audited`]: the first
    /// call's ops write failed after its deny row), which it finishes then: the host's entries
    /// are dropped and the retirement audited. Answers whether an update is still in flight on
    /// the host, holding its slot.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id or the reason),
    /// `unknown_host` (no host enrolled with the id; also without enrollment), `store_failed`
    /// (the deny row, or the ops file), `audit_unavailable`. The deny row stands once written:
    /// a failure after it leaves the host refused, and asking again finishes the retirement.
    pub fn retire_host(
        &self,
        caller: &Caller,
        request: &proto::RetireRequest,
    ) -> Result<proto::RetireResponse, proto::HostsRefusal> {
        let by = caller.operator()?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let why = optional_reason(&request.reason, proto::HostsRefusal::InvalidRequest)?;
        let enrollment = self.enrollment().ok_or(proto::HostsRefusal::UnknownHost)?;
        let mut fleet = self.lock_fleet();
        let first = enrollment.deny_host(&id)?;
        let node = id.as_str();
        let entry = if first || !self.retire_audited(node)? {
            let mut audit = AuditEntry::new(by, ACTION_RETIRE);
            audit.node_id = Some(node.to_owned());
            audit.reason = why;
            let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
                for entries in state.assignments.values_mut() {
                    entries.forget_host(node);
                }
                state.assignments.retain(|_, entries| !entries.is_empty());
                Ok::<(), proto::HostsRefusal>(())
            })?;
            fleet.set_marks(self.ops.read(marks_of));
            Some(done)
        } else {
            None
        };
        let update_in_flight = !fleet.release_if_final(&id, (self.clock)());
        drop(fleet);
        if !update_in_flight {
            self.running.forget(&id);
            tracing::info!(host_id = %id, "a retired host held no update: slot freed");
            enrollment.retire(&id);
        }
        Ok(proto::RetireResponse {
            entry: entry.as_ref().map(entry_proto),
            update_in_flight,
        })
    }

    /// Whether the audit log holds the retirement of `node`: the newest of its entries that
    /// approve or retire it is a `host_retire`. An approval that takes the host id back
    /// (`replace`) is newer, so a later retirement of it is owed its own entry. Once the audit
    /// file's retention has deleted the entry, a repeat audits the retirement again.
    ///
    /// # Errors
    /// `store_failed`: the audit log cannot be read (logged).
    fn retire_audited(&self, node: &str) -> Result<bool, proto::HostsRefusal> {
        let newest = self.ops.audit_page(&Query {
            since_seq: 0,
            limit: 1,
            newest: true,
            filter: Filter {
                actions: &[ACTION_RETIRE, ACTION_APPROVE, ACTION_REPLACE],
                node_id: Some(node),
                ..Filter::default()
            },
        })?;
        Ok(newest
            .first()
            .is_some_and(|logged| logged.entry.action == ACTION_RETIRE))
    }
}
