use std::time::Duration;

use hostkeeper_wire::RoleName;
use hostkeeper_wire::wire::{Heartbeat, Outcome};

use crate::converge::{Convergence, RoleCounts, State, converge_service, role_counts};
use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::services::{ServiceDef, Services};
use crate::testutil::{
    LEGACY, OPERATOR, Rig, beat, converge, desired, fleet, id, last, role, secs, unhealthy,
    updating,
};

const NOW: u64 = 1_000;

fn read(d: Option<(&str, u32, u32)>, hosts: &[(&str, RoleName, Heartbeat)]) -> Convergence {
    let d = d.map(|(v, s, f)| desired(v, s, f));
    converge(d.as_ref(), &fleet(secs(NOW), hosts), secs(NOW))
}

fn counted<'a>(c: &'a Convergence, name: &str) -> &'a RoleCounts {
    c.roles.iter().find(|r| r.role == name).expect("a role")
}

/// Catches: a farm where every live host runs the desired version, healthy and idle, not
/// reading converged; and the counts per role not being the hosts' own.
#[test]
fn a_farm_all_at_the_desired_version_has_converged() {
    let c = read(
        Some(("2.0", 1, 1)),
        &[
            ("fe1", role("frontend"), beat("2.0")),
            ("st1", role("storage"), beat("2.0")),
            ("st2", role("storage"), beat("2.0")),
            ("d1", role("daemon"), beat("2.0")),
        ],
    );
    assert_eq!(c.state, State::Converged);
    assert_eq!(c.desired.as_ref().expect("desired").as_str(), "2.0");
    assert!(!c.quiet);
    let names: Vec<&str> = c.roles.iter().map(|r| r.role.as_str()).collect();
    assert_eq!(names, ["frontend", "storage", "daemon"]);
    assert_eq!(
        *counted(&c, "storage"),
        RoleCounts {
            role: "storage".to_owned(),
            at_desired: 2,
            updating: 0,
            failed: 0,
            down: 0
        }
    );
}

/// Catches: a host still behind, or one mid-update, being passed over: each reads
/// in_progress and is counted where it stands.
#[test]
fn a_host_behind_or_updating_keeps_the_farm_in_progress() {
    let behind = read(
        Some(("2.0", 0, 0)),
        &[
            ("d1", role("daemon"), beat("2.0")),
            ("d2", role("daemon"), beat("1.0")),
        ],
    );
    assert_eq!(behind.state, State::InProgress);
    assert_eq!(counted(&behind, "daemon").at_desired, 1);
    let mid = read(
        Some(("2.0", 0, 0)),
        &[
            ("d1", role("daemon"), beat("2.0")),
            ("d2", role("daemon"), updating(beat("1.0"), "2.0")),
        ],
    );
    assert_eq!(mid.state, State::InProgress);
    assert_eq!(counted(&mid, "daemon").updating, 1);
    // A host that says it is updating while already on the version is not converged either.
    let odd = read(
        Some(("2.0", 0, 0)),
        &[("d1", role("daemon"), updating(beat("2.0"), "2.0"))],
    );
    assert_eq!(odd.state, State::InProgress);
}

/// Catches: a failed update to the desired version not blocking, and a failed update to
/// another version blocking: only the first is blocked, and it wins over a roll that is
/// still moving elsewhere.
#[test]
fn a_failed_update_to_the_desired_version_blocks_and_to_an_older_one_does_not() {
    let blocked = read(
        Some(("2.0", 0, 0)),
        &[
            (
                "d1",
                role("daemon"),
                last(beat("1.0"), "2.0", Outcome::Failed),
            ),
            ("d2", role("daemon"), updating(beat("1.0"), "2.0")),
        ],
    );
    assert_eq!(blocked.state, State::Blocked);
    let d = counted(&blocked, "daemon");
    assert_eq!((d.failed, d.updating, d.at_desired), (1, 1, 0));
    let older = read(
        Some(("2.0", 0, 0)),
        &[
            (
                "d1",
                role("daemon"),
                last(beat("1.0"), "1.5", Outcome::Failed),
            ),
            ("d2", role("daemon"), beat("2.0")),
            (
                "d3",
                role("daemon"),
                last(beat("1.0"), "1.5", Outcome::Succeeded),
            ),
        ],
    );
    assert_eq!(older.state, State::InProgress);
    assert_eq!(counted(&older, "daemon").failed, 0);
    // A failure to the desired version of a host that has since come to run it, or that
    // succeeded, is no failure.
    let healed = read(
        Some(("2.0", 0, 0)),
        &[
            (
                "d1",
                role("daemon"),
                last(beat("2.0"), "2.0", Outcome::Failed),
            ),
            (
                "d2",
                role("daemon"),
                last(beat("2.0"), "2.0", Outcome::Succeeded),
            ),
        ],
    );
    assert_eq!(healed.state, State::Converged);
    assert_eq!(counted(&healed, "daemon").failed, 0);
    // A failed host that is trying again is updating, not failed.
    let retry = read(
        Some(("2.0", 0, 0)),
        &[(
            "d1",
            role("daemon"),
            updating(last(beat("1.0"), "2.0", Outcome::Failed), "2.0"),
        )],
    );
    assert_eq!(retry.state, State::InProgress);
    assert_eq!(counted(&retry, "daemon").failed, 0);
}

/// Catches: a silent host counted as at the desired version or as live: a host that last
/// said it ran the desired version but has not heartbeated for the down limit is `down`
/// and nowhere else, a silent failed host does not block, and a farm with only silent hosts
/// is not converged.
#[test]
fn a_silent_host_is_down_and_nothing_else() {
    let mut f = fleet(
        secs(NOW),
        &[
            ("d1", role("daemon"), beat("2.0")),
            ("d2", role("daemon"), beat("2.0")),
        ],
    );
    f.record_in(
        LEGACY,
        id("d3"),
        role("daemon"),
        last(beat("1.0"), "2.0", Outcome::Failed),
        secs(NOW - 100),
    );
    f.record_in(
        LEGACY,
        id("d4"),
        role("daemon"),
        beat("2.0"),
        secs(NOW - 100),
    );
    let d = desired("2.0", 0, 0);
    let c = converge(Some(&d), &f, secs(NOW));
    assert_eq!(
        *counted(&c, "daemon"),
        RoleCounts {
            role: "daemon".to_owned(),
            at_desired: 2,
            updating: 0,
            failed: 0,
            down: 2
        }
    );
    assert_eq!(c.state, State::Converged);
    let alone = read(None, &[]);
    assert_eq!(alone.state, State::InProgress);
    let silent_only = converge(
        Some(&d),
        &fleet(secs(0), &[("d1", role("daemon"), beat("2.0"))]),
        secs(NOW),
    );
    assert_eq!(silent_only.state, State::InProgress);
}

/// Catches: a coordinator that has just restarted reporting converged from what little it has
/// heard: inside the quiet start, or with no host, the farm is in progress; with one host at the
/// version it converges, whatever minimum the desired state names (it is kept, and nothing reads
/// it), and an unhealthy host at the version does not keep it from converging.
#[test]
fn a_restarted_coordinator_does_not_report_a_vacuous_convergence() {
    let hosts = [("st1", role("storage"), beat("2.0"))];
    let d = desired("2.0", 1, 0);
    let f = fleet(secs(10), &hosts);
    let quiet = converge(Some(&d), &f, secs(20));
    assert!(quiet.quiet);
    assert_eq!(quiet.state, State::InProgress);
    assert_eq!(read(Some(("2.0", 0, 0)), &[]).state, State::InProgress);
    for (storage, frontend) in [(0, 0), (1, 0), (2, 0), (1, 1), (9, 9)] {
        let one = read(Some(("2.0", storage, frontend)), &hosts);
        assert_eq!(one.state, State::Converged, "{storage} {frontend}");
    }
    let sick = read(
        Some(("2.0", 1, 0)),
        &[("st1", role("storage"), unhealthy(beat("2.0")))],
    );
    assert_eq!(sick.state, State::Converged);
}

const AGENT: &str = "[[service]]\nname = \"agent\"\nroles = [\"daemon\"]\n";

/// A fleet whose hosts all heartbeated `service` at second 990: `kbf` hosts are not in it.
fn fleet_of(service: &str, hosts: &[(&str, RoleName, Heartbeat)]) -> Fleet {
    let mut f = fleet(secs(990), &[("kbf1", role("storage"), beat("9.0"))]);
    for (name, role, b) in hosts {
        f.record_in(service, id(name), role.clone(), b.clone(), secs(990));
    }
    f
}

/// Behaviour: a service's convergence reads its own hosts and its own desired state, never
/// kbf's: a kbf host at another version does not count, the role counts are the service's, one
/// daemon at the version is the whole service (no minimum is read), a host behind keeps it in
/// progress, and a failed update to the service's desired version blocks it.
///
/// Catches: the hosts of another service counted in, or a minimum of hosts up read again.
#[test]
fn a_service_converges_on_its_own_hosts() {
    let services = Services::parse(AGENT).expect("services");
    let def = services.get("agent").expect("agent");
    let d = desired("3.0", 0, 0);
    let behind = fleet_of(
        "agent",
        &[
            ("d1", role("daemon"), beat("3.0")),
            ("d2", role("daemon"), beat("2.0")),
        ],
    );
    assert_eq!(
        converge_service(def, Some(&d), &behind, secs(NOW)).state,
        State::InProgress
    );
    let one = fleet_of("agent", &[("d1", role("daemon"), beat("3.0"))]);
    let alone = converge_service(def, Some(&d), &one, secs(NOW));
    assert_eq!(alone.state, State::Converged, "no minimum is read");
    assert_eq!(alone.desired.as_ref().expect("desired").as_str(), "3.0");
    assert_eq!(counted(&alone, "daemon").at_desired, 1);
    assert_eq!(alone.roles.len(), 1, "the kbf storage host is not counted");
    let two = fleet_of(
        "agent",
        &[
            ("d1", role("daemon"), beat("3.0")),
            ("d2", role("daemon"), beat("3.0")),
        ],
    );
    assert_eq!(
        converge_service(def, Some(&d), &two, secs(NOW)).state,
        State::Converged
    );
    let stuck = fleet_of(
        "agent",
        &[
            ("d1", role("daemon"), beat("3.0")),
            (
                "d2",
                role("daemon"),
                last(beat("2.0"), "3.0", Outcome::Failed),
            ),
        ],
    );
    assert_eq!(
        converge_service(def, Some(&d), &stuck, secs(NOW)).state,
        State::Blocked
    );
    let none = converge_service(def, None, &two, secs(NOW));
    assert_eq!((none.desired, none.state), (None, State::InProgress));
    assert!(role_counts(def, None, &two, secs(NOW)).is_empty());
}

/// Catches: a minimum of hosts up read again: the service has one live host at the version and
/// that host is unhealthy; whatever the desired state names for `storage` and `frontend` it
/// reads converged, and a second host behind still keeps it in progress.
#[test]
fn convergence_reads_no_minimum() {
    let def = ServiceDef::new("pool", &["builder"]);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_named_in(
        "pool",
        id("b1"),
        "builder",
        unhealthy(beat("2.0")),
        secs(990),
    );
    for (storage, frontend) in [(0, 0), (1, 0), (5, 5)] {
        let d = desired("2.0", storage, frontend);
        let state = converge_service(&def, Some(&d), &f, secs(NOW)).state;
        assert_eq!(state, State::Converged, "{storage} {frontend}");
    }
    f.record_named_in("pool", id("b2"), "builder", beat("1.0"), secs(990));
    let d = desired("2.0", 0, 0);
    assert_eq!(
        converge_service(&def, Some(&d), &f, secs(NOW)).state,
        State::InProgress
    );
}

/// The names of `roles`, in the order they are listed.
fn role_names(roles: &[RoleCounts]) -> Vec<&str> {
    roles.iter().map(|r| r.role.as_str()).collect()
}

/// Catches: roles listed alphabetically (`daemon`, `frontend`, `mac-daemon`, `storage`)
/// where the status and CI have always read frontend, storage, daemon, mac-daemon: hosts
/// heard in the reverse order come out in that order.
#[test]
fn kbf_roles_in_convergence_keep_the_legacy_order() {
    let f = fleet(
        secs(990),
        &[
            ("m1", role("mac-daemon"), beat("2.0")),
            ("d1", role("daemon"), beat("2.0")),
            ("s1", role("storage"), beat("2.0")),
            ("f1", role("frontend"), beat("2.0")),
        ],
    );
    let c = converge(Some(&desired("2.0", 1, 1)), &f, secs(NOW));
    assert_eq!(
        role_names(&c.roles),
        ["frontend", "storage", "daemon", "mac-daemon"]
    );
}

/// Catches: roles the four legacy names do not cover listed before them or in map insertion
/// order: a definition with `zeta`, `alpha`, `daemon` and `frontend` lists frontend, then
/// daemon (the legacy order), then alpha, zeta (by name).
#[test]
fn roles_of_a_definition_come_legacy_first_then_by_name() {
    let def = ServiceDef::new("pool", &["zeta", "alpha", "daemon", "frontend"]);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    for role in ["zeta", "alpha", "daemon", "frontend"] {
        f.record_named_in(
            "pool",
            id(&format!("{role}-1")),
            role,
            beat("2.0"),
            secs(990),
        );
    }
    let c = converge_service(&def, Some(&desired("2.0", 0, 0)), &f, secs(NOW));
    assert_eq!(
        role_names(&c.roles),
        ["frontend", "daemon", "alpha", "zeta"]
    );
    assert_eq!(c.state, State::Converged);
}

/// Catches: a host of a role the definition does not name entering the counts (it would
/// keep the service from converging, being behind): a stranger at the old version beside
/// a builder at the new one leaves the service converged, with one role listed.
#[test]
fn convergence_counts_only_the_roles_of_the_definition() {
    let def = ServiceDef::new("pool", &["builder"]);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_named_in("pool", id("b1"), "builder", beat("2.0"), secs(990));
    f.record_named_in("pool", id("x1"), "stranger", beat("1.0"), secs(990));
    let c = converge_service(&def, Some(&desired("2.0", 0, 0)), &f, secs(NOW));
    assert_eq!(role_names(&c.roles), ["builder"]);
    assert_eq!(c.state, State::Converged);
}

/// Catches: the status listing a service's roles in another order than convergence does:
/// hosts of all four roles heard in the reverse order are listed frontend, storage,
/// daemon, mac-daemon under `kbf`.
#[tokio::test]
async fn status_lists_roles_in_the_legacy_order() {
    let rig = Rig::new("converge-role-status");
    rig.set(r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#)
        .await;
    rig.at(2_000);
    for (name, role) in [
        ("m1", role("mac-daemon")),
        ("d1", role("daemon")),
        ("s1", role("storage")),
        ("f1", role("frontend")),
    ] {
        let who = Identity {
            host_id: id(name),
            role,
        };
        rig.coordinator
            .heartbeat(who, beat("2.0"))
            .expect("answered");
    }
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    assert_eq!(
        role_names(&status.per_service["kbf"].roles),
        ["frontend", "storage", "daemon", "mac-daemon"]
    );
}
