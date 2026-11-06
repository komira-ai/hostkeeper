//! The hosts the status and the convergence count for a service assigned beyond its rollout file:
//! an operator's `Services.Assign` of a role or a host adds its hosts to the counts, by the same
//! assignment function the order path asks, and `Services.Unassign` takes them out. The rig is
//! [`crate::marks_support::rig`]: `web` and `db` on the roles `daemon` and `storage`; the roles
//! `mac` and `arm` are in no rollout file.

use hostkeeper_config::platform::Platform as IndexPlatform;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::ServiceBeat;
use serde_json::{Value, json};

use crate::assign_support::{assign, unassign};
use crate::converge::State;
use crate::marks_support::{
    INSTALLER, NEW_DAEMON, WEB, ask_typed, beat_of, hold, rig, runs, scope_host, set, unhold,
};
use crate::orders::Order;
use crate::release_index_support::{beat_on, set_index, try_set};
use crate::testutil::{OPERATOR, Rig};

/// The role counts of `web` in the status, as `[role, at_desired, updating, failed, down]`.
fn roles(rig: &Rig) -> Vec<(String, usize, usize, usize, usize)> {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    status.per_service[WEB]
        .roles
        .iter()
        .map(|c| (c.role.clone(), c.at_desired, c.updating, c.failed, c.down))
        .collect()
}

/// The convergence of `web` as `Releases.Status` answers it, as JSON: its state and roles.
fn releases_status(rig: &Rig) -> Value {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    let answer = crate::grpc::report::status(&status, WEB);
    assert_eq!(answer.services.len(), 1);
    let convergence = answer.services[0]
        .convergence
        .as_ref()
        .expect("a convergence");
    json!({
        "state": convergence.state().as_str_name(),
        "roles": convergence
            .roles
            .iter()
            .map(|r| json!([r.role, r.at_desired]))
            .collect::<Vec<_>>(),
        "missing": convergence.hosts.as_ref().map(|h| h.missing),
    })
}

/// The state of `web` as `GET /v1/convergence` answers it, which must be the status's too.
fn state(rig: &Rig) -> State {
    let answered = rig
        .coordinator
        .convergence_verified(WEB)
        .expect("a convergence");
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    assert_eq!(status.per_service[WEB].convergence, answered);
    answered.state
}

/// The heartbeat of the host `host` of the role `role`, an agent that takes installs, listing
/// `services`; the order it is given, if any.
fn beat(rig: &Rig, host: &str, role: &str, services: Vec<ServiceBeat>) -> Option<Order> {
    ask_typed(rig, host, role, beat_of(INSTALLER, services)).1
}

fn row(
    role: &str,
    at: usize,
    updating: usize,
    failed: usize,
    down: usize,
) -> (String, usize, usize, usize, usize) {
    (role.to_owned(), at, updating, failed, down)
}

/// Catches: the status and the convergence counting only the roles of the rollout file, so a
/// service an operator assigned to another role reads `converged` while that role's hosts do not
/// run the desired version. A host of the assigned role that does not list the service is missing;
/// once it runs an older version its role is counted behind; only when it is at the desired
/// version is the service converged. A host assigned by its own entry brings its role too, and the
/// added roles follow the rollout file's, by name.
#[tokio::test]
async fn a_role_assigned_by_an_operator_is_counted_until_its_hosts_are_at_desired() {
    let rig = rig("counts-assigned-role", "");
    set(&rig, WEB, "2.0", 1).await;
    beat(&rig, "d1", "daemon", vec![runs(WEB, "2.0")]);
    beat(&rig, "m1", "mac", vec![]);
    assert_eq!(state(&rig), State::Converged);
    assert_eq!(roles(&rig), [row("daemon", 1, 0, 0, 0)]);

    assign(&rig, WEB, "", "mac").expect("assigned");
    assert_eq!(state(&rig), State::InProgress);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_IN_PROGRESS", "roles": [["daemon", 1]], "missing": 1})
    );

    beat(&rig, "m1", "mac", vec![runs(WEB, "1.0")]);
    assert_eq!(state(&rig), State::InProgress);
    assert_eq!(roles(&rig), [row("daemon", 1, 0, 0, 0)]);
    let mut updating = runs(WEB, "1.0");
    updating.updating = Some(crate::testutil::version("2.0"));
    beat(&rig, "m1", "mac", vec![updating]);
    assert_eq!(state(&rig), State::InProgress);
    assert_eq!(
        roles(&rig),
        [row("daemon", 1, 0, 0, 0), row("mac", 0, 1, 0, 0)]
    );

    beat(&rig, "m1", "mac", vec![runs(WEB, "2.0")]);
    assert_eq!(state(&rig), State::Converged);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_CONVERGED", "roles": [["daemon", 1], ["mac", 1]], "missing": null})
    );

    beat(&rig, "a1", "arm", vec![runs(WEB, "1.0")]);
    assert_eq!(state(&rig), State::Converged);
    assign(&rig, WEB, "a1", "").expect("assigned");
    beat(&rig, "a1", "arm", vec![runs(WEB, "1.0")]);
    assert_eq!(state(&rig), State::InProgress);
    beat(&rig, "a1", "arm", vec![runs(WEB, "2.0")]);
    assert_eq!(state(&rig), State::Converged);
    assert_eq!(
        roles(&rig),
        [
            row("daemon", 1, 0, 0, 0),
            row("arm", 1, 0, 0, 0),
            row("mac", 1, 0, 0, 0)
        ]
    );
}

/// Catches: an `Unassign` that leaves its hosts in the counts, so a service taken off a role
/// never converges again while that role keeps an older version: after the `Unassign` of the
/// role, and of a host assigned by its own entry, neither is counted, missing or behind.
#[tokio::test]
async fn an_unassign_takes_the_hosts_out_of_the_count() {
    let rig = rig("counts-unassigned-role", "");
    set(&rig, WEB, "2.0", 1).await;
    beat(&rig, "d1", "daemon", vec![runs(WEB, "2.0")]);
    beat(&rig, "m2", "mac", vec![]);
    assign(&rig, WEB, "", "mac").expect("assigned");
    beat(&rig, "m1", "mac", vec![runs(WEB, "1.0")]);
    beat(&rig, "a1", "arm", vec![runs(WEB, "1.0")]);
    assign(&rig, WEB, "a1", "").expect("assigned");
    beat(&rig, "a1", "arm", vec![runs(WEB, "1.0")]);
    assert_eq!(state(&rig), State::InProgress);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_IN_PROGRESS", "roles": [["daemon", 1]], "missing": 1})
    );

    unassign(&rig, WEB, "", "mac", false).expect("unassigned");
    assert_eq!(state(&rig), State::InProgress);
    unassign(&rig, WEB, "a1", "", false).expect("unassigned");
    assert_eq!(state(&rig), State::Converged);
    beat(&rig, "m1", "mac", vec![runs(WEB, "1.0")]);
    beat(&rig, "a1", "arm", vec![runs(WEB, "1.0")]);
    assert_eq!(state(&rig), State::Converged);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_CONVERGED", "roles": [["daemon", 1]], "missing": null})
    );
}

/// Catches: the counts reading an assignment the order path does not: a rollout role taken off by
/// `Unassign` while its host still runs an older version, a host of an assigned role whose agent
/// cannot list its services (never ordered, so never missing), and a held host of an assigned
/// role (never ordered, so never counted) leave the service converged, as the order path leaves
/// them unordered.
#[tokio::test]
async fn hosts_the_order_path_does_not_order_are_not_counted() {
    let rig = rig("counts-not-ordered", "");
    set(&rig, WEB, "2.0", 1).await;
    beat(&rig, "d1", "daemon", vec![runs(WEB, "2.0")]);
    beat(&rig, "s1", "storage", vec![runs(WEB, "1.0")]);
    assert_eq!(state(&rig), State::InProgress);
    unassign(&rig, WEB, "", "storage", false).expect("unassigned");
    assert_eq!(beat(&rig, "s1", "storage", vec![runs(WEB, "1.0")]), None);
    assert_eq!(state(&rig), State::Converged);

    ask_typed(&rig, "m1", "mac", beat_of(&[], vec![]));
    assign(&rig, WEB, "", "mac").expect("assigned");
    ask_typed(&rig, "m1", "mac", beat_of(&[], vec![]));
    assert_eq!(state(&rig), State::Converged);

    beat(&rig, "m2", "mac", vec![runs(WEB, "2.0")]);
    hold(&rig, WEB, scope_host("m2")).expect("held");
    assert_eq!(beat(&rig, "m2", "mac", vec![runs(WEB, "1.0")]), None);
    assert_eq!(state(&rig), State::Converged);
    assert_eq!(roles(&rig), [row("daemon", 1, 0, 0, 0)]);
}

/// Catches: an assigned host counted missing although the order path can never install the
/// service on it, which would keep the service from converging for ever: a host whose daemon lists
/// its services but does not take installs (`unsupported`), and a host whose platform the release
/// index lists no package for (`no_package_for_platform`), are not missing; a host of the same
/// role on a listed platform is.
#[tokio::test]
async fn an_assigned_host_the_order_path_cannot_install_is_not_missing() {
    let rig = rig("counts-not-installable", "");
    let linux = [
        (IndexPlatform::LinuxX86_64, 1),
        (IndexPlatform::LinuxArm64, 2),
    ];
    set_index(&rig, WEB, "2.0", &linux).await;
    let on = |host: &str, platform: Platform, services: Vec<ServiceBeat>| {
        ask_typed(&rig, host, "mac", beat_on(platform, services)).1
    };
    ask_typed(
        &rig,
        "d1",
        "daemon",
        beat_on(Platform::LinuxX86_64, vec![runs(WEB, "2.0")]),
    );
    on("m1", Platform::DarwinArm64, vec![]);
    ask_typed(&rig, "m2", "mac", beat_of(NEW_DAEMON, vec![]));
    assign(&rig, WEB, "", "mac").expect("assigned");
    assert_eq!(on("m1", Platform::DarwinArm64, vec![]), None);
    assert_eq!(
        ask_typed(&rig, "m2", "mac", beat_of(NEW_DAEMON, vec![])).1,
        None
    );
    assert_eq!(state(&rig), State::Converged);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_CONVERGED", "roles": [["daemon", 1]], "missing": null})
    );

    on("m3", Platform::LinuxArm64, vec![]);
    assert_eq!(state(&rig), State::InProgress);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_IN_PROGRESS", "roles": [["daemon", 1]], "missing": 1})
    );
}

/// Catches: an assigned host counted missing while the order path holds its install back for
/// good: a host an operator holds (`held`), and every host while the desired version names no
/// package (`no_digest`), are not missing; once the hold is released, and once CI sets a version
/// with a package, the host is missing until it installs.
#[tokio::test]
async fn an_assigned_host_held_or_without_a_package_is_not_missing() {
    let rig = rig("counts-held-no-digest", "");
    try_set(&rig, &json!({"service": WEB, "version": "2.0"}))
        .await
        .expect("set without a digest");
    beat(&rig, "d1", "daemon", vec![runs(WEB, "2.0")]);
    beat(&rig, "m1", "mac", vec![]);
    assign(&rig, WEB, "", "mac").expect("assigned");
    assert_eq!(beat(&rig, "m1", "mac", vec![]), None);
    assert_eq!(state(&rig), State::Converged);

    set(&rig, WEB, "2.0", 1).await;
    hold(&rig, WEB, scope_host("m1")).expect("held");
    assert_eq!(beat(&rig, "m1", "mac", vec![]), None);
    assert_eq!(state(&rig), State::Converged);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_CONVERGED", "roles": [["daemon", 1]], "missing": null})
    );

    unhold(&rig, WEB, scope_host("m1")).expect("released");
    assert_eq!(state(&rig), State::InProgress);
    assert_eq!(
        releases_status(&rig),
        json!({"state": "CONVERGENCE_STATE_IN_PROGRESS", "roles": [["daemon", 1]], "missing": 1})
    );
}
