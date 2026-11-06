//! `Releases.Force`, the coordinator's half: what a force refuses before anything changes, whom it
//! orders and whom it skips, what it writes to the ops file and the audit, how the order reaches a
//! host over the typed heartbeat (past the gate, the canary, a deferral, a bad mark, a halt and a
//! host's earlier failure), what a standing force suspends (the automatic rollback, the update
//! slot of the pairs it orders) and how it ends. Every time is the rig's clock.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};
use serde_json::{Value, json};

use crate::force::SELF_SERVICE;
use crate::holds::HoldScope;
use crate::marks_support::{
    DB, NEW_DAEMON, OLD_DAEMON, WEB, ask, ask_typed, beat_of, digest, fleet_target, force_request,
    hold, hosts_target, operator, ops_bytes, rig, role_target, runs, scope_host, scope_role, set,
    update, who,
};
use crate::operator::Caller;
use crate::ops::{OpsState, ServiceOps};
use crate::orders::Order;
use crate::registry::Digest;
use crate::service::SetError;
use crate::testutil::{OPERATOR, Rig, ci_token, ops_of, version};

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

/// A new daemon running `web` at 1.0 whose last update, to 2.0, ended for `how` (a JSON object
/// of the update result's fields besides the target).
fn web_ended(target: &str, how: Value) -> Heartbeat {
    let mut last = json!({"target": target});
    if let (Some(base), Value::Object(more)) = (last.as_object_mut(), how) {
        base.extend(more);
    }
    beat_of(
        NEW_DAEMON,
        vec![service_beat(WEB, "1.0", json!({"last_update": last}))],
    )
}

/// A new daemon running `web` at 1.0 that failed the release of `target` after the commit, as the
/// order `order_id` (if any) told it to.
fn release_failure(target: &str, order_id: Option<&str>) -> Heartbeat {
    let mut how = json!({"outcome": "failed", "phase": "after_commit", "class": "release"});
    if let (Some(order), Some(map)) = (order_id, how.as_object_mut()) {
        map.insert("order_id".to_owned(), json!(order));
    }
    web_ended(target, how)
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

/// The version of `web` that is desired now.
fn desired_version(rig: &Rig) -> Option<Version> {
    rig.coordinator
        .desired
        .get(WEB)
        .map(|desired| desired.version)
}

/// The actions of the audit ring, oldest first.
fn actions(rig: &Rig) -> Vec<String> {
    rig.coordinator
        .ops()
        .state()
        .audit
        .into_iter()
        .map(|entry| entry.action)
        .collect()
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

/// The heartbeat of a host that failed the update of `web` to 2.0 (and runs `db`).
fn failed_here_beat() -> Heartbeat {
    beat_of(
        NEW_DAEMON,
        vec![
            service_beat(
                WEB,
                "1.0",
                json!({"last_update": {"target": "2.0", "outcome": "failed"}}),
            ),
            runs(DB, "1.0"),
        ],
    )
}

/// The heartbeat of the host that declined the version for an hour.
fn deferring_beat() -> Heartbeat {
    let declined = json!({"deferred": {
        "target": "2.0", "reason": "drain_bound", "retry_after_secs": 3600
    }});
    beat_of(
        NEW_DAEMON,
        vec![service_beat(WEB, "1.0", declined), runs(DB, "1.0")],
    )
}

/// The heartbeat of the host that holds the one live update slot, updating `db`.
fn db_updating_beat() -> Heartbeat {
    beat_of(
        NEW_DAEMON,
        vec![service_beat(DB, "1.0", json!({"updating": "2.0"}))],
    )
}

/// Proves: a fleet-wide force skips a pair an operator holds and lists it as `held`, and with
/// `include_held` orders it for that call only: the hold stands afterwards, in `Services.List`
/// and in the status.
///
/// Catches: a force that orders a held pair without being asked, one that leaves the pair out
/// with `include_held`, or one that drops the hold it passed.
#[tokio::test]
async fn force_skips_a_held_pair_unless_include_held_and_then_removes_no_hold() {
    let rig = two_hosts("force-held").await;
    hold(&rig, WEB, scope_host("d1")).expect("held");

    let first = force_fleet(&rig, "2.0", false);
    assert_eq!(first.held, ["d1"]);
    assert_eq!(first.targeted, 1);
    let (reply, order) = ask_typed(&rig, "d1", DAEMON, both("1.0", "1.0"));
    assert_eq!(
        reply,
        ServiceReply::Nothing,
        "the held pair is told nothing"
    );
    assert!(order.is_none(), "the held pair is ordered no force");
    assert_eq!(
        order_for(&rig, "d2", both("1.0", "1.0")),
        Some(forced("2.0", 2, 1, 1))
    );

    let second = force_fleet(&rig, "2.0", true);
    assert!(second.held.is_empty(), "include_held skips nobody");
    assert_eq!(second.targeted, 2);
    assert_eq!(
        order_for(&rig, "d1", both("1.0", "1.0")),
        Some(forced("2.0", 2, 2, 2)),
        "the held pair is ordered by the second force"
    );

    let listed = rig
        .coordinator
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed");
    assert_eq!(listed.holds.len(), 1, "the hold stands");
    assert_eq!(listed.holds[0].service, WEB);
    assert_eq!(listed.holds[0].scope, scope_host("d1"));
    let holds = status_ops(&rig).holds;
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].scope, HoldScope::Host("d1".to_owned()));
}

/// Proves: a force with `include_held` orders the hosts of a role an operator holds and leaves
/// the role hold in the ops file; the next force without it skips them again and sends them
/// nothing.
///
/// Catches: a force that removes a role hold to order its hosts, or an order that outlives the
/// hold's return for a force that did not include held pairs.
#[tokio::test]
async fn a_force_with_include_held_leaves_a_role_hold_in_place() {
    let rig = rig("force-role-hold", "");
    introduce(
        &rig,
        &[
            ("s1", STORAGE, both("1.0", "1.0")),
            ("d1", DAEMON, both("1.0", "1.0")),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    hold(&rig, WEB, scope_role(STORAGE)).expect("held");

    let included = force_fleet(&rig, "2.0", true);
    assert_eq!(included.targeted, 2);
    assert!(included.held.is_empty());
    let (_, order) = ask_typed(&rig, "s1", STORAGE, both("1.0", "1.0"));
    assert_eq!(order, Some(forced("2.0", 2, 1, 1)));
    let holds = status_ops(&rig).holds;
    assert_eq!(holds.len(), 1, "no hold is removed");
    assert_eq!(holds[0].scope, HoldScope::Role(STORAGE.to_owned()));

    let plain = force_fleet(&rig, "2.0", false);
    assert_eq!(plain.held, ["s1"]);
    assert_eq!(plain.targeted, 1);
    let (_, order) = ask_typed(&rig, "s1", STORAGE, both("1.0", "1.0"));
    assert!(order.is_none(), "the role hold stops the second force");
    assert_eq!(
        order_for(&rig, "d1", both("1.0", "1.0")),
        Some(forced("2.0", 2, 2, 2))
    );
}

/// Proves: one force of a service reaches every target in one heartbeat round, whatever holds the
/// gate closed for each: a host that holds the one live update slot elsewhere (`d4` updating
/// `db`), a closed canary, a bake, a deferral (`d2`), a halt, a version marked bad and a host that
/// failed the version (`d1`). One `Releases.Force`, then one typed heartbeat of each of `d1`,
/// `d2` and `d3` answers each the force order, and the gate alone still tells them nothing.
///
/// Catches: a force that waits for the slot, the canary or a deferral, that leaves a failed host
/// out, or that is refused over a bad mark or a halt.
#[tokio::test]
async fn force_reaches_every_target_in_one_heartbeat_round_and_passes_the_slot() {
    let rig = rig("force-round", "canary = { count = 1 }\nbake_beats = 3");
    let hosts = [
        ("d1", DAEMON, failed_here_beat()),
        ("d2", DAEMON, deferring_beat()),
        ("d3", DAEMON, both("1.0", "1.0")),
        ("d4", DAEMON, db_updating_beat()),
    ];
    introduce(&rig, &hosts);
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    rig.coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                reason: "crash loop".to_owned(),
            },
        )
        .expect("marked bad");
    rig.coordinator
        .halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "investigating".to_owned(),
            },
        )
        .expect("halted");
    for (host, role_name, b) in &hosts[..3] {
        assert_eq!(
            ask(&rig, host, role_name, b.clone()),
            ServiceReply::Nothing,
            "{host} is told nothing before the force"
        );
    }

    let response = force_fleet(&rig, "2.0", false);
    assert_eq!(response.targeted, 3);
    assert!(response.held.is_empty());
    assert!(response.skipped.is_empty());
    for (host, role_name, b) in &hosts[..3] {
        let (_, order) = ask_typed(&rig, host, role_name, b.clone());
        assert_eq!(order, Some(forced("2.0", 2, 1, 1)), "{host}");
    }
    assert!(marks(&rig).halt.is_none(), "the force cleared the halt");
    assert!(!marks(&rig).bad.contains(&version("2.0")));
    for (host, role_name, b) in &hosts[..3] {
        assert_eq!(
            ask(&rig, host, role_name, b.clone()),
            ServiceReply::Nothing,
            "{host}: the gate stays closed, only the force reaches it"
        );
    }
}

/// Proves: the coordinator refuses to force the service `hostkeeper` (`self_service`) and a
/// version CI never registered a package digest for (`no_digest`), and each refusal leaves the
/// ops file byte for byte and the desired state as they were.
///
/// Catches: a force of the updater's own service, a force with no digest to put in the order, or a
/// refusal that writes before it refuses.
#[tokio::test]
async fn coordinator_refuses_force_for_hostkeeper_and_without_a_registered_digest() {
    let rig = two_hosts("force-refuses").await;
    hold(&rig, WEB, scope_role(STORAGE)).expect("held");
    let before = ops_bytes(&rig);
    assert!(before.is_some(), "the hold made the ops file");
    let wanted = desired_version(&rig);

    let own = force(
        &rig,
        force_request(SELF_SERVICE, "2.0", fleet_target(SELF_SERVICE), false),
    );
    assert_eq!(
        own.expect_err("refused"),
        proto::ReleasesRefusal::SelfService
    );
    let unregistered = force(&rig, force_request(WEB, "9.9", fleet_target(WEB), false));
    assert_eq!(
        unregistered.expect_err("refused"),
        proto::ReleasesRefusal::NoDigest
    );
    assert_eq!(ops_bytes(&rig), before);
    assert_eq!(desired_version(&rig), wanted);
}

/// Proves: with `min_version = "1.5"`, a force to 1.4 is refused as `below_min_version` and one to
/// 1.5 is accepted; CI's set of 1.4 is refused too, and the refusals change nothing, so 1.4 is
/// never ordered to any host.
///
/// Catches: a floor that a force passes, a floor that refuses the floor version itself, or a
/// refusal that leaves the desired state or the ops file changed.
#[tokio::test]
async fn an_order_below_min_version_is_never_issued() {
    let rig = rig("force-floor", "min_version = \"1.5\"");
    introduce(
        &rig,
        &[
            ("d1", DAEMON, web_only("1.6")),
            ("d2", DAEMON, web_only("1.5")),
        ],
    );
    registered(&rig, &[("1.5", 1), ("1.6", 2)]).await;
    hold(&rig, WEB, scope_role(STORAGE)).expect("held");
    let before = ops_bytes(&rig);

    let low = force(&rig, force_request(WEB, "1.4", fleet_target(WEB), false));
    assert_eq!(
        low.expect_err("refused"),
        proto::ReleasesRefusal::BelowMinVersion
    );
    let body = set_body("1.4", 3);
    let err = rig
        .coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
        .expect_err("CI's set is refused");
    assert!(matches!(err, SetError::BelowMinVersion), "{err:?}");
    assert_eq!(ops_bytes(&rig), before);
    assert_eq!(desired_version(&rig), Some(version("1.6")));
    assert_eq!(
        ask(&rig, "d2", DAEMON, web_only("1.5")),
        update(WEB, "1.6"),
        "the gate orders the desired version, never 1.4"
    );

    let floor = force_fleet(&rig, "1.5", false);
    assert_eq!(floor.targeted, 2);
    assert_eq!(desired_version(&rig), Some(version("1.5")));
    assert_eq!(
        order_for(&rig, "d1", web_only("1.6")),
        Some(forced("1.5", 1, 1, 1)),
        "the floor itself is ordered, and no host is ordered 1.4"
    );
}

/// Proves: a fleet-wide force whose `confirm_service` is not the service is refused as
/// `confirm_mismatch`, and so is each other bad request with its own refusal (`unknown_target`
/// for a role the service does not run on and for a host never heard from, `invalid_request` for
/// no targets, an empty host list, a bad host id, a bad role name, a bad version and a bad
/// reason, `unknown_service`); none of them touches the ops file.
///
/// Catches: a fleet force that needs no confirmation, a request that is accepted with a target no
/// host can match, or a refusal that writes before it refuses.
#[tokio::test]
async fn a_fleet_force_without_the_service_name_confirmed_is_refused() {
    let rig = two_hosts("force-invalid").await;
    hold(&rig, WEB, scope_role(STORAGE)).expect("held");
    let before = ops_bytes(&rig);
    let wanted = desired_version(&rig);

    let request = |targets: Targets| force_request(WEB, "2.0", targets, false);
    let mut no_targets = request(fleet_target(WEB));
    no_targets.targets = None;
    let mut bad_version = request(fleet_target(WEB));
    bad_version.version = "x2".to_owned();
    let mut bad_reason = request(fleet_target(WEB));
    bad_reason.reason = "   ".to_owned();
    let refusals = proto::ReleasesRefusal::InvalidRequest;
    let cases = [
        (
            "a fleet confirmed with another service",
            request(fleet_target(DB)),
            proto::ReleasesRefusal::ConfirmMismatch,
        ),
        (
            "a role the service does not run on",
            request(role_target("frontend")),
            proto::ReleasesRefusal::UnknownTarget,
        ),
        (
            "a host never heard from",
            request(hosts_target(&["ghost"])),
            proto::ReleasesRefusal::UnknownTarget,
        ),
        ("no targets", no_targets, refusals),
        ("an empty host list", request(hosts_target(&[])), refusals),
        (
            "a bad host id",
            request(hosts_target(&["Bad Host"])),
            refusals,
        ),
        (
            "a bad role name",
            request(role_target("Bad_Role")),
            refusals,
        ),
        ("a bad version", bad_version, refusals),
        ("a bad reason", bad_reason, refusals),
        (
            "an unknown service",
            force_request("nosuch", "2.0", fleet_target("nosuch"), false),
            proto::ReleasesRefusal::UnknownService,
        ),
    ];
    for (what, request, want) in cases {
        assert_eq!(force(&rig, request).expect_err(what), want, "{what}");
        assert_eq!(ops_bytes(&rig), before, "{what}");
        assert_eq!(desired_version(&rig), wanted, "{what}");
    }
}

/// Proves: only the operator token may force: a CI token is `forbidden` and no token is
/// `unauthorized`, and neither changes the ops file.
///
/// Catches: a force open to CI's token or to an anonymous caller.
#[tokio::test]
async fn only_the_operator_token_may_force() {
    let rig = two_hosts("force-callers").await;
    hold(&rig, WEB, scope_role(STORAGE)).expect("held");
    let before = ops_bytes(&rig);
    let request = force_request(WEB, "2.0", fleet_target(WEB), false);

    let ci = rig.coordinator.caller(Some(&ci_token())).await;
    assert_eq!(
        rig.coordinator
            .force(&ci, request.clone())
            .expect_err("refused"),
        proto::ReleasesRefusal::Forbidden
    );
    let nobody = rig.coordinator.caller(None).await;
    assert_eq!(
        rig.coordinator
            .force(&nobody, request.clone())
            .expect_err("refused"),
        proto::ReleasesRefusal::Unauthorized
    );
    assert_eq!(
        rig.coordinator
            .force(&Caller::Anonymous, request)
            .expect_err("refused"),
        proto::ReleasesRefusal::Unauthorized
    );
    assert_eq!(ops_bytes(&rig), before);
}

/// Proves: a force over a standing halt (one an operator cleared once, so the generation is 1)
/// orders a halt generation greater than the one held before; the generation is persisted, the
/// JSON heartbeat answer tells it to an agent that announced `halt-v1`, and the halt is cleared
/// by an entry that follows the `force` entry.
///
/// Catches: a force whose order carries a generation a halted host does not lift under, one that
/// does not persist it, or one that leaves the halt standing.
#[tokio::test]
async fn coordinator_force_over_a_standing_halt_sends_a_greater_generation() {
    let rig = rig("force-halt", "");
    introduce(&rig, &[("d1", DAEMON, web_only("1.0"))]);
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    let halt = || {
        rig.coordinator.halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "disk tainted".to_owned(),
            },
        )
    };
    halt().expect("halted");
    rig.coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"clear_halt","service":"web","reason":"disk replaced"}"#,
        )
        .expect("cleared");
    halt().expect("halted again");
    assert_eq!(marks(&rig).halt_generation, 1);
    assert!(marks(&rig).halt.is_some());

    force_fleet(&rig, "2.0", false);

    let (_, order) = ask_typed(&rig, "d1", DAEMON, web_only("1.0"));
    assert_eq!(order, Some(forced("2.0", 2, 2, 1)));
    let persisted = ops_of(&rig.dir).state();
    assert_eq!(persisted.services[WEB].halt_generation, 2);
    let answer = rig
        .coordinator
        .heartbeat(who("d1", DAEMON), web_only("1.0"))
        .expect("answered");
    assert_eq!(answer.halts, BTreeMap::from([(WEB.to_owned(), 2)]));
    assert!(marks(&rig).halt.is_none(), "the halt is cleared");
    let seen = actions(&rig);
    assert!(
        seen.ends_with(&[
            "halt".to_owned(),
            "force".to_owned(),
            "clear_halt".to_owned()
        ]),
        "{seen:?}"
    );
}

/// Proves: a force lifts the bad mark of its target and marks bad each newer version a targeted
/// host leaves, in the order the hosts are planned (by node id), once each and never an older one;
/// the entries are `force`, `unmark_bad`, `mark_bad` (3.0), `mark_bad` (2.5); `bad` and
/// `bad_by_force` hold the versions left and not the target; CI cannot set a version left bad.
///
/// Catches: a target that stays marked bad, a version left unmarked or marked twice, an older
/// version marked bad, or a left version that CI can set again.
#[tokio::test]
async fn force_lifts_the_target_bad_mark_and_marks_each_version_left_bad() {
    let rig = rig("force-marks", "");
    introduce(
        &rig,
        &[
            ("d1", DAEMON, web_only("3.0")),
            ("d2", DAEMON, web_only("2.5")),
            ("d3", DAEMON, web_only("3.0")),
            ("d4", DAEMON, web_only("1.0")),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    rig.coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                reason: "crash loop".to_owned(),
            },
        )
        .expect("marked bad");

    let response = force_fleet(&rig, "2.0", false);

    let entries: Vec<&str> = response.entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(entries, ["force", "unmark_bad", "mark_bad", "mark_bad"]);
    assert_eq!(
        response.entries[2].reason.as_deref(),
        Some("3.0 is left by the force to 2.0")
    );
    assert_eq!(
        response.entries[3].reason.as_deref(),
        Some("2.5 is left by the force to 2.0")
    );
    let left = [version("3.0"), version("2.5")];
    assert_eq!(marks(&rig).bad, left);
    assert_eq!(marks(&rig).bad_by_force, left);
    let body = set_body("3.0", 5);
    let err = rig
        .coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
        .expect_err("refused");
    assert!(matches!(err, SetError::BadVersion), "{err:?}");
}

/// Proves: while a force stands, the automatic rollback of its service is suspended: release
/// failures of the forced version on two hosts roll nothing back and mark nothing bad; CI's set of
/// a newer version ends the force (`force_ended`, by `ci`), and release failures of that version
/// then roll back to the last good one.
///
/// Catches: a rollback that undoes the operator's force, a force that never ends, or a rollback
/// that stays suspended after CI sets another version.
#[tokio::test]
async fn force_suspends_automatic_rollback_until_a_newer_desired_version() {
    let rig = rig("force-rollback", "rollback = \"automatic\"");
    let hosts = ["d1", "d2", "d3"];
    for host in hosts {
        introduce(&rig, &[(host, DAEMON, web_only("1.0"))]);
    }
    registered(&rig, &[("1.0", 1)]).await;
    for host in hosts {
        ask(&rig, host, DAEMON, web_only("1.0"));
    }
    assert_eq!(marks(&rig).last_good, Some(version("1.0")));
    registered(&rig, &[("2.0", 2)]).await;
    force_fleet(&rig, "2.0", false);

    for host in ["d1", "d2"] {
        ask(&rig, host, DAEMON, release_failure("2.0", Some("force-1")));
    }
    assert_eq!(desired_version(&rig), Some(version("2.0")));
    assert!(marks(&rig).bad.is_empty(), "2.0 is not marked bad");
    assert!(marks(&rig).force.is_some(), "the force stands");

    registered(&rig, &[("3.0", 3)]).await;
    assert!(marks(&rig).force.is_none(), "CI's newer version ends it");
    let ended = rig
        .coordinator
        .ops()
        .state()
        .audit
        .into_iter()
        .find(|entry| entry.action == "force_ended")
        .expect("the end is audited");
    assert_eq!(ended.by, "ci");
    for host in ["d1", "d2"] {
        ask(&rig, host, DAEMON, release_failure("3.0", None));
    }
    assert_eq!(desired_version(&rig), Some(version("1.0")));
    assert_eq!(marks(&rig).bad, [version("3.0")]);
}

/// Proves: the order a force sends carries the package digest CI registered for the version and
/// nothing else besides the service, the version, the order id, the first try and the halt
/// generation; `Order::to_proto` equals a full `hostkeeper.agent.v1` order, every field of its
/// `Force` named, so a field added to the message fails to compile here until the order says it.
///
/// Catches: an order that names no digest or another one, a command field smuggled into the order,
/// or an order id or try that drifts.
#[tokio::test]
async fn force_order_carries_the_registered_digest_and_no_command_field() {
    let rig = rig("force-proto", "");
    introduce(&rig, &[("d1", DAEMON, web_only("1.0"))]);
    registered(&rig, &[("1.0", 1), ("2.0", 7)]).await;
    force_fleet(&rig, "2.0", false);

    let order = order_for(&rig, "d1", web_only("1.0")).expect("a force order");

    assert_eq!(
        order.to_proto(),
        agent::Order {
            order_id: "force-1".to_owned(),
            r#try: 1,
            verb: Some(agent::order::Verb::Force(agent::Force {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                package_digest: digest(7),
                halt_generation: 1,
                index_digest: String::new(),
            })),
        }
    );
}

/// Proves: a target whose daemon did not announce `forced-v1` is listed skipped as
/// `daemon_too_old`, is not counted in `targeted`, and its typed heartbeats carry no force order.
///
/// Catches: a force order sent to a daemon that would refuse the unknown verb, or a skipped host
/// counted as targeted.
#[tokio::test]
async fn a_daemon_that_did_not_announce_force_gets_no_force_order() {
    let rig = rig("force-old", "");
    let old = || beat_of(OLD_DAEMON, vec![runs(WEB, "1.0")]);
    introduce(
        &rig,
        &[("d1", DAEMON, web_only("1.0")), ("d2", DAEMON, old())],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;

    let response = force_fleet(&rig, "2.0", false);

    assert_eq!(response.targeted, 1);
    assert_eq!(skipped(&response), [("d2", "daemon_too_old")]);
    assert_eq!(marks(&rig).force.expect("a force").hosts, ["d1"]);
    for _ in 0..2 {
        assert!(order_for(&rig, "d2", old()).is_none());
    }
    assert_eq!(
        order_for(&rig, "d1", web_only("1.0")),
        Some(forced("2.0", 2, 1, 1))
    );
}

/// Proves: a host that reported the force's own order failed (`last_update` failed, order id
/// `force-1`) is not ordered again by the force and the gate tells it nothing; a second force to
/// another registered version orders it again as `force-2`.
///
/// Catches: a force order resent to a host that failed it forever, a gate that retries the failed
/// version, or a second force that cannot reach the host again.
#[tokio::test]
async fn a_host_that_reported_forced_failed_is_not_ordered_again_by_the_gate() {
    let rig = rig("force-failed", "");
    introduce(&rig, &[("d1", DAEMON, web_only("1.0"))]);
    registered(&rig, &[("1.0", 1), ("1.5", 3), ("2.0", 2)]).await;
    force_fleet(&rig, "2.0", false);
    assert_eq!(
        order_for(&rig, "d1", web_only("1.0")),
        Some(forced("2.0", 2, 1, 1))
    );

    let failed = || web_ended("2.0", json!({"outcome": "failed", "order_id": "force-1"}));
    let (reply, order) = ask_typed(&rig, "d1", DAEMON, failed());
    assert!(order.is_none(), "the failed force is not sent again");
    assert_eq!(reply, ServiceReply::Nothing, "and the gate does not retry");

    force_fleet(&rig, "1.5", false);
    assert_eq!(
        order_for(&rig, "d1", failed()),
        Some(forced("1.5", 3, 2, 2))
    );
}

/// Proves: the `force` entry names the operator's token as its actor, the targets and the hosts
/// holding an update slot the force passed (`d4:db`), and not a forced target that held a slot
/// itself (`d2` updating `web`).
///
/// Catches: an audit that names no one, a force entry that omits what it passed, or one that
/// lists a forced target as a holder passed.
#[tokio::test]
async fn force_audit_entry_names_token_targets_and_slot_holders_passed() {
    let rig = rig("force-audit", "");
    introduce(
        &rig,
        &[
            ("d1", DAEMON, web_only("1.0")),
            (
                "d2",
                DAEMON,
                beat_of(
                    NEW_DAEMON,
                    vec![service_beat(WEB, "1.0", json!({"updating": "2.0"}))],
                ),
            ),
            ("d4", DAEMON, db_updating_beat()),
        ],
    );
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;

    let response = force_fleet(&rig, "2.0", false);

    assert_eq!(response.targeted, 2);
    assert_eq!(response.entries[0].action, "force");
    assert_eq!(response.entries[0].by, i32::from(proto::Actor::Operator));
    let recorded = status_ops(&rig)
        .audit
        .into_iter()
        .find(|entry| entry.action == "force")
        .expect("the force is audited");
    assert_eq!(recorded.by, "operator");
    assert_eq!(recorded.service.as_deref(), Some(WEB));
    assert_eq!(
        recorded.reason.as_deref(),
        Some("fix the farm; version 2.0; targets fleet; passed d4:db")
    );
}

/// Catches: `forced-v1` left out of what the coordinator advertises while it sends the force
/// orders a daemon runs only when it is advertised. A forced daemon's typed heartbeat is answered
/// with the force and with `forced-v1` among the features, in the coordinator's order; the same
/// heartbeat without `forced-v1` is answered with no force and is not told it.
pub(crate) async fn forced_v1_is_acted_on() {
    let rig = rig("force-feature", "");
    introduce(&rig, &[("d1", DAEMON, web_only("1.0"))]);
    registered(&rig, &[("1.0", 1), ("2.0", 2)]).await;
    force_fleet(&rig, "2.0", false);

    let (answer, order) = rig
        .coordinator
        .heartbeat_with_orders(who("d1", DAEMON), web_only("1.0"))
        .expect("answered");
    assert_eq!(order, Some(forced("2.0", 2, 1, 1)));
    assert_eq!(
        answer.features.as_deref(),
        Some(&["deferred-v1", "halt-v1", "heartbeat-v2", "forced-v1"].map(String::from)[..])
    );

    let unannounced: Vec<&str> = NEW_DAEMON
        .iter()
        .copied()
        .filter(|name| *name != crate::force::FEATURE)
        .collect();
    let (answer, order) = rig
        .coordinator
        .heartbeat_with_orders(
            who("d1", DAEMON),
            beat_of(&unannounced, vec![runs(WEB, "1.0")]),
        )
        .expect("answered");
    assert!(
        !matches!(order, Some(Order::Force { .. })),
        "no force for a daemon that did not announce forced-v1: {order:?}"
    );
    assert_eq!(
        answer.features.as_deref(),
        Some(&["deferred-v1", "halt-v1", "heartbeat-v2"].map(String::from)[..])
    );
}

/// The coordinator advertises `forced-v1` ([`forced_v1_is_acted_on`]).
#[tokio::test]
async fn a_forced_daemon_is_told_forced_v1_with_the_force() {
    assert!(crate::service::FEATURES.contains(&crate::force::FEATURE));
    forced_v1_is_acted_on().await;
}
