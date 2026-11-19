//! A heartbeat decided in one transaction under the rollout gate, from the fleet's rows
//! (`docs/design/coordinator-multi-instance.md`, sections 5.2 and 5.3).
//!
//! Over a store other coordinators share, a heartbeat's decision is the backend's deciding
//! transaction ([`crate::store::decision`]). Before it asks for the gate, the heartbeat takes the
//! writers' locks of the desired and the operations store (the order every change takes them in),
//! so no store's change made inside the decision waits for a lock a thread waiting for the gate
//! holds. Under the gate it takes into memory each part another coordinator changed, then the
//! fleet's rows: the orders, bakes, tries and quiet time become the fleet's
//! ([`crate::fleet::Fleet::take_rows`]), and each host's beat another coordinator heard is noted
//! as this coordinator notes its own ([`Coordinator::fold`]). The heartbeat is then decided by the
//! code that always decided it, its stores' changes written in the decision's transaction as they
//! are made; at its end the orders that differ from the rows are written, each conditional on the
//! row it read, and the decision commits.
//!
//! What is not decided gives no order: a gate held (the host asks again at its next beat), a
//! store lost or a change pending in memory (nothing is decided until the database takes writes
//! again), a decision whose writes or commit failed. A decision that did not land puts the
//! stores' memory back as it was before it, so nothing it made is answered or read.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat};

use super::Coordinator;
use crate::agent_service::own_beat;
use crate::desired;
use crate::fleet::{Fleet, Node};
use crate::group_scope::Basis;
use crate::membership::{Standing, standing};
use crate::ops::OpsState;
use crate::ops::pending::Checkpoint;
use crate::services::ServiceDef;
use crate::store::decision::{HostRow, OrderRow, Rows};
use crate::store::{Decision, Opened, Writing};
use crate::trusted_root::TrustedRootSaid;
use crate::verdict_file;

/// One beat of a host, to note in the fleet.
pub(super) struct Hearing<'a> {
    pub(super) host: &'a HostId,
    /// The role its certificate names.
    pub(super) role: &'a str,
    /// The service its top level speaks for, if any.
    pub(super) legacy: Option<&'a str>,
    pub(super) node: &'a Node,
    pub(super) beat: Heartbeat,
    pub(super) announced: &'a [ServiceBeat],
    /// The agent's own entry, taken out of the beat's list ([`own_beat`]).
    pub(super) own: Option<&'a ServiceBeat>,
}

/// How a heartbeat is decided.
pub(super) enum Mode<'a> {
    /// Over memory: the store is one process's own.
    Alone,
    /// Not at this beat: nothing new is ordered.
    Skip,
    /// In the decision's transaction.
    Deciding(Box<Deciding<'a>>),
}

/// A heartbeat's decision under way, with what it puts back if it does not land.
pub(super) struct Deciding<'a> {
    decision: Box<Decision>,
    /// The orders it read.
    read: Vec<OrderRow>,
    /// The tries no longer outstanding when it took the rows in.
    sent: BTreeMap<(HostId, String), u64>,
    desired: desired::Held,
    ops: Checkpoint,
    verdicts: verdict_file::Held,
    _writers: (Writing<'a>, Writing<'a>),
}

/// The node a beat of a host makes: what it runs from its certificate's `role`, by the services
/// `beat` names, heard at `seen`.
pub(super) fn node_of(
    role: &str,
    legacy: Option<&str>,
    beat: &Heartbeat,
    trusted_root: TrustedRootSaid,
    seen: Duration,
) -> Node {
    let announced = beat.services.clone().unwrap_or_default();
    Node {
        role: role.to_owned(),
        features: beat.agent_features.clone(),
        agent: beat.agent.clone(),
        platform: beat.platform,
        seen,
        announced: legacy
            .map(|name| (name.to_owned(), beat.health))
            .into_iter()
            .chain(announced.iter().map(|one| (one.name.clone(), one.health)))
            .collect(),
        sent_unix: beat.sent_unix,
        cert_not_after_unix: beat.cert_not_after_unix,
        last_renew_error: beat.last_renew_error.clone(),
        trusted_root,
        skewed_beats: 0,
    }
}

impl Coordinator {
    /// Notes `hearing`, a beat of a host decided over `basis` with the operations state `ops`, in
    /// `fleet` at `now`: each service that counts the host records its word, a service that no
    /// longer counts it forgets it (or keeps what an update it still runs occupies), as a
    /// heartbeat has always been noted.
    pub(super) fn fold(
        fleet: &mut Fleet,
        basis: &Basis<'_>,
        ops: &OpsState,
        hearing: Hearing<'_>,
        now: Duration,
    ) {
        let Hearing {
            host,
            role,
            legacy,
            node,
            beat,
            announced,
            own,
        } = hearing;
        let platform = beat.platform;
        let of = |def: &ServiceDef| standing(def, host, node, ops, now);
        for def in basis.services.defs() {
            if of(def) == Standing::Dropped {
                fleet.forget_in(&def.name, host);
            }
        }
        let counted = |def: &ServiceDef| of(def) == Standing::Counted;
        let dropped = |def: &ServiceDef| of(def) == Standing::Dropped;
        // A host ordered to uninstall a service it is no longer assigned says how the uninstall
        // goes: kept as what it occupies, as a dropped host's word is.
        let uninstalling = |def: &ServiceDef| {
            ops.assignments
                .get(&def.name)
                .is_some_and(|entries| entries.uninstall.contains_key(host.as_str()))
        };
        if let Some(name) = legacy {
            match basis.services.get(name) {
                Some(def) if dropped(def) => {
                    fleet.record_outside_in(name, host.clone(), role, beat, now);
                }
                top if top.is_none_or(counted) => {
                    fleet.record_named_in(name, host.clone(), role, beat, now);
                }
                _ => {}
            }
        }
        for one in announced.iter().chain(own) {
            match basis.services.get(&one.name) {
                Some(def) if counted(def) => fleet.record_named_in(
                    &one.name,
                    host.clone(),
                    role,
                    one.as_heartbeat(platform),
                    now,
                ),
                // A host taken off the service while it ran an update of it says when that ends.
                Some(def)
                    if dropped(def)
                        || uninstalling(def)
                        || (fleet.get_in(&one.name, host).is_none()
                            && fleet.occupant(&one.name, host).is_some()) =>
                {
                    fleet.record_outside_in(
                        &one.name,
                        host.clone(),
                        role,
                        one.as_heartbeat(platform),
                        now,
                    );
                }
                _ => {}
            }
        }
    }

    /// Opens the heartbeat's decision, at the coordinator's time `now`, with `fleet` locked: over
    /// a shared store, its transaction under the gate, the parts it read afresh taken into memory
    /// and the fleet's rows into `fleet`.
    pub(super) fn open_decision(&self, fleet: &mut Fleet, now: Duration) -> Mode<'_> {
        let writers = (self.desired.hold_writers(), self.ops.hold_writers());
        let mut decision = match self.ops.backend().decide() {
            Ok(Opened::Alone) => return Mode::Alone,
            Ok(Opened::Busy) => return Mode::Skip,
            Err(why) => {
                tracing::debug!(%why, "a heartbeat is not decided: the store cannot decide now");
                return Mode::Skip;
            }
            Ok(Opened::Deciding(decision)) => decision,
        };
        if self.ops.halt_unwritten() {
            // A change stands in memory and not in the rows (its commit failed, and the retry at
            // this heartbeat too): nothing is decided until it lands.
            return Mode::Skip;
        }
        if let Some(held) = decision.take_desired() {
            self.desired.take_decided(held);
        }
        if let Some((state, facts)) = decision.take_ops() {
            self.ops.take_decided(state, facts);
        }
        if let Some(held) = decision.take_verdicts() {
            self.verdicts.take_decided(held);
        }
        self.refresh_marks(fleet);
        let rows = decision.rows().clone();
        fleet.take_rows(&rows, now);
        self.take_in_hosts(fleet, &rows, now);
        Mode::Deciding(Box::new(Deciding {
            decision,
            read: rows.orders,
            sent: fleet.sent_tries(),
            desired: self.desired.copy(),
            ops: self.ops.checkpoint(),
            verdicts: self.verdicts.copy(),
            _writers: writers,
        }))
    }

    /// Notes in `fleet` the beat of each host of `rows` that another coordinator heard and whose
    /// row changed since `fleet` took it in, placed at its time on this coordinator's clock.
    fn take_in_hosts(&self, fleet: &mut Fleet, rows: &Rows, now: Duration) {
        let ops = self.ops.state();
        for row in &rows.hosts {
            if fleet.row_is_new(&row.host_id, row.seen_unix, row.sent_unix, &row.instance) {
                self.take_in_host(fleet, &ops, row, rows.now_unix, now);
            }
        }
    }

    /// Notes the beat of `row` in `fleet`; a row that does not read is left out, and the host's
    /// next beat replaces it.
    fn take_in_host(
        &self,
        fleet: &mut Fleet,
        ops: &OpsState,
        row: &HostRow,
        now_unix: i64,
        now: Duration,
    ) {
        let (Ok(host), Ok(mut beat)) = (
            row.host_id.parse::<HostId>(),
            serde_json::from_slice::<Heartbeat>(&row.beat),
        ) else {
            tracing::warn!(node_id = %row.host_id, "a host's row does not read: left out");
            return;
        };
        let age = u64::try_from(now_unix.saturating_sub(row.seen_unix)).unwrap_or(0);
        let seen = now.saturating_sub(Duration::from_secs(age));
        let basis = self.basis_of(&host);
        let own = own_beat(&basis.services, &mut beat);
        let legacy = self.legacy_for(&beat).map(str::to_owned);
        let node = node_of(
            &row.role,
            legacy.as_deref(),
            &beat,
            TrustedRootSaid::default(),
            seen,
        );
        let announced = beat.services.clone().unwrap_or_default();
        Self::fold(
            fleet,
            &basis,
            ops,
            Hearing {
                host: &host,
                role: &row.role,
                legacy: legacy.as_deref(),
                node: &node,
                beat,
                announced: &announced,
                own: own.as_ref(),
            },
            seen,
        );
        fleet.note_node(host, node, self.ops.unix_now());
    }

    /// Ends `deciding`, the decision of a heartbeat answered from `fleet` at `now`: writes the
    /// orders that differ from the rows it read and commits. True when it landed; otherwise the
    /// stores' memory is put back as it was before it, and nothing it decided is answered.
    pub(super) fn end_decision(
        &self,
        fleet: &Fleet,
        deciding: Box<Deciding<'_>>,
        now: Duration,
    ) -> bool {
        let Deciding {
            decision,
            read,
            sent,
            desired,
            ops,
            verdicts,
            _writers,
        } = *deciding;
        let now_unix = decision.rows().now_unix;
        let writes = fleet.writes_since(&read, &sent, now, now_unix);
        match decision.finish(&writes) {
            Ok(()) => true,
            Err(why) => {
                tracing::warn!(%why, "a heartbeat's decision did not land: it is ordered nothing");
                self.desired.rewind(desired);
                self.ops.rewind(ops);
                self.verdicts.take_decided(verdicts);
                false
            }
        }
    }
}
