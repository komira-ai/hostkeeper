//! `Releases.Force`, the coordinator's half: the targets a force names (a role, a list of hosts),
//! whom its order reaches and whom not after it was ordered, a second force replacing the first,
//! a force or its end that cannot be written, and the update slot the forced pairs do not take.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};
use serde_json::{Value, json};

use crate::desired::StoreError;
use crate::force::{ForceRecord, ForceTargets, end_by_ci};
use crate::marks_support::{
    DB, NEW_DAEMON, OLD_DAEMON, WEB, ask, ask_typed, beat_of, digest, fleet_target, force_request,
    hold, hosts_target, operator, ops_bytes, rig, role_target, runs, scope_host, set, start,
    unhold, update,
};
use crate::ops::{AuditEntry, OpsError, OpsState, OpsStore, ServiceOps, Write};
use crate::orders::Order;
use crate::registry::Digest;
use crate::rollback_tests::unwritable;
use crate::service::SetError;
use crate::testutil::{OPERATOR, Rig, ci_token, version};

/// The role of most hosts of these tests.
const DAEMON: &str = "daemon";

/// The second role, which both services run on.
const STORAGE: &str = "storage";

/// What a host reports of `service` running `running`, healthy, with the facts of `extra` (an
/// object) added: `updating`, `last_update`, `deferred`.
fn service_beat(service: &str, running: &str, extra: Value) -> ServiceBeat {
    let mut value = json!({"name": service, "version": running, "health": "healthy"});
    if let (Some(base), Value::Object(more)) = (value.as_object_mut(), extra) {
        base.extend(more);
    }
    serde_json::from_value(value).expect("a service beat")
}

/// A new daemon running `web` at `web` and `db` at `db`.
fn both(web: &str, db: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, web), runs(DB, db)])
}

/// A new daemon that runs `web` alone, at `web`.
fn web_only(web: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, web)])
}

/// The hosts `hosts` (name, role, heartbeat) are heard, before any desired state exists: each is
/// told nothing.
fn introduce(rig: &Rig, hosts: &[(&str, &str, Heartbeat)]) {
    for (host, role_name, b) in hosts {
        assert_eq!(
            ask(rig, host, role_name, b.clone()),
            ServiceReply::Nothing,
            "{host} is told nothing while no desired state is set"
        );
    }
}

/// CI sets each of `versions` of `web` in turn, with the digest `n` registered for it: the last
/// stays desired.
async fn registered(rig: &Rig, versions: &[(&str, u8)]) {
    for (ver, n) in versions {
        set(rig, WEB, ver, *n).await;
    }
}

/// A rig of the daemons `d1` and `d2`, both running `web` and `db` at 1.0, with 1.0 and 2.0 of
/// `web` registered and 2.0 desired.
async fn two_hosts(test: &str) -> Rig {
    let rig = rig(test, "");
    introduce(
        &rig,
        &[
            ("d1", DAEMON, both("1.0", "1.0")),
            ("d2", DAEMON, both("1.0", "1.0")),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    rig
}

/// `Releases.Force` as the operator.
fn force(
    rig: &Rig,
    request: proto::ForceRequest,
) -> Result<proto::ForceResponse, proto::ReleasesRefusal> {
    rig.coordinator.force(&operator(), request)
}

/// A force of `web` to `ver` at `targets` that the coordinator accepts.
fn force_at(rig: &Rig, ver: &str, targets: Targets, include_held: bool) -> proto::ForceResponse {
    force(rig, force_request(WEB, ver, targets, include_held)).expect("the force is accepted")
}

/// A fleet-wide force of `web` to `ver` that the coordinator accepts.
fn force_fleet(rig: &Rig, ver: &str, include_held: bool) -> proto::ForceResponse {
    force_at(rig, ver, fleet_target(WEB), include_held)
}

/// The operator's order the typed heartbeat `b` of `host` (role `daemon`) is answered with.
fn order_for(rig: &Rig, host: &str, b: Heartbeat) -> Option<Order> {
    ask_typed(rig, host, DAEMON, b).1
}

/// The order of the force number `generation` of `ver` of `web`, whose package digest is the
/// one `n` registered and whose halt generation is `halt_generation`.
fn forced(ver: &str, n: u8, halt_generation: u64, generation: u64) -> Order {
    Order::Force {
        service: WEB.to_owned(),
        version: version(ver),
        digest: Digest::parse(&digest(n)).expect("a digest"),
        index_digest: None,
        halt_generation,
        order_id: format!("force-{generation}"),
        order_try: 1,
    }
}

/// The marks of `web` now.
fn marks(rig: &Rig) -> ServiceOps {
    rig.coordinator.ops().marks(WEB)
}

/// The ops state as the status reports it.
fn status_ops(rig: &Rig) -> OpsState {
    rig.coordinator.status(Some(OPERATOR)).expect("status").ops
}

/// The (host, reason) pairs a force skipped.
fn skipped(response: &proto::ForceResponse) -> Vec<(&str, &str)> {
    response
        .skipped
        .iter()
        .map(|host| (host.node_id.as_str(), host.reason.as_str()))
        .collect()
}

/// The body that has CI set `ver` of `web` with the digest `n`.
fn set_body(ver: &str, n: u8) -> String {
    format!(
        r#"{{"service":"web","version":"{ver}","release_digest":"{}"}}"#,
        digest(n)
    )
}

/// Proves: a role target orders only the hosts of that role that run the service, and the record
/// of the force names the role.
///
/// Catches: a role force that orders hosts of another role.
#[tokio::test]
async fn a_role_force_orders_only_the_hosts_of_that_role() {
    let rig = rig("force-role", "");
    introduce(
        &rig,
        &[
            ("s1", STORAGE, both("1.0", "1.0")),
            ("d1", DAEMON, both("1.0", "1.0")),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;

    let response = force_at(&rig, "2.0", role_target(STORAGE), false);

    assert_eq!(response.targeted, 1);
    let record = marks(&rig).force.expect("a force");
    assert_eq!(record.targets, ForceTargets::Role(STORAGE.to_owned()));
    assert_eq!(record.hosts, ["s1"]);
    assert!(order_for(&rig, "d1", both("1.0", "1.0")).is_none());
    let (_, order) = ask_typed(&rig, "s1", STORAGE, both("1.0", "1.0"));
    assert_eq!(order, Some(forced("2.0", 2, 1, 1)));
}

/// Proves: a hosts target that names a host the coordinator has heard from that does not run the
/// service lists it skipped as `unknown_target`, orders the others, and sends the skipped host
/// nothing.
///
/// Catches: a hosts force refused whole for one host that runs another service, or one that orders
/// a host of the service it was not asked to.
#[tokio::test]
async fn a_hosts_force_lists_a_known_host_that_does_not_run_the_service_as_skipped() {
    let rig = rig("force-hosts-skip", "");
    introduce(
        &rig,
        &[
            ("d1", DAEMON, both("1.0", "1.0")),
            ("d9", DAEMON, beat_of(NEW_DAEMON, vec![runs(DB, "1.0")])),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;

    let response = force_at(&rig, "2.0", hosts_target(&["d1", "d9"]), false);

    assert_eq!(response.targeted, 1);
    assert_eq!(skipped(&response), [("d9", "unknown_target")]);
    let record = marks(&rig).force.expect("a force");
    assert_eq!(
        record.targets,
        ForceTargets::Hosts(vec!["d1".into(), "d9".into()])
    );
    assert_eq!(record.hosts, ["d1"]);
    let (_, order) = ask_typed(
        &rig,
        "d9",
        DAEMON,
        beat_of(NEW_DAEMON, vec![runs(DB, "1.0")]),
    );
    assert!(order.is_none());
}

/// Proves: the targets of a force are described as the audit says them: `fleet`, `role <name>`
/// and `hosts <ids>`; and a force that includes held pairs says so in its reason, with the host
/// ids sorted and unique and no slot holder passed.
///
/// Catches: an audit reason that names the wrong targets, a host list in the caller's order or
/// with repeats, or no mention of `include_held`.
#[tokio::test]
async fn force_targets_are_described_as_the_audit_says_them() {
    assert_eq!(ForceTargets::Fleet.describe(), "fleet");
    assert_eq!(
        ForceTargets::Role(STORAGE.to_owned()).describe(),
        "role storage"
    );
    assert_eq!(
        ForceTargets::Hosts(vec!["d1".to_owned(), "d2".to_owned()]).describe(),
        "hosts d1,d2"
    );

    let rig = two_hosts("force-describe").await;
    force_at(&rig, "2.0", hosts_target(&["d2", "d1", "d1"]), true);
    let recorded = status_ops(&rig)
        .audit
        .into_iter()
        .find(|entry| entry.action == "force")
        .expect("the force is audited");
    assert_eq!(
        recorded.reason.as_deref(),
        Some("fix the farm; version 2.0; targets hosts d1,d2; include_held; passed none")
    );
}

/// Proves: a version a targeted host leaves that is marked bad already is not marked again: the
/// force writes no `mark_bad` entry for it, and does not count it among the versions it left.
///
/// Catches: a second mark of a version already bad, or a version an operator marked taken for one
/// the force left.
#[tokio::test]
async fn a_version_left_that_is_bad_already_is_not_marked_again() {
    let rig = rig("force-left-bad", "");
    introduce(
        &rig,
        &[
            ("d1", DAEMON, web_only("3.0")),
            ("d2", DAEMON, web_only("1.0")),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    rig.coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: WEB.to_owned(),
                version: "3.0".to_owned(),
                reason: "crash loop".to_owned(),
            },
        )
        .expect("marked bad");

    let response = force_fleet(&rig, "2.0", false);

    let entries: Vec<&str> = response.entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(entries, ["force"]);
    assert_eq!(response.targeted, 2);
    assert_eq!(marks(&rig).bad, [version("3.0")]);
    assert!(marks(&rig).bad_by_force.is_empty());
}

/// Proves: a second force replaces the first: the record is the second's, its generation is 2 and
/// so is the order id, and a host is ordered the second version only.
///
/// Catches: two forces standing at once, or a generation that does not count on.
#[tokio::test]
async fn a_second_force_replaces_the_first() {
    let rig = rig("force-replace", "");
    introduce(&rig, &[("d1", DAEMON, web_only("1.0"))]);
    registered(&rig, &[("1.0", 1), ("1.5", 3), ("2.0", 2)]).await;
    force_fleet(&rig, "2.0", false);
    force_fleet(&rig, "1.5", false);

    let record = marks(&rig).force.expect("a force");
    assert_eq!(record.version, version("1.5"));
    assert_eq!(record.generation, 2);
    assert_eq!(marks(&rig).force_generation, 2);
    assert_eq!(
        order_for(&rig, "d1", web_only("1.0")),
        Some(forced("1.5", 3, 2, 2))
    );
}

/// Proves: a host the force does not target is sent no force order, and one it targets is.
///
/// Catches: a force order sent to every host that runs the service.
#[tokio::test]
async fn a_host_the_force_does_not_target_gets_no_force() {
    let rig = two_hosts("force-untargeted").await;

    force_at(&rig, "2.0", hosts_target(&["d1"]), false);

    assert!(order_for(&rig, "d2", both("1.0", "1.0")).is_none());
    assert_eq!(
        order_for(&rig, "d1", both("1.0", "1.0")),
        Some(forced("2.0", 2, 1, 1))
    );
}

/// Proves: a targeted host that already runs the forced version, or reports that it is updating to
/// it, is sent no force order; a targeted host behind is.
///
/// Catches: a force order that restarts a host that is already at the version or on its way.
#[tokio::test]
async fn a_target_that_runs_or_is_updating_to_the_version_gets_no_force() {
    let rig = two_hosts("force-done").await;
    ask(&rig, "d1", DAEMON, both("2.0", "1.0"));

    let response = force_fleet(&rig, "2.0", false);

    assert_eq!(response.targeted, 2, "both are targeted");
    assert!(order_for(&rig, "d1", both("2.0", "1.0")).is_none());
    let updating = beat_of(
        NEW_DAEMON,
        vec![
            service_beat(WEB, "1.0", json!({"updating": "2.0"})),
            runs(DB, "1.0"),
        ],
    );
    assert!(
        order_for(&rig, "d2", updating).is_none(),
        "it is running the force"
    );
    assert_eq!(
        order_for(&rig, "d2", both("1.0", "1.0")),
        Some(forced("2.0", 2, 1, 1)),
        "and is ordered again if it reports it is not"
    );
}

/// Proves: a pair an operator holds after the force was ordered is sent no force order while the
/// hold stands, and is sent it again when the hold is released.
///
/// Catches: a force order that reaches a pair held since, or one that is lost for good by a hold.
#[tokio::test]
async fn a_target_held_after_the_force_gets_no_force_until_released() {
    let rig = two_hosts("force-held-later").await;
    force_fleet(&rig, "2.0", false);

    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert!(order_for(&rig, "d1", both("1.0", "1.0")).is_none());
    unhold(&rig, WEB, scope_host("d1")).expect("released");
    assert_eq!(
        order_for(&rig, "d1", both("1.0", "1.0")),
        Some(forced("2.0", 2, 1, 1))
    );
}

/// Proves: a host whose daemon announced `forced-v1` when the force was made but no longer does
/// in its heartbeat is sent no force order.
///
/// Catches: a force order for a daemon that does not announce the feature now.
#[tokio::test]
async fn a_host_whose_daemon_no_longer_announces_force_gets_none() {
    let rig = two_hosts("force-feature-lost").await;
    force_fleet(&rig, "2.0", false);

    let old = beat_of(OLD_DAEMON, vec![runs(WEB, "1.0"), runs(DB, "1.0")]);
    assert!(order_for(&rig, "d1", old).is_none());
    assert_eq!(
        order_for(&rig, "d1", both("1.0", "1.0")),
        Some(forced("2.0", 2, 1, 1))
    );
}

/// Proves: a force whose ops file cannot be written is refused as `store_failed`, no force
/// stands, no host is sent an order and nothing reaches the ops file.
///
/// Catches: a force that is answered as accepted, or that orders hosts, when its record was not
/// written.
#[tokio::test]
async fn a_force_that_cannot_be_written_is_refused_as_store_failed() {
    let mut rig = two_hosts("force-store-failed").await;
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(OpsState::default()));
    rig.at(2_100);
    ask(&rig, "d1", DAEMON, both("1.0", "1.0"));

    let refused = force(&rig, force_request(WEB, "1.0", fleet_target(WEB), false));

    assert_eq!(
        refused.expect_err("refused"),
        proto::ReleasesRefusal::StoreFailed
    );
    assert!(marks(&rig).force.is_none());
    assert!(order_for(&rig, "d1", both("1.0", "1.0")).is_none());
    assert_eq!(ops_bytes(&rig), None);
}

/// Proves: when the ops file cannot be written, CI's set of another version answers
/// `SetError::Ops` and the force it could not end still stands.
///
/// Catches: a force that ends in memory only, or a set that reports success when the end of the
/// force was not written.
#[tokio::test]
async fn a_ci_set_that_cannot_end_the_force_answers_an_ops_error() {
    let mut rig = two_hosts("force-end-failed").await;
    registered(&rig, &[("3.0", 3)]).await;
    let failing = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&failing);
    let ops = OpsStore::refusing(
        OpsState::default(),
        Box::new(move |_| {
            if switch.load(Ordering::SeqCst) {
                Err(OpsError::Io(StoreError::Io {
                    path: "ops".into(),
                    source: std::io::Error::other("disk full"),
                }))
            } else {
                Ok(())
            }
        }),
    );
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(ops);
    rig.at(2_100);
    ask(&rig, "d1", DAEMON, both("1.0", "1.0"));
    force_fleet(&rig, "2.0", false);
    assert!(marks(&rig).force.is_some());

    failing.store(true, Ordering::SeqCst);
    let body = set_body("3.0", 3);
    let err = rig
        .coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
        .expect_err("the end of the force is not written");

    assert!(matches!(err, SetError::Ops(_)), "{err:?}");
    assert!(marks(&rig).force.is_some(), "the force stands");
}

/// Proves: CI's set ends a force only when it names another version than the one forced; a set of
/// the forced version leaves it standing, and the end is audited as CI's `force_ended` with the
/// workflow and both versions in its reason.
///
/// Catches: a force that CI's set of the same version ends, or an end that is not audited.
#[test]
fn end_by_ci_ends_a_force_only_for_another_version() {
    let ops = OpsStore::volatile();
    assert!(!end_by_ci(&ops, WEB, &version("3.0"), "release").expect("no force"));
    ops.change(
        Write::BeforeMemory,
        AuditEntry::new("operator", "force"),
        |state| {
            state.services.entry(WEB.to_owned()).or_default().force = Some(ForceRecord {
                version: version("2.0"),
                generation: 1,
                reason: "fix the farm".to_owned(),
                by: "operator".to_owned(),
                digest: Digest::parse(&digest(2)).expect("a digest"),
                release_index: None,
                halt_generation: 1,
                targets: ForceTargets::Fleet,
                include_held: false,
                hosts: vec!["d1".to_owned()],
            });
        },
    )
    .expect("seeded");

    assert!(!end_by_ci(&ops, WEB, &version("2.0"), "release").expect("same version"));
    assert!(ops.marks(WEB).force.is_some());
    assert!(end_by_ci(&ops, WEB, &version("3.0"), "release").expect("ended"));
    assert!(ops.marks(WEB).force.is_none());
    let last = ops.state().audit.pop().expect("an entry");
    assert_eq!(
        (last.by.as_str(), last.action.as_str()),
        ("ci", "force_ended")
    );
    assert_eq!(
        last.reason.as_deref(),
        Some("3.0 set by release; the force to 2.0 ends")
    );
}

/// Proves: a forced pair takes no update slot: while a force of `web` stands and the forced `d1`
/// is updating, the gate still orders another host (`d2`), and then holds the third (`d3`)
/// behind `d2`'s slot as usual.
///
/// Catches: a forced update that blocks the rollout of the rest of the fleet, or a force that
/// frees every slot.
#[tokio::test]
async fn a_forced_host_that_is_updating_takes_no_update_slot() {
    let rig = rig("force-no-slot", "");
    introduce(
        &rig,
        &[
            ("d1", DAEMON, web_only("1.0")),
            ("d2", DAEMON, web_only("1.0")),
            ("d3", DAEMON, web_only("1.0")),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    force_at(&rig, "2.0", hosts_target(&["d1"]), false);

    let updating = beat_of(
        NEW_DAEMON,
        vec![service_beat(WEB, "1.0", json!({"updating": "2.0"}))],
    );
    let (_, order) = ask_typed(&rig, "d1", DAEMON, updating);
    assert!(order.is_none(), "d1 is running the force");

    assert_eq!(ask(&rig, "d2", DAEMON, web_only("1.0")), update(WEB, "2.0"));
    assert_eq!(
        ask(&rig, "d3", DAEMON, web_only("1.0")),
        ServiceReply::Nothing,
        "d2 holds the one slot now"
    );
}
