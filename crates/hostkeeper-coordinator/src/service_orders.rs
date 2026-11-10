//! The rollout gate's order for a heartbeat: the update, the install or the uninstall of the
//! first service, in the order the gate goes by, that the gate orders the host now
//! ([`crate::decide`]). The operator's orders, which go past the gate, are decided before this
//! ([`crate::orders::next`]).

use std::time::Duration;

use hostkeeper_wire::wire::ServiceReply;

use super::Coordinator;
use crate::acks::release_ack;
use crate::decide::Decision;
use crate::fleet::Fleet;
use crate::halt;
use crate::identity::Identity;
use crate::order_terms::{self, Terms};
use crate::orders::{Order, install_order_id};
use crate::picks;

/// What the gate's order for one heartbeat is decided from.
pub(super) struct Asked<'a> {
    /// The host that sent the heartbeat.
    pub(super) who: &'a Identity,
    /// The legacy service the heartbeat's top level speaks for, if any.
    pub(super) legacy: Option<&'a str>,
    /// The services a halt the heartbeat raised holds: nothing is ordered of them.
    pub(super) held: &'a [String],
    /// Whether the transport carries typed orders: only then is an install or an uninstall
    /// ordered.
    pub(super) orders: bool,
    /// The coordinator's time.
    pub(super) now: Duration,
}

impl Coordinator {
    /// The gate's order for the heartbeat `asked`, under the fleet's lock: the services of
    /// `names` (those the heartbeat speaks for, in the gate's order) first, then every other
    /// service the coordinator manages, which the host may be told to install. An update is the
    /// reply, with its terms for an `order-v1` agent; an install or an uninstall is a typed
    /// order and the reply says nothing. `None` when the gate orders nothing.
    pub(super) fn gate_order<'a>(
        &'a self,
        fleet: &mut Fleet,
        asked: &Asked<'_>,
        mut names: Vec<&'a str>,
    ) -> Option<(ServiceReply, Option<Order>, Option<Terms>)> {
        let id = &asked.who.host_id;
        let now = asked.now;
        // Each service's plan is built once for these decisions: the fleet changes only by the
        // order that ends them.
        let _plans = crate::stages::Memo::begin();
        let desired = self.desired.all();
        // The releases the picks below read: a rollback just made under this lock set another
        // desired version.
        self.keep_releases(fleet);
        let unlisted: Vec<&str> = self
            .services
            .names()
            .filter(|name| !names.contains(name))
            .collect();
        names.extend(unlisted);
        for name in names {
            if asked.held.iter().any(|service| service == name) {
                continue;
            }
            let decision = halt::decide(&self.ops, &self.services, &desired, name, fleet, id, now);
            let order = match decision {
                Decision::Update { version } => {
                    let order_try = fleet.order_try(name, id, &version, false, now);
                    // The entry picked for the host goes out only over gRPC: the JSON route
                    // carries no order, and its host waits for its next gRPC heartbeat.
                    let Some(picked) =
                        picks::order_of(fleet, name, &version, id).filter(|_| asked.orders)
                    else {
                        continue;
                    };
                    let terms = order_terms::terms(name, &version, &picked.package, order_try);
                    // The host's ack goes from the file before the order exists.
                    if !release_ack(&self.ops, name, id) {
                        continue;
                    }
                    fleet.order(name, id.clone(), version.clone(), now);
                    let service = (Some(name) != asked.legacy).then(|| name.to_owned());
                    let reply = ServiceReply::Update { service, version };
                    let typed = order_terms::typed(&reply, terms.as_ref(), Some(picked.target));
                    return Some((reply, typed, terms));
                }
                Decision::Install {
                    version,
                    digest,
                    index_digest,
                } if asked.orders => {
                    if !release_ack(&self.ops, name, id) {
                        continue;
                    }
                    let order_id = install_order_id(&digest);
                    let order_try = fleet.order_try(name, id, &version, false, now);
                    let target = picks::order_of(fleet, name, &version, id).map(|p| p.target);
                    fleet.order(name, id.clone(), version.clone(), now);
                    Order::Install {
                        service: name.to_owned(),
                        version,
                        digest,
                        index_digest,
                        order_id,
                        order_try,
                        target,
                    }
                }
                Decision::Uninstall { order_id, running } if asked.orders => {
                    let order_try = fleet.order_try(name, id, &running, true, now);
                    fleet.order_uninstall(name, id.clone(), running, order_id.clone(), now);
                    Order::Uninstall {
                        service: name.to_owned(),
                        order_id,
                        order_try,
                    }
                }
                _ => continue,
            };
            return Some((ServiceReply::Nothing, Some(order), None));
        }
        None
    }
}
