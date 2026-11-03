use hostkeeper_wire::RoleName;
use hostkeeper_wire::wire::{Heartbeat, Outcome};

use crate::converge::{Convergence, RoleCounts, State, converge_service, role_counts};
use crate::fleet::Fleet;
use crate::services::{RoleDef, ServiceDef, Services};
use crate::testutil::{
    LEGACY, beat, converge, desired, fleet, last, role, secs, unhealthy, updating,
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
        crate::testutil::id("d3"),
        role("daemon"),
        last(beat("1.0"), "2.0", Outcome::Failed),
        secs(NOW - 100),
    );
    f.record_in(
        LEGACY,
        crate::testutil::id("d4"),
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

/// Catches: a coordinator that has just restarted reporting converged from what little it
/// has heard: inside the quiet start, with no host, or with fewer hosts up than the
/// minimums, the farm is in progress; with the minimums met it converges.
#[test]
fn a_restarted_coordinator_does_not_report_a_vacuous_convergence() {
    let hosts = [("st1", role("storage"), beat("2.0"))];
    let d = desired("2.0", 1, 0);
    let f = fleet(secs(10), &hosts);
    let quiet = converge(Some(&d), &f, secs(20));
    assert!(quiet.quiet);
    assert_eq!(quiet.state, State::InProgress);
    assert_eq!(read(Some(("2.0", 0, 0)), &[]).state, State::InProgress);
    let short = read(Some(("2.0", 2, 0)), &hosts);
    assert_eq!(short.state, State::InProgress);
    let no_frontend = read(Some(("2.0", 1, 1)), &hosts);
    assert_eq!(no_frontend.state, State::InProgress);
    assert_eq!(read(Some(("2.0", 1, 0)), &hosts).state, State::Converged);
    // A storage host at the version but unhealthy is not up.
    let sick = read(
        Some(("2.0", 1, 0)),
        &[("st1", role("storage"), unhealthy(beat("2.0")))],
    );
    assert_eq!(sick.state, State::InProgress);
}

const TWO_DAEMONS: &str = "[[service]]\nname = \"agent\"\n[service.roles.daemon]\nmin_up = 2\n";

/// A fleet whose hosts all heartbeated `service` at second 990: `kbf` hosts are not in it.
fn fleet_of(service: &str, hosts: &[(&str, RoleName, Heartbeat)]) -> crate::fleet::Fleet {
    let mut f = fleet(secs(990), &[("kbf1", role("storage"), beat("9.0"))]);
    for (name, role, b) in hosts {
        f.record_in(
            service,
            crate::testutil::id(name),
            role.clone(),
            b.clone(),
            secs(990),
        );
    }
    f
}

/// Behaviour: a service's convergence reads its own hosts and its own desired state, never
/// kbf's: a kbf host at another version does not count, the role counts are the service's,
/// the service's required hosts up come from its role definition, and a failed update to
/// the service's desired version blocks it.
#[test]
fn a_service_converges_on_its_own_hosts_and_its_own_minimums() {
    let services = Services::parse(TWO_DAEMONS).expect("services");
    let def = services.get("agent").expect("agent");
    let d = desired("3.0", 0, 0);
    let one = fleet_of("agent", &[("d1", role("daemon"), beat("3.0"))]);
    let short = converge_service(def, Some(&d), &one, secs(NOW));
    assert_eq!(short.state, State::InProgress, "two daemons must be up");
    assert_eq!(short.desired.as_ref().expect("desired").as_str(), "3.0");
    assert_eq!(counted(&short, "daemon").at_desired, 1);
    assert_eq!(short.roles.len(), 1, "the kbf storage host is not counted");
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

/// Catches: the `never_last` floor of a decision applied to the convergence of a definition
/// other than kbf's: one rule reads every service, the role's `min_up` else the desired
/// state's, so a `never_last` role whose minimum is 0 needs no host up to read `converged`
/// (decisions still keep its last host up). An unhealthy host at the desired version is live
/// and not up: with the minimum 0 the service converges, with a `min_up` of 1 it does not.
#[test]
fn convergence_applies_the_minimum_without_the_never_last_floor() {
    let guarded = RoleDef {
        never_last: true,
        ..RoleDef::plain()
    };
    let def = ServiceDef::new("pool", &[("builder", guarded.clone())]);
    let mut f = Fleet::new(std::time::Duration::ZERO);
    f.record_in(
        "pool",
        crate::testutil::id("b1"),
        role("builder"),
        unhealthy(beat("2.0")),
        secs(990),
    );
    let d = desired("2.0", 0, 0);
    assert_eq!(
        converge_service(&def, Some(&d), &f, secs(NOW)).state,
        State::Converged
    );
    let needs_one = ServiceDef::new(
        "pool",
        &[(
            "builder",
            RoleDef {
                min_up: Some(1),
                ..guarded
            },
        )],
    );
    assert_eq!(
        converge_service(&needs_one, Some(&d), &f, secs(NOW)).state,
        State::InProgress
    );
}
