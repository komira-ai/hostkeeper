//! Builders for the tests of the operator's marks, holds and force: a coordinator that runs two
//! services, `web` and `db`, on the roles `daemon` and `storage`, with no legacy service and a
//! CI token that may set both; the heartbeats of daemons that list both; the operator as a
//! caller; and the requests of the calls.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;
use hostkeeper_proto::hostkeeper::operator::v1::hold_scope::Scope;
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};
use serde_json::json;

use crate::desired::DesiredStore;
use crate::identity::Identity;
use crate::operator::Caller;
use crate::ops::OpsStore;
use crate::orders::Order;
use crate::service::Coordinator;
use crate::services::Services;
use crate::store::{Backend, Fake};
use crate::testutil::{Rig, beat, ci_token, id, role, version};
use crate::verdict_file::VerdictFile;

/// The service most tests hold, mark and force.
pub(crate) const WEB: &str = "web";
/// The second service, which holds a slot elsewhere.
pub(crate) const DB: &str = "db";
/// The features a daemon of this release announces.
pub(crate) const NEW_DAEMON: &[&str] = &[
    "heartbeat-v2",
    "halt-v1",
    "deferred-v1",
    "forced-v1",
    "abort-v1",
];
/// The features of a daemon that also takes the `install` and `uninstall` verbs.
pub(crate) const INSTALLER: &[&str] = &[
    "heartbeat-v2",
    "halt-v1",
    "deferred-v1",
    "forced-v1",
    "abort-v1",
    "install-v1",
];
/// The features of a daemon that predates the force and the abort.
pub(crate) const OLD_DAEMON: &[&str] = &["heartbeat-v2", "halt-v1", "deferred-v1"];

/// A package digest: 64 lower-case hex characters, `n` repeated.
pub(crate) fn digest(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

/// The services `web` and `db`, both on `daemon` and `storage`; `web` with `extra` lines of its
/// definition (`canary = { count = 1 }`, `bake_beats = 2`, `rollback = "automatic"`, ...).
pub(crate) fn services_with(extra: &str) -> Services {
    Services::parse(&format!(
        "[[service]]\nname = \"web\"\nroles = [\"daemon\", \"storage\"]\n{extra}\n\n\
         [[service]]\nname = \"db\"\nroles = [\"daemon\", \"storage\"]\n"
    ))
    .expect("the services")
}

/// A coordinator on the files of `dir` that starts now, running [`services_with`] `extra`.
pub(crate) fn start(dir: &Scratch, clock: &Arc<AtomicU64>, extra: &str) -> Coordinator {
    let list = format!(
        "{}services = [\"web\", \"db\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    Rig::start_with_allow_list(dir, clock, &list)
        .with_services(services_with(extra))
        .with_legacy_service(None)
}

/// [`start`] with the desired state, the ops store and the verdicts on `fake`, as it holds them:
/// a start again on what a coordinator before committed.
pub(crate) fn start_on(fake: &Fake, clock: &Arc<AtomicU64>, extra: &str) -> Coordinator {
    let list = format!(
        "{}services = [\"web\", \"db\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    let loaded = fake.load().expect("the fake loads");
    // One backend under the three stores, as `serve` has: a force is one commit of it.
    let backend: crate::store::Shared = Arc::new(fake.clone());
    Rig::start_over(
        DesiredStore::over(Arc::clone(&backend), loaded.desired),
        clock,
        &list,
    )
    .with_services(services_with(extra))
    .with_legacy_service(None)
    .with_ops(OpsStore::over(Arc::clone(&backend), loaded.ops))
    .with_verdicts(VerdictFile::over(backend, loaded.verdicts))
}

/// A rig running [`services_with`] `extra` on `fake`, its clock at 2000, past the quiet start.
pub(crate) fn rig_on(test: &str, fake: &Fake, extra: &str) -> Rig {
    let mut rig = Rig::new(test);
    rig.coordinator = start_on(fake, &rig.clock, extra);
    rig.at(2_000);
    rig
}

/// A rig running [`services_with`] `extra`, its clock at 2000, past the quiet start.
pub(crate) fn rig(test: &str, extra: &str) -> Rig {
    let mut rig = Rig::new(test);
    rig.coordinator = start(&rig.dir, &rig.clock, extra);
    rig.at(2_000);
    rig
}

/// A coordinator on the same files that starts at the rig's clock now: it is quiet for 30
/// seconds after.
pub(crate) fn restart(rig: &mut Rig, extra: &str) {
    rig.coordinator = start(&rig.dir, &rig.clock, extra);
}

/// Sets `service` to `version` as CI does, registering the digest [`digest`] `n` for it.
pub(crate) async fn set(rig: &Rig, service: &str, version: &str, n: u8) {
    let body = format!(
        r#"{{"service":"{service}","version":"{version}","release_digest":"{}"}}"#,
        digest(n)
    );
    rig.coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
        .expect("set");
}

/// What a host reports of `service`: it runs `running`, healthy.
pub(crate) fn runs(service: &str, running: &str) -> ServiceBeat {
    serde_json::from_value(json!({"name": service, "version": running, "health": "healthy"}))
        .expect("a service beat")
}

/// A heartbeat of a daemon announcing `features` and listing `services`.
pub(crate) fn beat_of(features: &[&str], services: Vec<ServiceBeat>) -> Heartbeat {
    let mut b = beat("1.0");
    b.agent_features = features.iter().map(|f| (*f).to_owned()).collect();
    b.services = Some(services);
    b
}

/// The host `host` of the role `role`, as its certificate says.
pub(crate) fn who(host: &str, role_name: &str) -> Identity {
    Identity {
        host_id: id(host),
        role: role(role_name),
    }
}

/// The operator, with the token named `operator`.
pub(crate) fn operator() -> Caller {
    Caller::Operator("operator".parse().expect("a token name"))
}

/// The reply to a heartbeat of `host` (role `role`) on the JSON route.
pub(crate) fn ask(rig: &Rig, host: &str, role_name: &str, b: Heartbeat) -> ServiceReply {
    rig.coordinator
        .heartbeat(who(host, role_name), b)
        .expect("answered")
        .reply
}

/// The reply and the operator's order for a heartbeat of `host` (role `role`) over a
/// transport that carries typed orders.
pub(crate) fn ask_typed(
    rig: &Rig,
    host: &str,
    role_name: &str,
    b: Heartbeat,
) -> (ServiceReply, Option<Order>) {
    let (answer, order) = rig
        .coordinator
        .heartbeat_with_orders(
            who(host, role_name),
            b,
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .expect("answered");
    (answer.reply, order)
}

/// The reply that orders `service` to `to`.
pub(crate) fn update(service: &str, to: &str) -> ServiceReply {
    ServiceReply::Update {
        service: Some(service.to_owned()),
        version: version(to),
    }
}

/// A hold scope of one host.
pub(crate) fn scope_host(host: &str) -> Option<proto::HoldScope> {
    Some(proto::HoldScope {
        scope: Some(Scope::Host(host.to_owned())),
    })
}

/// A hold scope of a role.
pub(crate) fn scope_role(name: &str) -> Option<proto::HoldScope> {
    Some(proto::HoldScope {
        scope: Some(Scope::Role(name.to_owned())),
    })
}

/// The hold scope of the whole fleet.
pub(crate) fn scope_fleet() -> Option<proto::HoldScope> {
    Some(proto::HoldScope {
        scope: Some(Scope::Fleet(proto::WholeFleet {})),
    })
}

/// Holds `service` in `scope` as the operator.
pub(crate) fn hold(
    rig: &Rig,
    service: &str,
    scope: Option<proto::HoldScope>,
) -> Result<proto::HoldResponse, proto::ServicesRefusal> {
    rig.coordinator.hold(
        &operator(),
        proto::HoldRequest {
            scope,
            service: service.to_owned(),
            reason: "investigating".to_owned(),
        },
    )
}

/// Releases the hold of `service` in `scope` as the operator.
pub(crate) fn unhold(
    rig: &Rig,
    service: &str,
    scope: Option<proto::HoldScope>,
) -> Result<proto::UnholdResponse, proto::ServicesRefusal> {
    rig.coordinator.unhold(
        &operator(),
        proto::UnholdRequest {
            scope,
            service: service.to_owned(),
            reason: "done".to_owned(),
        },
    )
}

/// The fleet target of `service`, confirmed.
pub(crate) fn fleet_target(service: &str) -> Targets {
    Targets::Fleet(proto::FleetTarget {
        confirm_service: service.to_owned(),
    })
}

/// The target of the hosts `hosts`.
pub(crate) fn hosts_target(hosts: &[&str]) -> Targets {
    Targets::Hosts(proto::HostList {
        node_ids: hosts.iter().map(|h| (*h).to_owned()).collect(),
    })
}

/// The target of the role `name`.
pub(crate) fn role_target(name: &str) -> Targets {
    Targets::Role(name.to_owned())
}

/// A force of `service` to `version` at `targets`, with the reason `fix the farm`.
pub(crate) fn force_request(
    service: &str,
    version: &str,
    targets: Targets,
    include_held: bool,
) -> proto::ForceRequest {
    proto::ForceRequest {
        service: service.to_owned(),
        version: version.to_owned(),
        reason: "fix the farm".to_owned(),
        targets: Some(targets),
        include_held,
    }
}

/// The bytes of the rig's ops file, if there is one.
pub(crate) fn ops_bytes(rig: &Rig) -> Option<Vec<u8>> {
    std::fs::read(rig.dir.path().join("desired.json.ops")).ok()
}
