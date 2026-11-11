//! `Releases.Halt` once the coordinator holds a host group (`docs/design/hostgroups.md`, section
//! 2.3): a halt is a (group, package)'s, and the request, which names no group, halts the package
//! in every group that lists it and has a desired version of it, each at its own desired version.
//! A group already halted is left as it is; `already_halted` is the answer when none was left to
//! halt. One audit entry for each group halted, naming it; the answer holds the first.
//!
//! `Releases.ClearHalt` clears the halts the same way ([`crate::operate::operate`]).

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::halt;
use crate::operator::{entry_proto, valid_reason};
use crate::service::Coordinator;

type Refusal = proto::ReleasesRefusal;

impl Coordinator {
    /// `Releases.Halt` for a coordinator that holds a host group.
    ///
    /// # Errors
    /// As [`Self::halt`], and `not_in_host_group` for a package no group lists.
    pub(crate) fn halt_in_groups(
        &self,
        by: &str,
        request: proto::HaltRequest,
    ) -> Result<proto::HaltResponse, Refusal> {
        let service = self
            .package_def(&request.service)
            .ok_or(Refusal::UnknownService)?
            .name
            .clone();
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        // Raised under the fleet's lock, as a host's halt is.
        let _fleet = self.lock_fleet();
        let groups = self.desired.host_groups();
        let reached = self.reach(&groups, &self.ops.state(), &service, "")?;
        let targets: Vec<_> = reached
            .iter()
            .filter_map(|one| {
                let wanted = self.desired.group_desired(&one.name, &service)?;
                Some((one.name.clone(), wanted.version))
            })
            .collect();
        if targets.is_empty() {
            return Err(Refusal::InvalidRequest);
        }
        let mut done = halt::raise_by_operator_in(
            &self.ops,
            &service,
            &targets,
            by,
            &why,
            Refusal::AlreadyHalted,
        )?;
        Ok(proto::HaltResponse {
            entry: Some(entry_proto(&done.swap_remove(0))),
        })
    }
}
