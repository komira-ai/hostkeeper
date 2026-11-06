//! `Hosts.List` and `Hosts.Get`: the hosts the coordinator has heard from since it started, as
//! plain functions over the messages of `hostkeeper.operator.v1`.
//!
//! Both read what `Releases.Status` reads, in one snapshot under the fleet's lock
//! ([`Coordinator::status_in`]): every node a heartbeat noted, and for each how every service
//! that heard it, or does not count it, sees it ([`crate::grpc::report::host`]). They take the
//! operator token only, as every call of [`crate::operator`] does, and change nothing.

use std::sync::PoisonError;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use crate::grpc::report;
use crate::operator::Caller;
use crate::service::Coordinator;

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

    /// `Hosts.List`: the hosts heard from since the coordinator started, by node id, after the
    /// node id `page_token` names, at most `limit` ([`MAX_HOSTS_PAGE`] for 0 or above it), and
    /// the token of the next page (the last node id of this one; empty on the last page).
    /// Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token that is no node id).
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
        let now = (self.clock)();
        let unix_now = self.ops.unix_now();
        let fleet = self.lock_fleet();
        let status = self.status_in(&fleet, now);
        let mut rest = fleet
            .nodes()
            .filter(|(id, _)| after.as_ref().is_none_or(|after| *id > after));
        let hosts: Vec<proto::Host> = rest
            .by_ref()
            .take(size)
            .map(|(id, node)| report::host(id, node, &status, now, unix_now))
            .collect();
        let next_page_token = match hosts.last() {
            Some(last) if rest.next().is_some() => last.node_id.clone(),
            _ => String::new(),
        };
        Ok(proto::ListHostsResponse {
            hosts,
            next_page_token,
        })
    }

    /// `Hosts.Get`: the host `node_id`, as `Hosts.List` answers it. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id), `unknown_host` (not heard
    /// from since the coordinator started).
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
        let fleet = self.lock_fleet();
        let node = fleet.node(&id).ok_or(proto::HostsRefusal::UnknownHost)?;
        let status = self.status_in(&fleet, now);
        Ok(proto::GetHostResponse {
            host: Some(report::host(&id, node, &status, now, unix_now)),
        })
    }
}
