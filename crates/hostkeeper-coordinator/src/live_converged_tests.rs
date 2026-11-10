//! A converged service holds no other service's rollout (`live.rs`): a bake counts only for a
//! host of the plan that the coordinator saw update, so a host the release picks no package for,
//! an excluded or silent host, and a host first heard after a restart already at the version
//! never make a service live; an order to a host that goes silent frees the slot when it ages
//! out; and `Releases.Status` names why each service that holds another is live, and the host.
//! The fleets are built beat by beat, every time explicit, over real coordinator state.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::{Health, Heartbeat};

use crate::decide::{Decision, Gate, Skip};
use crate::desired::Desired;
use crate::fleet::{Fleet, Marks, Node, ORDER_AGE};
use crate::live::{Live, Why, why_live};
use crate::marks_support::native_targets;
use crate::picks::{Heard, TARGET_FEATURE};
use crate::rollouts::plan_proto;
use crate::services::Services;
use crate::testutil::{beat, id, role, secs, unhealthy, updating, version};

/// `hello` runs on the daemons and the Macs and bakes for two beats, as its example entry does;
/// `hostkeeper` runs on the daemons.
const SERVICES: &str = "[[service]]\nname = \"hello\"\nbake_beats = 2\n\
                        roles = [\"daemon\", \"mac-daemon\"]\n\
                        [[service]]\nname = \"hostkeeper\"\nroles = [\"daemon\"]\n";

const LINUX: [&str; 6] = [
    "linux-1", "linux-2", "linux-3", "linux-4", "linux-5", "linux-6",
];

fn services() -> Services {
    Services::parse(SERVICES).expect("services")
}

fn want(target: &str) -> Desired {
    let text = format!(
        r#"{{"version":"{target}","max_updating":{{"daemon":2,"mac-daemon":2}},"min_up":{{"daemon":0,"mac-daemon":0}}}}"#
    );
    Desired::parse(text.as_bytes()).expect("parsed")
}

/// `hello` wants 0.1.92 and `hostkeeper` 0.1.245.
fn all() -> BTreeMap<String, Desired> {
    BTreeMap::from([
        ("hello".to_owned(), want("0.1.92")),
        ("hostkeeper".to_owned(), want("0.1.245")),
    ])
}

/// Keeps a release of `service` at its desired version that lists one `apt` entry per platform:
/// a service with no release kept picks every host nothing (`no_build_for_host`).
fn keep_release(f: &mut Fleet, service: &str) {
    let release = crate::releases::Release::new(native_targets(service, 1), None);
    f.picks
        .keep_one(service, &all()[service].version, Arc::new(release));
}

/// Notes the node `host` of `role` on `platform` as its heartbeat at `at` says it: it lists
/// `services`, healthy, runs each with `apt`, and its daemon lists the installers `installers`
/// and takes typed orders and installs.
fn hear(
    f: &mut Fleet,
    (host, role, platform): (&str, &str, Platform),
    services: &[&str],
    installers: &[&str],
    at: u64,
) {
    let node = Node {
        role: role.to_owned(),
        features: vec![
            TARGET_FEATURE.to_owned(),
            crate::membership::HEARTBEAT_V2.to_owned(),
            crate::orders::INSTALL_FEATURE.to_owned(),
        ],
        agent: None,
        platform,
        seen: secs(at),
        announced: services
            .iter()
            .map(|s| ((*s).to_owned(), Health::Healthy))
            .collect(),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root: crate::trusted_root::TrustedRootSaid::default(),
        skewed_beats: 0,
    };
    let request = agent::HeartbeatRequest {
        features: vec![TARGET_FEATURE.to_owned()],
        installers: installers.iter().map(|i| (*i).to_owned()).collect(),
        services: services
            .iter()
            .map(|s| agent::ServiceBeat {
                name: (*s).to_owned(),
                installer: Some("apt".to_owned()),
                ..agent::ServiceBeat::default()
            })
            .collect(),
        ..agent::HeartbeatRequest::default()
    };
    f.picks
        .hear(id(host), Heard::of(&request).expect("a good list"));
    f.note_node(id(host), node, 0);
}

/// A Linux daemon `host` beats `hello` as `hello` says and `hostkeeper` at 0.1.244 at `at`.
fn linux(f: &mut Fleet, host: &str, hello: Heartbeat, at: u64) {
    hear(
        f,
        (host, "daemon", Platform::LinuxX86_64),
        &["hello", "hostkeeper"],
        &["apt"],
        at,
    );
    f.record_in("hello", id(host), role("daemon"), hello, secs(at));
    f.record_in(
        "hostkeeper",
        id(host),
        role("daemon"),
        beat("0.1.244"),
        secs(at),
    );
}

fn why(f: &Fleet, service: &str, at: u64) -> Option<Live> {
    let s = services();
    why_live(
        s.get(service).expect("the service"),
        all().get(service),
        f,
        secs(at),
    )
}

fn plan(f: &Fleet, service: &str, at: u64) -> proto::RolloutPlan {
    let s = services();
    let all = all();
    let def = s.get(service).expect("the service");
    plan_proto(&s, &all, def, &all[service], false, f, secs(at))
}

fn decide(f: &Fleet, service: &str, host: &str, at: u64) -> Decision {
    crate::decide::decide_why(&services(), &all(), service, f, &id(host), secs(at))
}

fn baking(host: &str) -> Option<Live> {
    Some(Live {
        why: Why::Baking,
        host: Some(id(host)),
    })
}

fn live_on(service: &str, reason: proto::LiveReason, host: &str) -> proto::LiveRollout {
    proto::LiveRollout {
        service: service.to_owned(),
        reason: reason.into(),
        node_id: host.to_owned(),
    }
}

/// Behaviour, the shape of the incident: every Linux daemon was seen updating `hello` to 0.1.92
/// and has run it since, one of them unhealthy at it for an hour (so its bake never counts);
/// the version has no release kept, so the release picks every host nothing and `hello`'s plan
/// has no stage and no eligible host; both Macs are excluded from `hello` and went silent after
/// one beat. `hello` is not live, `hostkeeper`'s plan waits on nothing and its hosts are
/// ordered. Catches a host outside the plan (picked no package), an excluded host and a silent
/// host whose bake makes a converged service hold the farm's one live rollout.
#[test]
fn a_service_up_to_date_on_every_live_host_holds_no_slot() {
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    keep_release(&mut f, "hostkeeper");
    f.set_marks(Marks {
        excluded: BTreeSet::from([
            ("hello".to_owned(), "mac-1".to_owned()),
            ("hello".to_owned(), "mac-2".to_owned()),
        ]),
        ..Marks::default()
    });
    for mac in ["mac-1", "mac-2"] {
        hear(
            &mut f,
            (mac, "mac-daemon", Platform::DarwinArm64),
            &["hello"],
            &["pkg"],
            100,
        );
        f.record_outside_in("hello", id(mac), "mac-daemon", beat("0.1.91"), secs(100));
    }
    for host in LINUX {
        linux(&mut f, host, updating(beat("0.1.91"), "0.1.92"), 100);
    }
    let mut at = 115;
    while at <= 3_700 {
        for host in LINUX {
            let hello = match host {
                "linux-6" => unhealthy(beat("0.1.92")),
                _ => beat("0.1.92"),
            };
            linux(&mut f, host, hello, at);
        }
        at += 15;
    }
    let now = 3_700;
    assert_eq!(
        crate::picks::skip(&f, "hello", &version("0.1.92"), &id("linux-6")),
        Some(Skip::NoBuildForHost)
    );
    assert_eq!(
        decide(&f, "hello", "linux-6", now),
        Decision::Skip(Skip::UpToDate)
    );
    let hello = plan(&f, "hello", now);
    assert_eq!((hello.stages.len(), hello.eligible), (0, 0), "no stage");
    assert_eq!(why(&f, "hello", now), None);
    let hostkeeper = plan(&f, "hostkeeper", now);
    assert_eq!(
        (hostkeeper.waiting_on, hostkeeper.waiting_on_why),
        (vec![], vec![])
    );
    assert_ne!(
        hostkeeper.waiting,
        i32::from(proto::PlanWait::RolloutElsewhere)
    );
    assert_eq!(
        decide(&f, "hostkeeper", "linux-1", now),
        Decision::Update {
            version: version("0.1.245")
        }
    );
}

/// Behaviour: `hello` has a release and three hosts that updated to 0.1.92 and baked; `linux-5`
/// reports a staged copy of 0.1.92 (`absent`: installed, never registered), and its daemon
/// lists no installer of the release, so the release picks it nothing (`no_build_for_host`). Its
/// beats never count a bake, and it does not make `hello` live: `hostkeeper` is ordered. While
/// the same host is one the release picks and the plan counts, healthy and seen updating, its
/// bake holds `hostkeeper` with the reason and the host named. Catches a host with no package for
/// the service keeping the service's rollout live for as long as it stays so.
#[test]
fn a_host_the_release_picks_no_package_for_holds_no_slot() {
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    keep_release(&mut f, "hello");
    keep_release(&mut f, "hostkeeper");
    for host in ["linux-1", "linux-2", "linux-3"] {
        linux(&mut f, host, updating(beat("0.1.91"), "0.1.92"), 100);
        for at in [115, 130, 145] {
            linux(&mut f, host, beat("0.1.92"), at);
        }
    }
    let mut staged = beat("0.1.92");
    staged.health = Health::Absent;
    for at in [100, 115, 130, 145] {
        hear(
            &mut f,
            ("linux-5", "daemon", Platform::LinuxX86_64),
            &["hello", "hostkeeper"],
            &["rpm"],
            at,
        );
        f.record_in(
            "hello",
            id("linux-5"),
            role("daemon"),
            staged.clone(),
            secs(at),
        );
        f.record_in(
            "hostkeeper",
            id("linux-5"),
            role("daemon"),
            beat("0.1.244"),
            secs(at),
        );
    }
    assert_eq!(
        crate::picks::skip(&f, "hello", &version("0.1.92"), &id("linux-5")),
        Some(Skip::NoBuildForHost)
    );
    assert_eq!(f.bake_beats("hello", &id("linux-5"), &version("0.1.92")), 0);
    assert_eq!(why(&f, "hello", 145), None);
    assert_eq!(
        decide(&f, "hostkeeper", "linux-1", 145),
        Decision::Update {
            version: version("0.1.245")
        }
    );

    linux(&mut f, "linux-5", updating(beat("0.1.91"), "0.1.92"), 160);
    linux(&mut f, "linux-5", beat("0.1.92"), 175);
    assert_eq!(why(&f, "hello", 175), baking("linux-5"));
    assert_eq!(
        decide(&f, "hostkeeper", "linux-1", 175),
        Decision::Gate(Gate::RolloutElsewhere)
    );
    let held = plan(&f, "hostkeeper", 175);
    assert_eq!(
        (held.waiting_on, held.waiting_on_why),
        (
            vec!["hello".to_owned()],
            vec![live_on("hello", proto::LiveReason::Baking, "linux-5")]
        )
    );
}

/// Behaviour: the coordinator orders `hello` to `linux-1` at 200 s (an update of a host that runs
/// it) and to `linux-2` at 300 s (an install, of a host with no word of `hello`), and neither
/// host beats again. Each order holds `hostkeeper` (`waiting_on_why` names it `ordered` on its
/// host) until it is [`ORDER_AGE`] old, the last second included, and frees the slot at that
/// age. Catches an order to a host that went silent holding the farm's rollout for ever.
#[test]
fn an_order_to_a_host_that_goes_silent_frees_the_slot_at_its_age() {
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    keep_release(&mut f, "hello");
    keep_release(&mut f, "hostkeeper");
    linux(&mut f, "linux-1", beat("0.1.91"), 200);
    f.order("hello", id("linux-1"), version("0.1.92"), secs(200));
    let age = ORDER_AGE.as_secs();
    let cases = [("linux-1", 200), ("linux-2", 300)];
    for (host, at) in cases {
        if host == "linux-2" {
            f.order("hello", id(host), version("0.1.92"), secs(at));
        }
        let last = at + age - 1;
        linux(&mut f, "linux-3", beat("0.1.92"), last);
        assert_eq!(
            why(&f, "hello", last),
            Some(Live {
                why: Why::Ordered,
                host: Some(id(host)),
            }),
            "{host} at {last}"
        );
        assert_eq!(
            plan(&f, "hostkeeper", last).waiting_on_why,
            vec![live_on("hello", proto::LiveReason::Ordered, host)],
            "{host} at {last}"
        );
        let aged = at + age;
        linux(&mut f, "linux-3", beat("0.1.92"), aged);
        assert_ne!(
            why(&f, "hello", aged).and_then(|l| l.host),
            Some(id(host)),
            "{host} at {aged}"
        );
    }
    assert_eq!(why(&f, "hello", 300 + age), None);
}

/// Behaviour: a restarted coordinator (a new fleet) hears every host of `hello` already at
/// 0.1.92, healthy: `hello` is not live on the first beat, nor through the two beats of its bake,
/// and `hostkeeper` is ordered; nor does a host first heard unhealthy at the version make it
/// live. A host it hears updating to 0.1.92 and then running it does: `hello` is live, baking on
/// that host, until it has beaten healthy twice. A host first heard at the version that is then
/// ordered and updates, or is seen at another version, and runs 0.1.92 again bakes like any
/// other. Catches a
/// restart that makes a converged service live again for its bake, and a host found at a version
/// whose later move is not seen.
#[test]
fn a_restart_does_not_make_a_converged_service_live() {
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    keep_release(&mut f, "hello");
    keep_release(&mut f, "hostkeeper");
    for at in [100, 115, 130] {
        for host in LINUX {
            let hello = match host {
                "linux-6" => unhealthy(beat("0.1.92")),
                _ => beat("0.1.92"),
            };
            linux(&mut f, host, hello, at);
        }
        assert_eq!(why(&f, "hello", at), None, "at {at}");
        assert_eq!(
            decide(&f, "hostkeeper", "linux-1", at),
            Decision::Update {
                version: version("0.1.245")
            },
            "at {at}"
        );
    }

    let mut moved = f.clone();
    linux(
        &mut moved,
        "linux-7",
        updating(beat("0.1.91"), "0.1.92"),
        145,
    );
    linux(&mut moved, "linux-7", beat("0.1.92"), 160);
    assert_eq!(why(&moved, "hello", 160), baking("linux-7"));
    linux(&mut moved, "linux-7", beat("0.1.92"), 175);
    assert_eq!(why(&moved, "hello", 175), None, "baked");

    let mut ordered = f.clone();
    ordered.order("hello", id("linux-2"), version("0.1.92"), secs(145));
    linux(
        &mut ordered,
        "linux-2",
        updating(beat("0.1.92"), "0.1.92"),
        160,
    );
    linux(&mut ordered, "linux-2", beat("0.1.92"), 175);
    assert_eq!(
        why(&ordered, "hello", 175),
        baking("linux-2"),
        "after an order"
    );
    let mut back = f;
    linux(&mut back, "linux-3", beat("0.1.91"), 145);
    linux(&mut back, "linux-3", beat("0.1.92"), 160);
    assert_eq!(why(&back, "hello", 160), baking("linux-3"), "after 0.1.91");
}

/// Behaviour: `hostkeeper`'s plan names, for the one service that holds it, why it is live and on
/// which host, in each way a rollout is live: `linux-1` updating, its promotion pending, under
/// an order the fleet has no word of, baking after an update, and a plan under way (`linux-1`
/// baked, `linux-2` left to order), which names no host. Catches a reason or a host left out of
/// `Releases.Status`, or given for the wrong one.
#[test]
fn status_names_why_a_service_is_live_and_on_which_host() {
    let base = || {
        let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
        keep_release(&mut f, "hello");
        keep_release(&mut f, "hostkeeper");
        linux(&mut f, "linux-1", beat("0.1.91"), 85);
        f
    };
    let mut pending = beat("0.1.92");
    pending.promotion_pending = true;
    let mut cases: Vec<(&str, Fleet, proto::LiveReason, &str)> = Vec::new();
    for (case, hello, reason) in [
        (
            "updating",
            updating(beat("0.1.91"), "0.1.92"),
            proto::LiveReason::Updating,
        ),
        ("pending", pending, proto::LiveReason::PromotionPending),
        ("baking", beat("0.1.92"), proto::LiveReason::Baking),
    ] {
        let mut f = base();
        linux(&mut f, "linux-1", hello, 100);
        cases.push((case, f, reason, "linux-1"));
    }
    let mut unseen = base();
    unseen.order("hello", id("linux-9"), version("0.1.92"), secs(100));
    linux(&mut unseen, "linux-1", beat("0.1.92"), 100);
    linux(&mut unseen, "linux-1", beat("0.1.92"), 100);
    cases.push(("ordered", unseen, proto::LiveReason::Ordered, "linux-9"));
    let mut under_way = base();
    for at in [100, 115, 130] {
        linux(&mut under_way, "linux-1", beat("0.1.92"), at);
        linux(&mut under_way, "linux-2", beat("0.1.91"), at);
    }
    cases.push(("plan", under_way, proto::LiveReason::PlanUnderWay, ""));
    for (case, mut f, reason, host) in cases {
        linux(&mut f, "linux-3", beat("0.1.92"), 130);
        let held = plan(&f, "hostkeeper", 130);
        assert_eq!(
            (held.waiting, held.waiting_on, held.waiting_on_why),
            (
                i32::from(proto::PlanWait::RolloutElsewhere),
                vec!["hello".to_owned()],
                vec![live_on("hello", reason, host)]
            ),
            "{case}"
        );
    }
}
