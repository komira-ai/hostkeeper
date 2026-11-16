//! The heartbeat's decisions over the fleet's rows, for coordinators that share a database
//! (`docs/design/coordinator-multi-instance.md`, section 5).
//!
//! A heartbeat is noted in the fleet in memory as it always was, and then stored as the host's
//! row ([`Said`]: the beat as the fleet took it in, the host's role and what the host said of its
//! trusted root), with the orders it ended. Everything it decides is decided in one transaction
//! that holds the rollout gate ([`Coordinator::decide_over_rows`]): the transaction reads the rows
//! first, takes in the beats of the hosts it has not heard (those another coordinator stored),
//! the orders, the bakes and the quiet time, and only then runs the rules of [`crate::decide`] on
//! the fleet so made; the order it gives is written in the same transaction, and answered when the
//! transaction has committed. The fleet in memory is a cache of the rows, refreshed under the
//! gate before each decision, never the source of one.
//!
//! The desired state and the operations state are not read under the gate. They are memory, and
//! each heartbeat first takes in what another coordinator committed
//! ([`Coordinator::refresh_from_rows`]), so a version, a pause or a plan's position set through
//! another coordinator is seen here at the next beat. A change committed between that read and
//! the gate is seen at the beat after; the gate's own checks (the slots, the queue, a halt, the
//! stage's position) refuse what that stale read could give.
//!
//! A beat that finds the gate held, or the rows out of reach, is answered with no order: the host
//! asks again at its next beat. Coordinators over files have no rows and decide on the fleet
//! they hold ([`Coordinator::decide_tail`] with nothing asked of the rows).

use std::cmp::Reverse;
use std::sync::Arc;
use std::sync::MutexGuard;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Heartbeat, ServiceBeat as WireBeat, ServiceReply, VerdictAck};
use serde::{Deserialize, Serialize};

use super::Coordinator;
use super::gate_orders::Asked;
use crate::fleet::{DOWN_AFTER, Fleet, Node, ORDER_AGE};
use crate::group_scope::Basis;
use crate::identity::Identity;
use crate::membership::{Standing, standing};
use crate::ops::OpsState;
use crate::order_terms::Terms;
use crate::orders::Order;
use crate::reconcile::reconcile_asking;
use crate::rollouts::Step;
use crate::services::ServiceDef;
use crate::store::postgres::FleetRows;
use crate::store::postgres::fleet_rows::{HostBeat, HostRow, ServiceBeat, Timing};
use crate::store::postgres::fleet_session::Gate;
use crate::store::postgres::rollout::{Admission, Ask, Gated, Limits, Uninstalling, Wait};
use crate::trusted_root::TrustedRootSaid;
use crate::verdict_file;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;

/// The rules of the fleet's rows a coordinator is configured with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowRules {
    /// How far behind the stored beat a beat may be, in the stored row's age, before the host is
    /// believed again (`--beat-reorder-secs`).
    pub(crate) beat_reorder: Duration,
    /// How long a service may go without asking before it leaves its place in the queue
    /// (`--queue-idle-secs`).
    pub(crate) queue_idle: Duration,
}

/// The coordinator's fleet rows and the rules it reads them by.
pub(crate) struct Rows {
    pub(crate) rows: Arc<FleetRows>,
    pub(crate) rules: RowRules,
}

/// What a beat says, as its row keeps it: the beat as the fleet took it in, and what the node
/// holds beyond it.
#[derive(Serialize, Deserialize)]
pub(super) struct Said {
    legacy: Option<String>,
    beat: Heartbeat,
    own: Option<WireBeat>,
    root_age_secs: Option<u64>,
    root_reason: i32,
}

impl Said {
    /// What `beat` says, heard for `legacy`, with `own` among the services and `node`'s root.
    pub(super) fn of(
        legacy: Option<&str>,
        beat: &Heartbeat,
        own: Option<&WireBeat>,
        node: &Node,
    ) -> Self {
        Self {
            legacy: legacy.map(str::to_owned),
            beat: beat.clone(),
            own: own.cloned(),
            root_age_secs: node.trusted_root.age_secs,
            root_reason: node.trusted_root.reason as i32,
        }
    }

    fn root(&self) -> TrustedRootSaid {
        TrustedRootSaid {
            age_secs: self.root_age_secs,
            reason: agent::TrustedRootReason::try_from(self.root_reason)
                .unwrap_or(agent::TrustedRootReason::Unspecified),
        }
    }
}

/// The node a heartbeat of the host makes: what it runs, from its certificate's `role`, by the
/// services `beat` names, heard at `seen`.
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

/// One host's beat to note in the fleet.
pub(super) struct Hearing<'a> {
    pub(super) host: &'a HostId,
    pub(super) role: &'a str,
    pub(super) legacy: Option<&'a str>,
    pub(super) node: Node,
    pub(super) beat: Heartbeat,
    pub(super) announced: &'a [WireBeat],
    pub(super) own: Option<&'a WireBeat>,
}

/// What the decisions of one heartbeat are made from.
pub(super) struct Tail<'a> {
    pub(super) who: &'a Identity,
    pub(super) legacy: Option<&'a str>,
    pub(super) announced: &'a [WireBeat],
    pub(super) held: &'a [String],
    pub(super) heard: &'a [verdict_file::Heard],
    pub(super) basis: &'a Basis<'a>,
    pub(super) orders: bool,
    pub(super) now: Duration,
}

/// The order about to be given, which the rows must admit first.
pub(super) struct Placing<'a> {
    pub(super) service: &'a str,
    pub(super) role: &'a str,
    pub(super) target: &'a Version,
    pub(super) order_try: u64,
    /// The order id of an uninstall; none for an update or an install.
    pub(super) uninstall: Option<&'a str>,
    /// The most hosts of the role updating at once, of the desired state.
    pub(super) max_updating: u32,
    /// The most services rolling at once.
    pub(super) live_rollouts: usize,
}

/// What the decisions of a heartbeat ask before they write: whether an order may be given, and
/// whether a stage is still the one to enter. Over the fleet's rows the rows answer
/// ([`RowJudge`]); over the fleet in memory nothing is asked ([`Unjudged`]).
pub(super) trait Judge {
    /// Whether the order of `placing` may be given.
    fn give(&mut self, placing: &Placing<'_>) -> bool;
    /// The order given to the host was not given after all: take it back.
    fn take_back(&mut self);
    /// Whether the plan is still at the stage `step` read it at.
    fn enter(&mut self, step: &Step<'_>) -> bool;
}

/// Nothing is asked: the fleet in memory is the only one.
pub(super) struct Unjudged;

impl Judge for Unjudged {
    fn give(&mut self, _: &Placing<'_>) -> bool {
        true
    }

    fn take_back(&mut self) {}

    fn enter(&mut self, _: &Step<'_>) -> bool {
        true
    }
}

/// The rows judge, in the transaction that holds the gate.
struct RowJudge<'g, 'a> {
    gate: &'g mut Gate<'a>,
    rules: RowRules,
    who: &'g Identity,
    unix_now: i64,
    /// The first failure of the rows: every later answer is no, and the transaction is rolled back.
    failed: Option<postgres::Error>,
}

impl RowJudge<'_, '_> {
    fn answer(&mut self, asked: Result<bool, postgres::Error>) -> bool {
        match asked {
            Ok(answer) => answer && self.failed.is_none(),
            Err(e) => {
                self.failed.get_or_insert(e);
                false
            }
        }
    }
}

impl Judge for RowJudge<'_, '_> {
    fn give(&mut self, placing: &Placing<'_>) -> bool {
        let asked = give(self.gate, self.rules, placing, self.who, self.unix_now);
        self.answer(asked)
    }

    fn take_back(&mut self) {
        let undone = self
            .gate
            .take_back(self.who.host_id.as_str())
            .map(|()| true);
        self.answer(undone);
    }

    fn enter(&mut self, step: &Step<'_>) -> bool {
        let asked = self.gate.stage_is_at(
            step.group,
            step.service,
            step.version,
            i64::try_from(step.from).unwrap_or(i64::MAX),
            i64::try_from(step.first).unwrap_or(i64::MAX),
        );
        self.answer(asked)
    }
}

impl Coordinator {
    /// The coordinator over `rows`, read under the rollout gate by every heartbeat.
    #[must_use]
    pub(crate) fn with_rows(mut self, rows: Arc<FleetRows>, rules: RowRules) -> Self {
        self.rows = Some(Rows { rows, rules });
        self
    }

    /// Has `hook` run by each heartbeat over the fleet's rows between its beat's store and its
    /// ask for the gate.
    #[cfg(test)]
    pub(crate) fn pause_before_gate(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self
            .before_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    /// Has `hook` run by each heartbeat over the fleet's rows inside the gate, after it read the
    /// rows and before it decides: the other coordinator's beat in it finds the gate held.
    #[cfg(test)]
    pub(crate) fn pause_in_gate(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self
            .in_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    fn run_in_gate(&self) {
        let hook = self
            .in_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn run_before_gate(&self) {
        let hook = self
            .before_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Takes into memory the desired state and the operations state another coordinator
    /// committed, before a heartbeat reads them: an operator's edit, a halt, a stage entered or a
    /// version set through that coordinator is then seen here at the next beat, and a plan
    /// position read here is the one the rows hold. One query says whether the rows hold an
    /// audit line newer than the newest this coordinator has; only then are the states read.
    ///
    /// Memory is not replaced while it holds a change not yet committed, or when it moved while
    /// the rows were read: the next beat reads them again. The read is not under the gate, so a
    /// change committed after it is seen at the next beat; what an order or a stage writes is
    /// still asked of the rows under the gate ([`Gate::admit`], [`Gate::stage_is_at`]).
    pub(super) fn refresh_from_rows(&self) {
        let Some(rows) = &self.rows else {
            return;
        };
        let known = self.ops.newest_seq();
        match rows.rows.state_after(known) {
            Ok(None) => {}
            Ok(Some(fresh)) => {
                self.desired.take_rows(
                    fresh.desired,
                    Box::new(|| self.ops.take_rows(fresh.ops, known)),
                );
            }
            Err(why) => {
                tracing::warn!(%why, "the state other coordinators committed is not read from the rows");
            }
        }
    }

    /// Takes the orders the rows hold for the host `host` into `fleet`, before its beat is noted:
    /// the beat then ends the order it ends, as it does where the order was given, and the row of
    /// the order is ended with it ([`Self::store_beat`]).
    pub(super) fn take_orders_of(&self, fleet: &mut Fleet, host: &HostId, now: Duration) {
        let Some(rows) = &self.rows else {
            return;
        };
        match rows.rows.orders_of(host.as_str()) {
            Ok(found) => {
                let unix_now = i64::try_from(self.ops.unix_now()).unwrap_or(i64::MAX);
                fleet.sync_orders_of(host, &found, now, unix_now);
            }
            Err(why) => {
                tracing::warn!(node_id = %host, %why, "the orders of a host are not read from the rows");
            }
        }
    }

    /// Notes the beat of `hearing` in `fleet`: the host leaves the services that no longer count
    /// it, then what it said of each service is recorded as that service's heartbeat of the host.
    pub(super) fn fold_beat(
        &self,
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
        let counted = |def: &ServiceDef| standing(def, host, &node, ops, now) == Standing::Counted;
        for def in basis.services.defs() {
            if standing(def, host, &node, ops, now) == Standing::Dropped {
                fleet.forget_in(&def.name, host);
            }
        }
        let dropped = |def: &ServiceDef| standing(def, host, &node, ops, now) == Standing::Dropped;
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
        fleet.note_node(host.clone(), node, self.ops.unix_now());
    }

    /// Takes the row of a host another coordinator stored into `fleet`, as that coordinator noted
    /// it (a row that does not read is left out, and the host's next beat replaces it).
    fn take_in_row(
        &self,
        fleet: &mut Fleet,
        ops: &OpsState,
        row: &HostRow,
        now: Duration,
        unix_now: i64,
    ) {
        let Ok(host) = row.host_id.parse::<HostId>() else {
            return;
        };
        let Ok(said) = serde_json::from_slice::<Said>(&row.beat) else {
            tracing::warn!(node_id = %row.host_id, "a host's row does not read: left out");
            return;
        };
        let age = u64::try_from(unix_now.saturating_sub(row.seen_unix)).unwrap_or(0);
        let seen = now.saturating_sub(Duration::from_secs(age));
        let basis = self.basis_of(&host);
        let node = node_of(
            &row.role,
            said.legacy.as_deref(),
            &said.beat,
            said.root(),
            seen,
        );
        let announced = said.beat.services.clone().unwrap_or_default();
        self.fold_beat(
            fleet,
            &basis,
            ops,
            Hearing {
                host: &host,
                role: &row.role,
                legacy: said.legacy.as_deref(),
                node,
                beat: said.beat,
                announced: &announced,
                own: said.own.as_ref(),
            },
            seen,
        );
    }

    /// Stores the beat `said` of `host` as the host's row, and the orders it ended: false when
    /// the rows could not be written.
    fn store_beat(&self, rows: &Rows, fleet: &mut Fleet, host: &HostId, said: &Said) -> bool {
        // A heartbeat that was read from JSON writes as JSON.
        let bytes = serde_json::to_vec(said).unwrap_or_default();
        let beat = &said.beat;
        let counts = |health: Health, updating: bool, pending: bool| {
            health == Health::Healthy && !updating && !pending
        };
        let entries: Vec<(&str, String, bool)> = said
            .legacy
            .as_deref()
            .map(|name| {
                (
                    name,
                    beat.version.as_str().to_owned(),
                    counts(beat.health, beat.updating.is_some(), beat.promotion_pending),
                    beat.health,
                )
            })
            .into_iter()
            .chain(
                beat.services
                    .iter()
                    .flatten()
                    .chain(said.own.as_ref())
                    .map(|one| {
                        (
                            one.name.as_str(),
                            one.version.as_str().to_owned(),
                            counts(one.health, one.updating.is_some(), one.promotion_pending),
                            one.health,
                        )
                    }),
            )
            // A copy that is absent is staged only: it settles no order.
            .map(|(name, version, counts, health)| {
                (
                    name,
                    if health == Health::Absent {
                        String::new()
                    } else {
                        version
                    },
                    counts,
                )
            })
            .collect();
        let services: Vec<ServiceBeat<'_>> = entries
            .iter()
            .map(|(service, target, counts)| ServiceBeat {
                service,
                target,
                counts: *counts,
            })
            .collect();
        let unix_now = i64::try_from(self.ops.unix_now()).unwrap_or(i64::MAX);
        let role = fleet
            .node(host)
            .map_or_else(String::new, |node| node.role.clone());
        let stored = HostBeat {
            host_id: host.as_str(),
            role: &role,
            beat: &bytes,
            sent_unix: i64::try_from(beat.sent_unix.unwrap_or(0)).unwrap_or(0),
            up: counts(beat.health, beat.updating.is_some(), beat.promotion_pending),
            running: None,
            repo_report: None,
            instance: "",
            services: &services,
        };
        let timing = Timing {
            now: unix_now,
            down_after: secs(DOWN_AFTER),
            reorder: secs(rows.rules.beat_reorder),
            quiet: secs(fleet.quiet()),
        };
        let ended: Vec<String> = fleet
            .take_ended_orders()
            .into_iter()
            .filter(|(id, _)| id == host)
            .map(|(_, service)| service)
            .collect();
        let ended: Vec<&str> = ended.iter().map(String::as_str).collect();
        let written = rows.rows.beat(&stored, &timing).and_then(|_| {
            if ended.is_empty() {
                Ok(())
            } else {
                rows.rows.end_orders(host.as_str(), &ended)
            }
        });
        match written {
            Ok(()) => true,
            Err(why) => {
                tracing::error!(node_id = %host, %why, "the fleet's rows are not written: nothing is ordered");
                false
            }
        }
    }

    /// The decisions of a heartbeat over the fleet's rows: stores the beat, and then, in one
    /// transaction that holds the rollout gate, reads the rows into `fleet`, decides, and writes
    /// the order. Nothing is ordered when the rows cannot be reached or the gate is held.
    pub(super) fn decide_over_rows(
        &self,
        rows: &Rows,
        mut fleet: MutexGuard<'_, Fleet>,
        said: Said,
        tail: &Tail<'_>,
    ) -> (ServiceReply, Option<Order>, Option<Terms>, Vec<VerdictAck>) {
        let nothing = (ServiceReply::Nothing, None, None, Vec::new());
        if !self.store_beat(rows, &mut fleet, &tail.who.host_id, &said) {
            return nothing;
        }
        #[cfg(test)]
        self.run_before_gate();
        let fleet: &mut Fleet = &mut fleet;
        let unix_now = i64::try_from(self.ops.unix_now()).unwrap_or(i64::MAX);
        let gated = rows.rows.gated(Wait::Beat, |gate| {
            let snapshot = gate.snapshot()?;
            let now = tail.now;
            if !snapshot.hosts.is_empty() {
                let ops = self.ops.state();
                for row in &snapshot.hosts {
                    self.take_in_row(fleet, &ops, row, now, unix_now);
                }
            }
            fleet.sync_bakes(&snapshot.bakes, now, unix_now);
            fleet.sync_orders(&snapshot.orders, now, unix_now);
            fleet.sync_quiet(snapshot.quiet_until, now, unix_now);
            #[cfg(test)]
            self.run_in_gate();
            let mut judge = RowJudge {
                gate,
                rules: rows.rules,
                who: tail.who,
                unix_now,
                failed: None,
            };
            let answer = self.decide_tail(fleet, tail, &mut judge);
            if let Some(e) = judge.failed.take() {
                return Err(e);
            }
            // A service every host runs has no rollout left: it leaves the queue.
            let done: Vec<&str> = tail
                .legacy
                .into_iter()
                .chain(tail.announced.iter().map(|one| one.name.as_str()))
                .filter(|name| {
                    tail.basis.services.get(name).is_some_and(|def| {
                        self.desired_in(tail.basis, name)
                            .is_some_and(|want| crate::rollouts::rolled(def, &want, fleet))
                    })
                })
                .collect();
            if !done.is_empty() {
                gate.finish(&done)?;
            }
            Ok(answer)
        });
        match gated {
            Ok(Gated::Decided(answer)) => answer,
            Ok(Gated::Busy) => nothing,
            Err(why) => {
                tracing::error!(%why, "a decision over the fleet's rows failed: nothing is ordered");
                nothing
            }
        }
    }

    /// The decisions of one heartbeat, on `fleet`: the intents settle, the plans reconcile and
    /// enter their stages, the move steps, the verdicts are acknowledged, the operator's orders go
    /// and then the gate's, each order and each stage first asked of `judge` (with rows, the rows
    /// say).
    pub(super) fn decide_tail(
        &self,
        fleet: &mut Fleet,
        tail: &Tail<'_>,
        judge: &mut dyn Judge,
    ) -> (ServiceReply, Option<Order>, Option<Terms>, Vec<VerdictAck>) {
        let Tail {
            who,
            legacy,
            announced,
            held,
            heard,
            basis,
            orders,
            now,
        } = *tail;
        crate::intents::settle(&self.ops, &basis.services, fleet, &who.host_id);
        let mut names: Vec<&str> = legacy
            .into_iter()
            .chain(announced.iter().map(|one| one.name.as_str()))
            .collect();
        // Stable: services of one priority keep the order above.
        names.sort_by_key(|name| Reverse(basis.services.get(name).map_or(0, |def| def.priority)));
        // The hosts' words are in: record the last good versions and roll back what failed,
        // before the orders below read the desired states.
        for def in names.iter().filter_map(|name| basis.services.get(name)) {
            // Only the verdicts about the desired version count, and not those an override
            // acknowledged; the others are forgotten.
            if let Some(want) = self.desired_in(basis, &def.name) {
                let acked = self.ops.marks(&def.name).failure_acked;
                fleet.keep_verdicts_in(def, &want.version, &acked);
            }
            reconcile_asking(def, &self.ops, &self.desired, fleet, now, &mut |step| {
                judge.enter(step)
            });
        }
        self.step_move(fleet, basis, &who.host_id, now);
        // The verdicts are durable before the answer acknowledges them, and before any order.
        let acks = verdict_file::acknowledge(&self.verdicts, fleet, &who.host_id, heard, &names);
        // The operator's orders go first, past the gate, to a transport that carries them.
        if orders
            && !fleet.store_lost
            && let Some(order) = self.ops.read(|state| {
                crate::orders::next(fleet, state, &who.host_id, &basis.services, &names)
            })
        {
            return (ServiceReply::Nothing, Some(order), None, acks);
        }
        let asked = Asked {
            who,
            legacy,
            held,
            basis,
            orders,
            now,
        };
        let (reply, order, terms) = self.gate_order(fleet, &asked, names, judge).unwrap_or((
            ServiceReply::Nothing,
            None,
            None,
        ));
        (reply, order, terms, acks)
    }
}

/// Asks the rows whether the order of `placing` may be given to the host `who`.
fn give(
    gate: &mut Gate<'_>,
    rules: RowRules,
    placing: &Placing<'_>,
    who: &Identity,
    unix_now: i64,
) -> Result<bool, postgres::Error> {
    let ask = Ask {
        host_id: who.host_id.as_str(),
        role: placing.role,
        service: placing.service,
        target: placing.target.as_str(),
        now: unix_now,
        order_try: i64::try_from(placing.order_try).unwrap_or(i64::MAX),
    };
    if let Some(order_id) = placing.uninstall {
        return Ok(matches!(
            gate.place_uninstall(&ask, order_id)?,
            Uninstalling::Placed | Uninstalling::Existing
        ));
    }
    let limits = Limits {
        max_updating: i64::from(placing.max_updating),
        // The rule is per service, over the rows just read: the fleet's own (`below_min_up`).
        min_up: 0,
        live_rollouts: i64::try_from(placing.live_rollouts).unwrap_or(i64::MAX),
        order_age: secs(ORDER_AGE),
        down_after: secs(DOWN_AFTER),
        queue_idle: secs(rules.queue_idle),
    };
    Ok(matches!(
        gate.admit(&ask, &limits)?,
        Admission::Placed | Admission::Existing
    ))
}

fn secs(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}
