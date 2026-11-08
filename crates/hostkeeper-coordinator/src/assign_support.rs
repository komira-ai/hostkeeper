//! Builders for the tests of assignment, install and uninstall: the calls of `Services` as the
//! operator makes them, what a daemon that takes installs is told, a service it reports absent,
//! and why the gate holds a pair now. The rig is [`crate::marks_support::rig`]: `web` and `db`
//! on the roles `daemon` and `storage`.

use std::sync::atomic::Ordering;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::min_version::Scope as MinScope;
use hostkeeper_wire::wire::{Health, ServiceBeat, ServiceReply};

use crate::decide::Why;
use crate::marks_support::{INSTALLER, WEB, ask_typed, beat_of, digest, operator, runs};
use crate::orders::{Order, install_order_id};
use crate::registry::Digest;
use crate::testutil::{Rig, id, version};

/// `Services.Assign` of `service` to the host `node` or the role `role` (one of them empty), as
/// the operator.
pub(crate) fn assign(
    rig: &Rig,
    service: &str,
    node: &str,
    role: &str,
) -> Result<proto::AssignResponse, proto::ServicesRefusal> {
    rig.coordinator.assign(
        &operator(),
        proto::AssignRequest {
            node_id: node.to_owned(),
            service: service.to_owned(),
            role: role.to_owned(),
        },
    )
}

/// `Services.Unassign` of `service` from the host `node` or the role `role`, as the operator.
pub(crate) fn unassign(
    rig: &Rig,
    service: &str,
    node: &str,
    role: &str,
    uninstall: bool,
) -> Result<proto::UnassignResponse, proto::ServicesRefusal> {
    rig.coordinator.unassign(
        &operator(),
        proto::UnassignRequest {
            node_id: node.to_owned(),
            service: service.to_owned(),
            uninstall,
            role: role.to_owned(),
        },
    )
}

/// `Services.SetMinVersion` of `service` for `scope` to `version` (empty: remove it), as the
/// operator.
pub(crate) fn set_min(
    rig: &Rig,
    service: &str,
    scope: MinScope,
    version: &str,
) -> Result<proto::SetMinVersionResponse, proto::ServicesRefusal> {
    rig.coordinator.set_min_version(
        &operator(),
        proto::SetMinVersionRequest {
            service: service.to_owned(),
            min_version: Some(proto::MinVersion {
                scope: Some(scope),
                version: version.to_owned(),
            }),
        },
    )
}

/// What `host`, a daemon of the role `daemon` that takes installs, is told when it lists
/// `services`.
pub(crate) fn told(
    rig: &Rig,
    host: &str,
    services: Vec<ServiceBeat>,
) -> (ServiceReply, Option<Order>) {
    ask_typed(rig, host, "daemon", beat_of(INSTALLER, services))
}

/// The order `told` gives `host`, which must answer nothing on the JSON reply.
pub(crate) fn order_to(rig: &Rig, host: &str, services: Vec<ServiceBeat>) -> Option<Order> {
    let (reply, order) = told(rig, host, services);
    assert_eq!(reply, ServiceReply::Nothing, "{host}");
    order
}

/// `service` as a host reports it staged and never run: `absent`, at `running`.
pub(crate) fn absent(service: &str, running: &str) -> ServiceBeat {
    let mut one = runs(service, running);
    one.health = Health::Absent;
    one
}

/// The first try of the install of `web` at `to` from the package of [`digest`] `n`.
pub(crate) fn install(to: &str, n: u8) -> Order {
    let digest = Digest::parse(&digest(n)).expect("a digest");
    Order::Install {
        target: None,
        service: WEB.to_owned(),
        version: version(to),
        order_id: install_order_id(&digest),
        digest,
        index_digest: None,
        order_try: 1,
    }
}

/// The try `order_try` of the uninstall of `service` by the order `order_id`.
pub(crate) fn uninstall_order(service: &str, order_id: &str, order_try: u64) -> Order {
    Order::Uninstall {
        service: service.to_owned(),
        order_id: order_id.to_owned(),
        order_try,
    }
}

/// Why the gate holds `service` on `host` now, or `None` when it would order it.
pub(crate) fn why_of(rig: &Rig, service: &str, host: &str) -> Option<Why> {
    let now = Duration::from_secs(rig.clock.load(Ordering::SeqCst));
    let coordinator = &rig.coordinator;
    let fleet = coordinator.lock_fleet();
    crate::halt::decide(
        coordinator.ops(),
        &coordinator.services,
        &coordinator.desired.all(),
        service,
        &fleet,
        &id(host),
        now,
    )
    .why()
}

/// The hosts ordered to uninstall `service`, by node id, as the ops store holds them.
pub(crate) fn intents(rig: &Rig, service: &str) -> Vec<String> {
    rig.coordinator.ops().read(|state| {
        state
            .assignments
            .get(service)
            .map(|entries| entries.uninstall.keys().cloned().collect())
            .unwrap_or_default()
    })
}
