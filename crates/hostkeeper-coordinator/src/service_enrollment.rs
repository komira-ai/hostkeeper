//! The calls of [`Coordinator`] that ask enrollment: who an enrolled host's certificate names,
//! whether a host is denied, retiring or duplicate, and a retiring host's final report.

use std::sync::Arc;

use hostkeeper_wire::wire::{Answer, Heartbeat};

use super::{Coordinator, HeartbeatRefused};
use crate::enrollment::{Admission, Enrollment};
use crate::identity::{Identity, IdentityError, identity_of};

/// What a heartbeat's turn is, once it is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Turn {
    /// Decide its order.
    Order,
    /// Record it and order it nothing: the host is duplicate, or retiring.
    Hold,
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
        self.answer(who, beat, false, machine_id)
            .map(|(answer, _)| answer)
    }

    /// The turn of a heartbeat of `host` from the daemon on `machine_id`, if the transport
    /// said: it is reported live first, and a duplicate host is held.
    pub(super) fn turn_of(&self, host: &hostkeeper_wire::HostId, machine_id: Option<&str>) -> Turn {
        if let (Some(enrollment), Some(machine_id)) = (&self.enrollment, machine_id) {
            enrollment.report_live(host, machine_id);
        }
        if self.host_is_duplicate(host) {
            Turn::Hold
        } else {
            Turn::Order
        }
    }

    /// The same coordinator with `enrollment`: it reads a client certificate its CA issued as
    /// the host it names, refuses a denied host's heartbeat, and orders a duplicate host
    /// nothing.
    #[must_use]
    pub fn with_enrollment(mut self, enrollment: Arc<Enrollment>) -> Self {
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
    /// later heartbeats are refused unread. Returns whether it was released.
    pub fn final_report(&self, who: Identity, beat: Heartbeat) -> bool {
        let host = who.host_id.clone();
        self.reply_to(who, beat, false, Turn::Hold);
        let released = self.lock_fleet().release_if_final(&host, (self.clock)());
        if released {
            tracing::info!(host_id = %host, "a retiring host reported its update final: slot freed");
            if let Some(enrollment) = &self.enrollment {
                enrollment.retire(&host);
            }
        }
        released
    }
}
