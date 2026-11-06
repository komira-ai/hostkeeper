//! The operator's calls that mark or steer the rollout, as plain functions over the messages of
//! `hostkeeper.operator.v1`: `Releases.MarkBad` and `Releases.Halt`, `Hosts.Abort` and
//! `Hosts.Exclude` (with `abandon`), `Services.Hold` and `Services.Unhold`, and `Audit.List`;
//! `Releases.Force` is in [`crate::force`], `Services.Assign`, `Unassign`, `SetMinVersion` and
//! `List` in [`crate::assign`], `Enrollment.List` and `Enrollment.Approve` in
//! [`crate::operator_enrollment`], and the inventory's calls in [`crate::inventory`]. A transport
//! (the gRPC services, or a JSON gateway) authenticates the bearer ([`Coordinator::caller`]) and
//! hands the call here.
//!
//! Every one of them takes the operator token only: a valid CI token is `forbidden` and no
//! token, or any other, `unauthorized`, before the request is looked at. A refusal is one value
//! of the service's refusal enum and changes nothing; `store_failed` means the ops file could not
//! be written, and asking again is safe. Every change is one entry of the audit ring and file,
//! under the name of the operator's token.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::hold_scope::Scope;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::is_service_name;
use hostkeeper_wire::{HostId, RoleName};

use crate::audit_file::{Logged, Query};
use crate::halt;
use crate::holds::{HoldMark, HoldScope, MAX_HOLDS, marks_of};
use crate::operate::reason;
use crate::ops::{AuditEntry, Exclusion, OpsError, Write, label};
use crate::reconcile::{CI, COORDINATOR};
use crate::service::Coordinator;
use crate::tokens::TokenName;

/// The most entries one `Audit.List` answers: a `limit` of 0, or above it, is this.
pub const MAX_AUDIT_PAGE: usize = 256;

/// Who calls, as the transport found the bearer.
#[derive(Clone, Debug)]
pub enum Caller {
    /// The operator token of this name.
    Operator(TokenName),
    /// A CI token the allow-list accepts: it reaches none of these calls.
    Ci,
    /// No bearer, or one that is no token.
    Anonymous,
}

/// Why a caller may not make an operator's call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Denied {
    /// No operator token, and no CI token either.
    Unauthorized,
    /// A CI token.
    Forbidden,
}

impl Caller {
    /// The name of the operator's token, which the audit records the change under.
    pub(crate) fn operator(&self) -> Result<&str, Denied> {
        match self {
            Self::Operator(name) => Ok(name.as_str()),
            Self::Ci => Err(Denied::Forbidden),
            Self::Anonymous => Err(Denied::Unauthorized),
        }
    }
}

macro_rules! refusals {
    ($($refusal:ident),*) => {$(
        impl From<Denied> for proto::$refusal {
            fn from(denied: Denied) -> Self {
                match denied {
                    Denied::Unauthorized => Self::Unauthorized,
                    Denied::Forbidden => Self::Forbidden,
                }
            }
        }

        impl From<OpsError> for proto::$refusal {
            fn from(why: OpsError) -> Self {
                tracing::error!(%why, "an operator's change could not be written: nothing changed");
                Self::StoreFailed
            }
        }
    )*};
}

refusals!(
    HostsRefusal,
    ReleasesRefusal,
    ServicesRefusal,
    EnrollmentRefusal,
    AuditRefusal
);

/// The reason `text` trimmed, if it is one: 1 to 256 bytes with no control character.
pub(crate) fn valid_reason(text: &str) -> Option<String> {
    reason(text).ok()
}

/// The actor of a change made by `by`: the coordinator, CI, or an operator's token.
fn actor_of(by: &str) -> proto::Actor {
    match by {
        COORDINATOR => proto::Actor::Coordinator,
        CI => proto::Actor::Ci,
        _ => proto::Actor::Operator,
    }
}

/// The actor a filter of `Audit.List` names by its token (`operator`, `ci`, `coordinator`).
fn actor_named(token: &str) -> Option<proto::Actor> {
    match token {
        "operator" => Some(proto::Actor::Operator),
        "ci" => Some(proto::Actor::Ci),
        "coordinator" => Some(proto::Actor::Coordinator),
        _ => None,
    }
}

/// Whether `text` could be an action of the audit: 1 to 64 bytes of `[a-z_]`.
fn is_action(text: &str) -> bool {
    (1..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// `entry` as `hostkeeper.operator.v1` says it, without its time: the answer of the call that
/// made it.
pub(crate) fn entry_proto(entry: &AuditEntry) -> proto::AuditEntry {
    proto::AuditEntry {
        seq: entry.seq,
        by: actor_of(&entry.by).into(),
        action: entry.action.clone(),
        service: entry.service.clone(),
        node_id: entry.node_id.clone(),
        reason: entry.reason.clone(),
        at_unix: None,
        by_name: Some(entry.by.clone()),
    }
}

/// `logged` as `Audit.List` says it: the entry, with the time the audit file gives it.
fn logged_proto(logged: &Logged) -> proto::AuditEntry {
    proto::AuditEntry {
        at_unix: logged.at_unix,
        ..entry_proto(&logged.entry)
    }
}

/// `mark` as `hostkeeper.operator.v1` says it.
pub(crate) fn hold_proto(mark: &HoldMark) -> proto::HoldMark {
    let scope = match &mark.scope {
        HoldScope::Host(id) => Scope::Host(id.clone()),
        HoldScope::Role(role) => Scope::Role(role.clone()),
        HoldScope::Fleet => Scope::Fleet(proto::WholeFleet {}),
    };
    proto::HoldMark {
        scope: Some(proto::HoldScope { scope: Some(scope) }),
        service: mark.service.clone(),
        reason: mark.reason.clone(),
        set_by: mark.by.clone(),
        set_unix: mark.at_unix,
    }
}

/// The scope of a hold request, checked: a node id, a role name, or the fleet.
fn scope_of(scope: Option<proto::HoldScope>) -> Option<HoldScope> {
    match scope?.scope? {
        Scope::Host(id) => id
            .parse::<HostId>()
            .ok()
            .map(|id| HoldScope::Host(id.to_string())),
        Scope::Role(role) => role
            .parse::<RoleName>()
            .ok()
            .map(|role| HoldScope::Role(role.to_string())),
        Scope::Fleet(proto::WholeFleet {}) => Some(HoldScope::Fleet),
    }
}

/// An entry by `by` for `action` about `service`, with `reason`.
fn entry(by: &str, action: &str, service: &str, reason: String) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.service = Some(service.to_owned());
    entry.reason = Some(reason);
    entry
}

impl Coordinator {
    /// Who calls with `bearer`: the operator token of its name, else a CI token the allow-list
    /// accepts (which spends the token, as any check of it does), else no one.
    pub async fn caller(&self, bearer: Option<&str>) -> Caller {
        if let Ok(name) = self.operator(bearer) {
            return Caller::Operator(name);
        }
        match self.ci(bearer).await {
            Ok(_) => Caller::Ci,
            Err(_) => Caller::Anonymous,
        }
    }

    /// `Releases.MarkBad`: marks a version of a service bad by hand, any version the service's
    /// scheme reads. No host is ordered it again (the gate holds it at `bad`) until an operator
    /// unmarks it or a force lifts the mark, and CI cannot set it. A held pair is not reached.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the version or the
    /// reason), `already_bad`, `store_failed`.
    pub fn mark_bad(
        &self,
        caller: &Caller,
        request: proto::MarkBadRequest,
    ) -> Result<proto::MarkBadResponse, proto::ReleasesRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::ReleasesRefusal::UnknownService)?;
        let version = Version::parse(&request.version)
            .ok()
            .filter(|version| def.version_scheme.parses(version))
            .ok_or(proto::ReleasesRefusal::InvalidRequest)?;
        let why = valid_reason(&request.reason).ok_or(proto::ReleasesRefusal::InvalidRequest)?;
        // Under the fleet's lock, as the coordinator's own marks are made: no heartbeat decides
        // between the mark and its write.
        let _fleet = self.lock_fleet();
        let audit = entry(by, "mark_bad", &def.name, format!("{version}: {why}"));
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let marks = state.services.entry(def.name.clone()).or_default();
            if marks.bad.contains(&version) {
                return Err(proto::ReleasesRefusal::AlreadyBad);
            }
            marks.bad.push(version.clone());
            Ok(())
        })?;
        Ok(proto::MarkBadResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Releases.Halt`: halts a service by hand, as a host's halt report does
    /// ([`halt::raise_by_operator`]): no host of it is ordered until `ClearHalt` or a force. The
    /// halt stands at the service's desired version.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the reason, or a
    /// service with no desired version, which has nothing to halt), `already_halted`,
    /// `store_failed` (the halt stands in memory, and its write is tried again at every
    /// heartbeat).
    pub fn halt(
        &self,
        caller: &Caller,
        request: proto::HaltRequest,
    ) -> Result<proto::HaltResponse, proto::ReleasesRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::ReleasesRefusal::UnknownService)?;
        let why = valid_reason(&request.reason).ok_or(proto::ReleasesRefusal::InvalidRequest)?;
        // Raised under the fleet's lock, as a host's halt is: never between another heartbeat's
        // decision to order and its order.
        let _fleet = self.lock_fleet();
        let target = self
            .desired
            .get(&def.name)
            .map(|desired| desired.version)
            .ok_or(proto::ReleasesRefusal::InvalidRequest)?;
        let done = halt::raise_by_operator(
            &self.ops,
            &def.name,
            &target,
            by,
            &why,
            proto::ReleasesRefusal::AlreadyHalted,
        )?;
        Ok(proto::HaltResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Hosts.Abort`: queues an `abort` of the service for the host's next heartbeat
    /// ([`crate::orders`]). It takes no slot, is sent once, and only to a daemon that announced
    /// [`crate::orders::ABORT_FEATURE`]; asking again while one is queued queues nothing more.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id or the reason),
    /// `unknown_service`, `unknown_host` (not heard from since the coordinator started),
    /// `store_failed`.
    pub fn abort(
        &self,
        caller: &Caller,
        request: proto::AbortRequest,
    ) -> Result<proto::AbortResponse, proto::HostsRefusal> {
        let by = caller.operator()?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::HostsRefusal::UnknownService)?;
        let why = valid_reason(&request.reason).ok_or(proto::HostsRefusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        if fleet.node(&id).is_none() {
            return Err(proto::HostsRefusal::UnknownHost);
        }
        let mut audit = entry(by, "abort", &def.name, why);
        audit.node_id = Some(id.to_string());
        let done = self.ops.change(Write::BeforeMemory, audit, |_| {})?;
        fleet.queue_abort(id, def.name.clone(), format!("abort-{}", done.seq));
        Ok(proto::AbortResponse {
            queued: true,
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Hosts.Exclude`: takes the host out of the service, as `/v1/operator`'s `exclude` does.
    /// Its slot stays held until its own beats or its order's age free it; with `abandon`, for a
    /// host that is gone, the slot is freed now, the entry is `exclude_abandon` with
    /// `possibly_mid_update` in its reason, and an alert is logged (`alert = "exclude_abandon"`).
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the node id or the
    /// reason), `capacity`, `store_failed`.
    pub fn exclude(
        &self,
        caller: &Caller,
        request: proto::ExcludeRequest,
    ) -> Result<proto::ExcludeResponse, proto::HostsRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::HostsRefusal::UnknownService)?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let why = valid_reason(&request.reason).ok_or(proto::HostsRefusal::InvalidRequest)?;
        let now = (self.clock)();
        let mut fleet = self.lock_fleet();
        let (action, said) = if request.abandon {
            ("exclude_abandon", format!("{why}; possibly_mid_update"))
        } else {
            ("exclude", why.clone())
        };
        let mut audit = entry(by, action, &def.name, said);
        audit.node_id = Some(id.to_string());
        let exclusion = Exclusion {
            service: def.name.clone(),
            node_id: id.to_string(),
            reason: why,
        };
        let done =
            crate::operate::exclude(&self.ops, exclusion, audit, proto::HostsRefusal::Capacity)?;
        if request.abandon {
            let held = fleet.abandon_in(&def.name, &id, now);
            tracing::error!(
                alert = "exclude_abandon",
                service = %def.name,
                node_id = %id,
                held_slot = held,
                "an operator abandoned a host that may be mid-update: its update slot is free now"
            );
        } else {
            fleet.forget_in(&def.name, &id);
        }
        Ok(proto::ExcludeResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Services.Hold`: holds the service on a host, on every host of a role, or on the whole
    /// fleet ([`crate::holds`]). A hold of the same service and scope replaces the one before.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the scope, the reason,
    /// or a hold past [`MAX_HOLDS`]), `store_failed`.
    pub fn hold(
        &self,
        caller: &Caller,
        request: proto::HoldRequest,
    ) -> Result<proto::HoldResponse, proto::ServicesRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::ServicesRefusal::UnknownService)?;
        let scope = scope_of(request.scope).ok_or(proto::ServicesRefusal::InvalidRequest)?;
        let why = valid_reason(&request.reason).ok_or(proto::ServicesRefusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        let mark = HoldMark {
            service: def.name.clone(),
            scope,
            reason: why,
            by: by.to_owned(),
            at_unix: self.ops.unix_now(),
        };
        let mut audit = entry(
            by,
            "hold",
            &def.name,
            format!("{}: {}", mark.scope.describe(), mark.reason),
        );
        if let HoldScope::Host(id) = &mark.scope {
            audit.node_id = Some(id.clone());
        }
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let before = state.holds.len();
            state
                .holds
                .retain(|hold| (&hold.service, &hold.scope) != (&mark.service, &mark.scope));
            if state.holds.len() == before && before >= MAX_HOLDS {
                return Err(proto::ServicesRefusal::InvalidRequest);
            }
            state.holds.push(mark.clone());
            Ok(())
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::HoldResponse {
            hold: Some(hold_proto(&mark)),
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Services.Unhold`: releases the hold of the service in the scope; its pairs are ordered
    /// to the desired version of their next heartbeat.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the scope or the
    /// reason), `not_held`, `store_failed`.
    pub fn unhold(
        &self,
        caller: &Caller,
        request: proto::UnholdRequest,
    ) -> Result<proto::UnholdResponse, proto::ServicesRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::ServicesRefusal::UnknownService)?;
        let scope = scope_of(request.scope).ok_or(proto::ServicesRefusal::InvalidRequest)?;
        let why = valid_reason(&request.reason).ok_or(proto::ServicesRefusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        let mut audit = entry(
            by,
            "unhold",
            &def.name,
            format!("{}: {why}", scope.describe()),
        );
        if let HoldScope::Host(id) = &scope {
            audit.node_id = Some(id.clone());
        }
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let before = state.holds.len();
            state
                .holds
                .retain(|hold| (&hold.service, &hold.scope) != (&def.name, &scope));
            if state.holds.len() == before {
                return Err(proto::ServicesRefusal::NotHeld);
            }
            Ok(())
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::UnholdResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Audit.List`: the entries of the audit file after `since_seq` that match every filter
    /// set, oldest first, at most `limit` ([`MAX_AUDIT_PAGE`] for 0 or more): the oldest of
    /// them, or with `newest` the newest; and the `since_seq` of the next page. Each has the
    /// time of its line and the name it was made under. The file is read a chunk at a time, not
    /// whole ([`crate::audit_file::page_in`]). Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a filter that is no actor token, action,
    /// service name, node id or token name), `store_failed` (the audit file could not be read).
    pub fn audit_list(
        &self,
        caller: &Caller,
        request: proto::ListAuditRequest,
    ) -> Result<proto::ListAuditResponse, proto::AuditRefusal> {
        caller.operator()?;
        let invalid = proto::AuditRefusal::InvalidRequest;
        let actor = match request.actor.as_deref() {
            Some(token) => Some(actor_named(token).ok_or(invalid)?),
            None => None,
        };
        let bad_action = request.action.as_deref().is_some_and(|a| !is_action(a));
        let bad_service = request
            .service
            .as_deref()
            .is_some_and(|s| !is_service_name(s));
        let bad_node = request
            .node_id
            .as_deref()
            .is_some_and(|n| n.parse::<HostId>().is_err());
        let bad_name = request.by_name.as_deref().is_some_and(|n| !label(n));
        if bad_action || bad_service || bad_node || bad_name {
            return Err(invalid);
        }
        let limit = match request.limit as usize {
            0 => MAX_AUDIT_PAGE,
            limit => limit.min(MAX_AUDIT_PAGE),
        };
        let keep = |e: &AuditEntry| {
            actor.is_none_or(|actor| actor_of(&e.by) == actor)
                && request.by_name.as_ref().is_none_or(|n| *n == e.by)
                && request.action.as_ref().is_none_or(|a| *a == e.action)
                && (request.service.is_none() || e.service == request.service)
                && (request.node_id.is_none() || e.node_id == request.node_id)
        };
        let page = self.ops.audit_page(&Query {
            since_seq: request.since_seq,
            limit,
            newest: request.newest,
            keep: &keep,
        })?;
        Ok(proto::ListAuditResponse {
            next_seq: page.last().map_or(request.since_seq, |l| l.entry.seq),
            entries: page.iter().map(logged_proto).collect(),
        })
    }
}
