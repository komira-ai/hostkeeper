//! The rollout gate's order for a heartbeat: the update, the install or the uninstall of the
//! first service, in the order the gate goes by, that the gate orders the host now
//! ([`crate::decide`]). The operator's orders, which go past the gate, are decided before this
//! ([`crate::orders::next`]).

use std::time::Duration;

use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::ServiceReply;

use super::Coordinator;
use crate::acks::release_ack;
use crate::decide::Decision;
use crate::fleet::Fleet;
use crate::halt;
use crate::identity::Identity;
use crate::order_terms::{self, Terms};
use crate::orders::{Order, install_order_id};

/// What the gate's order for one heartbeat is decided from.
pub(super) struct Asked<'a> {
    /// The host that sent the heartbeat.
    pub(super) who: &'a Identity,
    /// The platform the heartbeat names: which package of a release index the host is ordered.
    pub(super) platform: Platform,
    /// The legacy service the heartbeat's top level speaks for, if any.
    pub(super) legacy: Option<&'a str>,
    /// The services a halt the heartbeat raised holds: nothing is ordered of them.
    pub(super) held: &'a [String],
    /// Whether the agent reads order terms (`order-v1`): it is ordered only a version with a
    /// package digest.
    pub(super) order_v1: bool,
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
        let desired = self.desired.all();
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
                    // An agent that reads order terms is ordered only a version with a digest.
                    let terms = asked.order_v1.then(|| {
                        let digests = self.desired.digests(name);
                        order_terms::of_beat(
                            &digests,
                            name,
                            &version,
                            fleet.order_try(name, id, &version, false, now),
                            id,
                            asked.platform,
                        )
                    });
                    let Some(terms) = terms.map_or(Some(None), |made| made.map(Some)) else {
                        continue;
                    };
                    // The host's ack goes from the file before the order exists.
                    if !release_ack(&self.ops, name, id) {
                        continue;
                    }
                    fleet.order(name, id.clone(), version.clone(), now);
                    let service = (Some(name) != asked.legacy).then(|| name.to_owned());
                    return Some((ServiceReply::Update { service, version }, None, terms));
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
                    fleet.order(name, id.clone(), version.clone(), now);
                    Order::Install {
                        service: name.to_owned(),
                        version,
                        digest,
                        index_digest,
                        order_id,
                        order_try,
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
