//! The calls of [`Coordinator`] that ask enrollment: who an enrolled host's certificate names,
//! whether a host is denied, retiring or duplicate, what a heartbeat noted of the host, and a
//! retiring host's final report.

use std::sync::Arc;

use std::time::Duration;

use hostkeeper_enrollment::enroll::HostRunning;
use hostkeeper_wire::wire::{Answer, Health, Heartbeat};

use super::{Coordinator, HeartbeatRefused};
use crate::agent_service::runs_itself;
use crate::enrollment::{Admission, Enrollment};
use crate::fleet::Node;
use crate::identity::{Identity, IdentityError, identity_of};
use crate::membership::{Membership, membership};
use crate::ops::OpsState;

/// What a heartbeat's turn is, once it is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Turn {
    /// Decide its order.
    Order,
    /// Record it and order it nothing: the host is duplicate, or retiring.
    Hold,
}

/// What a heartbeat noted of its host that is kept with the host's enrollment: written by
/// [`Coordinator::keep_noted`] once the heartbeat's decision is over, never while it is open. Over
/// Postgres the decision holds its transaction open on one session while this thread decides,
/// and the enrollment's write takes another, so a write made meanwhile could wait for a lock the
/// decision holds while the decision waits for this thread: a wait no database can see.
#[derive(Debug, Default)]
pub(super) struct Noted {
    /// The heartbeat reported a service serving ([`Coordinator::serves`]).
    pub(super) serving: bool,
    /// What the host runs, to keep with its enrollment
    /// ([`Coordinator::note_running`](crate::service::Coordinator::note_running)).
    pub(super) running: Option<HostRunning>,
}

impl Coordinator {
    /// [`Self::heartbeat`] from the daemon on the machine `machine_id`, when the transport
    /// carries one: with enrollment it is reported live ([`Enrollment::report_live`]), so two
    /// machines speaking for one host id mark it duplicate. A duplicate host is heard and
    /// ordered nothing ([`Self::host_is_duplicate`]).
    ///
    /// # Errors
    /// As [`Self::heartbeat`].
    pub fn heartbeat_from(
        &self,
        who: Identity,
        beat: Heartbeat,
        machine_id: Option<&str>,
    ) -> Result<Answer, HeartbeatRefused> {
        self.answer(
            who,
            beat,
            false,
            machine_id,
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .map(|(answer, _)| answer)
    }

    /// The turn of a heartbeat of `host` from the daemon on `machine_id`, if the transport
    /// said: with enrollment, the heartbeat is noted ([`Enrollment::note_heartbeat`], which
    /// keeps the first since the host's approval) and the host reported live first, and a
    /// duplicate host is held.
    pub(super) fn turn_of(&self, host: &hostkeeper_wire::HostId, machine_id: Option<&str>) -> Turn {
        if let Some(enrollment) = &self.enrollment {
            enrollment.note_heartbeat(host);
            if let Some(machine_id) = machine_id {
                enrollment.report_live(host, machine_id);
            }
        }
        if self.host_is_duplicate(host) {
            Turn::Hold
        } else {
            Turn::Order
        }
    }

    /// Whether, with enrollment, the heartbeat `beat` of `host`, heard as `node` at `now`,
    /// reported a service serving, which [`Self::keep_noted`] keeps with the host
    /// ([`Enrollment::note_serving`], which keeps the first since the host's approval): a service
    /// of `services` (those the heartbeat is decided over: the rollout file's, or the host's
    /// group's) that counts the host as present, other than the agent itself, healthy, with no
    /// update of it under way.
    pub(super) fn serves(
        &self,
        services: &crate::services::Services,
        host: &hostkeeper_wire::HostId,
        node: &Node,
        beat: &Heartbeat,
        ops: &OpsState,
        now: Duration,
    ) -> bool {
        self.enrollment.is_some()
            && beat.services.iter().flatten().any(|one| {
                one.health == Health::Healthy
                    && one.updating.is_none()
                    && !runs_itself(node, &one.name)
                    && services.get(&one.name).is_some_and(|def| {
                        membership(def, host, node, ops, now) == Membership::Present
                    })
            })
    }

    /// Keeps with the enrollment of `host` what its heartbeat `noted`: that it served
    /// ([`Enrollment::note_serving`]) and what it runs ([`Enrollment::note_running`]). The caller
    /// holds the fleet's lock, so two heartbeats of one host write in the order they were heard,
    /// and no decision is open on this thread ([`Noted`]).
    pub(super) fn keep_noted(&self, host: &hostkeeper_wire::HostId, noted: Noted) {
        let Some(enrollment) = &self.enrollment else {
            return;
        };
        if noted.serving {
            enrollment.note_serving(host);
        }
        if let Some(running) = &noted.running {
            enrollment.note_running(host, running);
        }
    }

    /// The same coordinator with `enrollment`: it reads a client certificate its CA issued as
    /// the host it names, refuses a denied host's heartbeat, and orders a duplicate host
    /// nothing. Enrollment audits into this coordinator's ops store from now on, and an
    /// approval it recorded whose audit entry is missing there is audited now
    /// ([`Enrollment::audit_to`]). It places each host it approves by this coordinator's host
    /// groups while it holds one ([`Enrollment::held_groups`]).
    #[must_use]
    pub fn with_enrollment(mut self, enrollment: Arc<Enrollment>) -> Self {
        enrollment.audit_to(Arc::clone(&self.ops));
        enrollment.place_by(Arc::clone(&self.desired));
        self.enrollment = Some(enrollment);
        self
    }

    /// Enrollment, if the coordinator has it.
    #[must_use]
    pub fn enrollment(&self) -> Option<&Arc<Enrollment>> {
        self.enrollment.as_ref()
    }

    /// Who the client certificate `der` names: an updater's certificate of the farm CA (its
    /// subjectAltNames under [`Self::prefixes`]), else, with enrollment, a host's client
    /// certificate its CA issued ([`Enrollment::identity_of`]).
    ///
    /// # Errors
    /// [`IdentityError`]: why the certificate is no updater's, when it is no enrolled host's
    /// either.
    pub fn identify(&self, der: &[u8]) -> Result<Identity, IdentityError> {
        identity_of(der, &self.prefixes).or_else(|why| {
            self.enrollment
                .as_ref()
                .and_then(|enrollment| enrollment.identity_of(der))
                .ok_or(why)
        })
    }

    /// Whether the heartbeat of `who` is answered, read for its final report only, or
    /// refused, by its host's deny row ([`Enrollment::admission`]); without enrollment every
    /// heartbeat is answered.
    #[must_use]
    pub fn admission(&self, who: &Identity) -> Admission {
        self.enrollment
            .as_ref()
            .map_or(Admission::Admitted, |enrollment| {
                enrollment.admission(&who.host_id)
            })
    }

    /// Whether `host` is ordered nothing because two live daemons speak for it (design 5.4,
    /// the gate rule `duplicate`): the enrollment store's mark ([`Enrollment::is_duplicate`]).
    #[must_use]
    pub fn host_is_duplicate(&self, host: &hostkeeper_wire::HostId) -> bool {
        self.enrollment
            .as_ref()
            .is_some_and(|enrollment| enrollment.is_duplicate(host))
    }

    /// The final report of a retiring host (design 6.1): its heartbeat is heard as any other,
    /// and no order is made for it. When its update record is final (no service updating, no
    /// order outstanding: [`crate::fleet::Fleet::release_if_final`]), the host is forgotten, which frees the
    /// update slot it held, and its deny row is made final ([`Enrollment::retire`]), so its
    /// later heartbeats are refused unread. What it said of its trusted root is not kept: the
    /// host is unknown there. Returns whether it was released.
    pub fn final_report(&self, who: Identity, beat: Heartbeat) -> bool {
        let host = who.host_id.clone();
        let unknown = crate::trusted_root::TrustedRootSaid::default();
        let mut fleet = self.lock_fleet();
        let now = (self.clock)();
        self.tend_store(now);
        let basis = self.basis_of(&host);
        let mut noted = Noted::default();
        self.reply_to(
            &mut fleet,
            who,
            beat,
            false,
            (Turn::Hold, unknown),
            &basis,
            now,
            &mut noted,
        );
        self.keep_noted(&host, noted);
        drop(fleet);
        let released = self.lock_fleet().release_if_final(&host, (self.clock)());
        if released {
            self.running.forget(&host);
            self.operator_keys.forget(&host);
            self.ssh_reports.forget(&host);
            self.serial_console_reports.forget(&host);
            self.prerequisites.forget(&host);
            tracing::info!(host_id = %host, "a retiring host reported its update final: slot freed");
            if let Some(enrollment) = &self.enrollment {
                enrollment.retire(&host);
            }
        }
        released
    }
}
