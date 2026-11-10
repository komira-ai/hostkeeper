//! Goldens of the decision: for fixed fleets, desired states and times, the reply every host
//! gets, written out as literals. `decide_tests.rs` has one test per rule; these pin the whole
//! decision, so a change that moves what a host is told moves a literal here, in the same
//! change, where a reviewer sees it.
//!
//! Each scenario is the coordinator's own loop: hosts are asked in the order listed, and an
//! `update` is noted with `Fleet::order` before the next host is asked, as `service.rs` does
//! under its lock. A scenario that asks without ordering says so. The canary gate has its own
//! tests (`bake_canary_tests.rs`), so there is none here.
//!
//! A service rolls out by stages (`stages.rs`): with the default plan the first stage holds one
//! host of the eleven, so the first host asked goes and the others wait for the stage. A
//! scenario that wants the desired state's per-role limits alone to decide rolls kbf in one
//! stage (`stages = [100]`, `run_whole`): then `max_updating` caps the hosts of a role that
//! update at once, `min_up` holds an order that would leave too few of the role up, and the
//! roles do not wait for each other.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::wire::{Heartbeat, Outcome, Reply};

use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::services::Services;
use crate::testutil::{
    KBF_ENTRY, LEGACY, beat, decide_in, desired, fleet, id, kbf_services, last, role, secs,
    unhealthy, updating,
};

const NOW: Duration = Duration::from_secs(1_000);

fn say(reply: &Reply) -> String {
    match reply {
        Reply::Nothing => "nothing".to_owned(),
        Reply::Update { version } => format!("update {version}"),
    }
}

/// One line per host: the reply at `at`. With `order`, an update is noted as the coordinator
/// notes it before the next host is asked.
fn run(f: &mut Fleet, d: &Desired, hosts: &[&str], at: Duration, order: bool) -> Vec<String> {
    run_by(&kbf_services(), f, d, hosts, at, order)
}

/// [`run`] with kbf rolled in one stage (`stages = [100]`): only the desired state's limits hold
/// an order.
fn run_whole(f: &mut Fleet, d: &Desired, hosts: &[&str], order: bool) -> Vec<String> {
    let whole = Services::parse(&format!("{KBF_ENTRY}\nstages = [100]\n")).expect("services");
    run_by(&whole, f, d, hosts, NOW, order)
}

fn run_by(
    services: &Services,
    f: &mut Fleet,
    d: &Desired,
    hosts: &[&str],
    at: Duration,
    order: bool,
) -> Vec<String> {
    let wants = BTreeMap::from([(LEGACY.to_owned(), d.clone())]);
    hosts
        .iter()
        .map(|host| {
            let reply = decide_in(services, &wants, LEGACY, f, &id(host), at);
            if let (true, Reply::Update { version }) = (order, &reply) {
                f.order(LEGACY, id(host), version.clone(), at);
            }
            format!("{host}: {}", say(&reply))
        })
        .collect()
}

fn all_roles(version: &str) -> Fleet {
    fleet(
        NOW,
        &[
            ("s1", role("storage"), beat(version)),
            ("s2", role("storage"), beat(version)),
            ("s3", role("storage"), beat(version)),
            ("d1", role("daemon"), beat(version)),
            ("d2", role("daemon"), beat(version)),
            ("d3", role("daemon"), beat(version)),
            ("m1", role("mac-daemon"), beat(version)),
            ("m2", role("mac-daemon"), beat(version)),
            ("m3", role("mac-daemon"), beat(version)),
            ("f1", role("frontend"), beat(version)),
            ("f2", role("frontend"), beat(version)),
        ],
    )
}

const ALL: [&str; 11] = [
    "s1", "s2", "s3", "d1", "d2", "d3", "m1", "m2", "m3", "f1", "f2",
];

/// The first pass of a roll with the default plan over eleven hosts: the first stage holds one
/// host, so the first host asked goes and every other host waits for the stage, whatever its
/// role and its role's limit (1 storage, 2 daemons, 2 Macs, 1 frontend).
#[test]
fn the_first_pass_of_a_roll_with_the_default_limits() {
    let mut f = all_roles("1.0");
    assert_eq!(
        run(&mut f, &desired("2.0", 1, 1), &ALL, NOW, true),
        [
            "s1: update 2.0",
            "s2: nothing",
            "s3: nothing",
            "d1: nothing",
            "d2: nothing",
            "d3: nothing",
            "m1: nothing",
            "m2: nothing",
            "m3: nothing",
            "f1: nothing",
            "f2: nothing",
        ]
    );
}

/// The limits the desired state sets cap each role within a stage, with kbf in one stage: a
/// storage limit of 2 orders two storage hosts and not the third, a daemon limit of 1 orders one
/// daemon, a frontend goes (the default limit is 1), and a Mac limit of 0 holds the Macs. A role
/// at its limit does not hold another role.
#[test]
fn the_limits_of_the_desired_state_cap_each_role_within_a_stage() {
    let mut f = all_roles("1.0");
    let mut d = desired("2.0", 1, 1);
    d.max_updating.set("storage", 2);
    d.max_updating.set("daemon", 1);
    d.max_updating.set("mac-daemon", 0);
    assert_eq!(
        run_whole(&mut f, &d, &ALL, true),
        [
            "s1: update 2.0",
            "s2: update 2.0",
            "s3: nothing",
            "d1: update 2.0",
            "d2: nothing",
            "d3: nothing",
            "m1: nothing",
            "m2: nothing",
            "m3: nothing",
            "f1: update 2.0",
            "f2: nothing",
        ]
    );
    let macs = || {
        fleet(
            NOW,
            &[
                ("s1", role("storage"), beat("2.0")),
                ("d1", role("daemon"), beat("2.0")),
                ("m1", role("mac-daemon"), beat("1.0")),
                ("m2", role("mac-daemon"), beat("1.0")),
            ],
        )
    };
    assert_eq!(
        run_whole(&mut macs(), &d, &["m1", "m2"], true),
        ["m1: nothing", "m2: nothing"],
        "a Mac limit of 0 holds the role"
    );
    d.max_updating.set("mac-daemon", 2);
    assert_eq!(
        run_whole(&mut macs(), &d, &["m1", "m2"], true),
        ["m1: update 2.0", "m2: update 2.0"],
        "a Mac limit of 2 orders both"
    );
}

/// A roll in progress, asked without ordering: a host that is updating or at the version is
/// told nothing, and every host that is behind waits while the first stage, which three
/// started hosts fill, is not done, a daemon with a free slot of its role included.
#[test]
fn a_roll_in_progress() {
    let mut f = fleet(
        NOW,
        &[
            ("s1", role("storage"), updating(beat("1.0"), "2.0")),
            ("s2", role("storage"), beat("2.0")),
            ("s3", role("storage"), beat("1.0")),
            ("d1", role("daemon"), beat("2.0")),
            ("d2", role("daemon"), updating(beat("1.0"), "2.0")),
            ("d3", role("daemon"), beat("1.0")),
            ("m1", role("mac-daemon"), beat("2.0")),
            ("f1", role("frontend"), beat("1.0")),
            ("f2", role("frontend"), beat("1.0")),
        ],
    );
    let hosts = ["s1", "s2", "s3", "d1", "d2", "d3", "m1", "f1", "f2"];
    assert_eq!(
        run(&mut f, &desired("2.0", 1, 1), &hosts, NOW, false),
        [
            "s1: nothing",
            "s2: nothing",
            "s3: nothing",
            "d1: nothing",
            "d2: nothing",
            "d3: nothing",
            "m1: nothing",
            "f1: nothing",
            "f2: nothing",
        ]
    );
}

/// Every other role at the version: the frontends go one at a time, and the second waits for
/// the first's stage.
#[test]
fn frontends_go_one_at_a_time() {
    let mut f = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("2.0")),
            ("d1", role("daemon"), beat("2.0")),
            ("m1", role("mac-daemon"), beat("2.0")),
            ("f1", role("frontend"), beat("1.0")),
            ("f2", role("frontend"), beat("1.0")),
            ("f3", role("frontend"), beat("1.0")),
        ],
    );
    assert_eq!(
        run(
            &mut f,
            &desired("2.0", 1, 1),
            &["f1", "f2", "f3"],
            NOW,
            true
        ),
        ["f1: update 2.0", "f2: nothing", "f3: nothing"]
    );
}

/// A frontend with no other frontend up is ordered when the desired state names no frontend up
/// and held (`min_up`) when it names one; a storage host is held when ordering it would leave
/// fewer storage hosts up than the minimum. Two storage hosts of a minimum of two go neither; of
/// three, with kbf in one stage and the storage limit at 3, one goes, as the other two must stay
/// up, and with a minimum of one two go.
#[test]
fn the_minimums_hold_an_order_that_would_leave_too_few_up() {
    // f2 has been silent for 900 s: f1 is the only frontend up.
    let mut f = fleet(NOW, &[("f1", role("frontend"), beat("1.0"))]);
    f.record_in(LEGACY, id("f2"), role("frontend"), beat("1.0"), secs(100));
    assert_eq!(
        run(&mut f, &desired("2.0", 0, 0), &["f1"], NOW, false),
        ["f1: update 2.0"]
    );
    assert_eq!(
        run(&mut f, &desired("2.0", 0, 1), &["f1"], NOW, false),
        ["f1: nothing"],
        "f1 is the only frontend up"
    );
    // Two storage hosts, a minimum of two others up: neither goes.
    let mut two = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("1.0")),
            ("s2", role("storage"), beat("1.0")),
        ],
    );
    assert_eq!(
        run(&mut two, &desired("2.0", 2, 1), &["s1", "s2"], NOW, true),
        ["s1: nothing", "s2: nothing"]
    );
    let three = || {
        fleet(
            NOW,
            &[
                ("s1", role("storage"), beat("1.0")),
                ("s2", role("storage"), beat("1.0")),
                ("s3", role("storage"), beat("1.0")),
            ],
        )
    };
    let hosts = ["s1", "s2", "s3"];
    let mut d = desired("2.0", 2, 1);
    d.max_updating.set("storage", 3);
    assert_eq!(
        run_whole(&mut three(), &d, &hosts, true),
        ["s1: update 2.0", "s2: nothing", "s3: nothing"]
    );
    let mut d = desired("2.0", 1, 1);
    d.max_updating.set("storage", 3);
    assert_eq!(
        run_whole(&mut three(), &d, &hosts, true),
        ["s1: update 2.0", "s2: update 2.0", "s3: nothing"],
        "a minimum of one leaves room for two"
    );
}

/// A host that failed the desired version is not ordered it again, healthy or not; a new desired
/// version lifts the block for it. With kbf in one stage and the daemon limit at 1, the failed
/// host that is unhealthy (d2) holds the one slot, so nobody else goes until it heartbeats
/// healthy, and only d2 itself may be told a new version; the host that failed and is healthy
/// (d1) holds none, and a host that failed another version (d4) is not held back once d2 is
/// healthy.
#[test]
fn a_failed_host_is_not_ordered_again() {
    let failed = |b: Heartbeat, target: &str| last(b, target, Outcome::Failed);
    let hosts = ["d1", "d2", "d3", "d4"];
    let limited = |mut d: Desired| {
        d.max_updating.set("daemon", 1);
        d
    };
    let with = |d2_healthy: bool| {
        let d2 = failed(beat("1.0"), "2.0");
        fleet(
            NOW,
            &[
                ("d1", role("daemon"), failed(beat("1.0"), "2.0")),
                (
                    "d2",
                    role("daemon"),
                    if d2_healthy { d2 } else { unhealthy(d2) },
                ),
                ("d3", role("daemon"), beat("1.0")),
                ("d4", role("daemon"), failed(beat("1.0"), "1.5")),
            ],
        )
    };
    let mut f = with(false);
    assert_eq!(
        run_whole(&mut f, &limited(desired("2.0", 1, 1)), &hosts, false),
        ["d1: nothing", "d2: nothing", "d3: nothing", "d4: nothing"]
    );
    assert_eq!(
        run_whole(&mut f, &limited(desired("3.0", 1, 1)), &hosts, false),
        [
            "d1: nothing",
            "d2: update 3.0",
            "d3: nothing",
            "d4: nothing"
        ]
    );
    let one = limited(desired("2.0", 1, 1));
    assert_eq!(
        run_whole(&mut f, &one, &hosts, false),
        ["d1: nothing", "d2: nothing", "d3: nothing", "d4: nothing"],
        "the unhealthy failed host holds the only slot"
    );
    let mut f = with(true);
    assert_eq!(
        run_whole(&mut f, &one, &hosts, false),
        [
            "d1: nothing",
            "d2: nothing",
            "d3: update 2.0",
            "d4: update 2.0"
        ],
        "healthy again, it holds none"
    );
}

/// A role behind keeps no other role waiting: with nothing ordered, every host that is behind,
/// the frontends included, is told to update; once the first is ordered the others wait for its
/// stage and not for its role.
#[test]
fn no_role_is_kept_waiting_by_another() {
    for (kind, host) in [
        (role("storage"), "s1"),
        (role("daemon"), "d1"),
        (role("mac-daemon"), "m1"),
    ] {
        let behind = || {
            let mut f = fleet(
                NOW,
                &[
                    ("s1", role("storage"), beat("2.0")),
                    ("d1", role("daemon"), beat("2.0")),
                    ("m1", role("mac-daemon"), beat("2.0")),
                    ("f1", role("frontend"), beat("1.0")),
                    ("f2", role("frontend"), beat("1.0")),
                ],
            );
            f.record_in(LEGACY, id(host), kind.clone(), beat("1.0"), NOW);
            f
        };
        let d = desired("2.0", 0, 1);
        let hosts = [host, "f1", "f2"];
        assert_eq!(
            run(&mut behind(), &d, &hosts, NOW, false),
            [
                format!("{host}: update 2.0"),
                "f1: update 2.0".to_owned(),
                "f2: update 2.0".to_owned()
            ],
            "{kind}: nothing ordered"
        );
        assert_eq!(
            run(&mut behind(), &d, &hosts, NOW, true),
            [
                format!("{host}: update 2.0"),
                "f1: nothing".to_owned(),
                "f2: nothing".to_owned()
            ],
            "{kind}: the first is ordered"
        );
    }
}

/// An order holds its slot for 120 s without the host reporting it: the host that asks again is
/// told again, the other waits at 119 s and goes at 120 s. The hosts keep heartbeating at the
/// time of each question.
#[test]
fn an_order_ages_out_after_120_seconds() {
    let mut f = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("1.0")),
            ("s2", role("storage"), beat("1.0")),
        ],
    );
    let d = desired("2.0", 0, 1);
    let mut lines = Vec::new();
    for (at, host) in [
        (1_000, "s1"),
        (1_000, "s2"),
        (1_010, "s1"),
        (1_119, "s2"),
        (1_120, "s2"),
    ] {
        let at = secs(at);
        f.record_in(LEGACY, id("s1"), role("storage"), beat("1.0"), at);
        f.record_in(LEGACY, id("s2"), role("storage"), beat("1.0"), at);
        let line = run(&mut f, &d, &[host], at, true);
        lines.push(format!("t={} {}", at.as_secs(), line[0]));
    }
    assert_eq!(
        lines,
        [
            "t=1000 s1: update 2.0",
            "t=1000 s2: nothing",
            "t=1010 s1: update 2.0",
            "t=1119 s2: nothing",
            "t=1120 s2: update 2.0",
        ]
    );
}

/// No order in the first 30 s after the coordinator starts.
#[test]
fn thirty_seconds_of_quiet() {
    let mut f = Fleet::new(secs(970), crate::fleet::QUIET_START);
    f.record_in(LEGACY, id("d1"), role("daemon"), beat("1.0"), secs(999));
    let d = desired("2.0", 1, 1);
    assert_eq!(run(&mut f, &d, &["d1"], secs(999), false), ["d1: nothing"]);
    assert_eq!(
        run(&mut f, &d, &["d1"], secs(1_000), false),
        ["d1: update 2.0"]
    );
}

/// A rollback is an older desired version under the same rules: the first stage holds one host
/// whatever its role, and the minimum of the role holds an order as it holds an update (two
/// storage hosts of a minimum of two go neither way).
#[test]
fn a_rollback_follows_the_same_rules() {
    let mut f = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("2.0")),
            ("s2", role("storage"), beat("2.0")),
            ("s3", role("storage"), beat("2.0")),
            ("f1", role("frontend"), beat("2.0")),
            ("f2", role("frontend"), beat("2.0")),
        ],
    );
    assert_eq!(
        run(
            &mut f,
            &desired("1.0", 2, 1),
            &["s1", "s2", "s3", "f1", "f2"],
            NOW,
            true
        ),
        [
            "s1: update 1.0",
            "s2: nothing",
            "s3: nothing",
            "f1: nothing",
            "f2: nothing"
        ]
    );
    let mut thin = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("2.0")),
            ("s2", role("storage"), beat("2.0")),
        ],
    );
    assert_eq!(
        run(&mut thin, &desired("1.0", 2, 1), &["s1", "s2"], NOW, true),
        ["s1: nothing", "s2: nothing"]
    );
}
