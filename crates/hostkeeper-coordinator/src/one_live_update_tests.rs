//! One rollout live at a time across services (`stages.rs`, `elsewhere`, the coordinator's
//! `--live-rollouts`, default 1): a service whose rollout is not live is held with
//! `rollout_elsewhere` while another service's holds an update slot, bakes or is under way,
//! whatever the desired state's `max_updating` says. Inside a service the stages choose the
//! hosts to order and the role's `max_updating` caps how many of them hold a slot at once
//! (`no_slot`). A restart mid-update resumes under the same rule. Every time is explicit; the
//! fleets are built beat by beat.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_wire::RoleName;
use hostkeeper_wire::wire::{Health, Heartbeat, Outcome, ServiceBeat, ServiceReply};

use crate::decide::{Decision, Gate, Skip, Why, decide_why};
use crate::desired::Desired;
use crate::fleet::{Fleet, Marks};
use crate::identity::Identity;
use crate::rollouts::advance;
use crate::services::Services;
use crate::testutil::{
    Rig, beat, id, kbf_def, last, ops_of, role, scratch, secs, unhealthy, updating, version,
};

/// `a` runs on daemons and bakes for two beats; `b` runs on daemons and storage hosts; `wide` runs
/// on daemons, and the tests roll four of them. None keeps a host up. All have the default plan
/// (1, 10, 50 and 100 percent).
const SERVICES: &str = "[[service]]\nname = \"a\"\nbake_beats = 2\nroles = [\"daemon\"]\n\
                        [[service]]\nname = \"b\"\nroles = [\"daemon\", \"storage\"]\n\
                        [[service]]\nname = \"wide\"\nroles = [\"daemon\"]\n";

fn services() -> Services {
    Services::parse(SERVICES).expect("services")
}

fn want(target: &str) -> Desired {
    let text = format!(r#"{{"version":"{target}","min_up":{{"storage":0,"frontend":0}}}}"#);
    Desired::parse(text.as_bytes()).expect("parsed")
}

/// Like [`want`], with the `max_updating` of the `daemon` role given.
fn want_daemons(target: &str, max_updating: u32) -> Desired {
    let text = format!(
        r#"{{"version":"{target}","max_updating":{{"daemon":{max_updating}}},"min_up":{{"storage":0,"frontend":0}}}}"#
    );
    Desired::parse(text.as_bytes()).expect("parsed")
}

/// Every service of [`SERVICES`] wants 2.0.
fn all() -> BTreeMap<String, Desired> {
    ["a", "b", "wide"]
        .into_iter()
        .map(|s| (s.to_owned(), want("2.0")))
        .collect()
}

fn why(f: &Fleet, all: &BTreeMap<String, Desired>, service: &str, host: &str, at: u64) -> Decision {
    decide_why(&services(), all, service, f, &id(host), secs(at))
}

fn go() -> Decision {
    Decision::Update {
        version: version("2.0"),
    }
}

fn gate(why: Gate) -> Decision {
    Decision::Gate(why)
}

fn skip(why: Skip) -> Decision {
    Decision::Skip(why)
}

/// The decision, noted as an order when it is one, as the coordinator does under its lock.
fn ask(f: &mut Fleet, service: &str, host: &str, at: u64) -> Decision {
    let decision = why(f, &all(), service, host, at);
    if let Decision::Update { version } = &decision {
        f.order(service, id(host), version.clone(), secs(at));
    }
    decision
}

/// [`ask`] for `host` of `wide` as the coordinator answers it: the plan first enters the stages
/// it may enter by itself and keeps the position (`rollouts::advance`, under `ops`), then the
/// decision over the desired states `all`.
fn ask_wide(
    f: &mut Fleet,
    ops: &crate::ops::OpsStore,
    all: &BTreeMap<String, Desired>,
    host: &str,
    at: u64,
) -> Decision {
    let services = services();
    let def = services.get("wide").expect("wide");
    advance(def, &all["wide"], ops, f, secs(at));
    let decision = why(f, all, "wide", host, at);
    if let Decision::Update { version } = &decision {
        f.order("wide", id(host), version.clone(), secs(at));
    }
    decision
}

fn record(f: &mut Fleet, service: &str, host: &str, role: RoleName, b: Heartbeat, at: u64) {
    f.record_in(service, id(host), role, b, secs(at));
}

/// Behaviour: with `a` behind on `d1` and `b` behind on `d2`, the first asked is ordered and
/// the other service waits (`rollout_elsewhere`) through the order, the update and the bake, and
/// is ordered once `d1` has beaten healthy twice at 2.0. The same holds on one host: `d1` behind
/// on `b` waits while it bakes `a`. Once `b` has its own order, `d1` waits for its stage. Catches
/// a rollout counted per service, so two services roll together, and a bake of one service not
/// holding another service's order.
#[test]
fn two_services_with_pending_updates_order_one_host_and_the_second_waits_for_the_first() {
    let elsewhere = gate(Gate::RolloutElsewhere);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    record(&mut f, "a", "d1", role("daemon"), beat("1.0"), 100);
    record(&mut f, "b", "d1", role("daemon"), beat("1.0"), 100);
    record(&mut f, "b", "d2", role("daemon"), beat("1.0"), 100);
    assert_eq!(ask(&mut f, "a", "d1", 100), go());
    assert_eq!(ask(&mut f, "b", "d2", 100), elsewhere, "ordered");

    record(
        &mut f,
        "a",
        "d1",
        role("daemon"),
        updating(beat("1.0"), "2.0"),
        115,
    );
    assert_eq!(ask(&mut f, "b", "d2", 115), elsewhere, "updating");

    record(&mut f, "a", "d1", role("daemon"), beat("2.0"), 130);
    assert_eq!(ask(&mut f, "b", "d2", 130), elsewhere, "one beat");
    assert_eq!(ask(&mut f, "b", "d1", 130), elsewhere, "its own bake");

    record(&mut f, "a", "d1", role("daemon"), beat("2.0"), 145);
    record(&mut f, "b", "d1", role("daemon"), beat("1.0"), 145);
    record(&mut f, "b", "d2", role("daemon"), beat("1.0"), 145);
    assert_eq!(ask(&mut f, "b", "d2", 145), go(), "baked and released");
    assert_eq!(
        ask(&mut f, "b", "d1", 145),
        gate(Gate::StageWait),
        "d2 has the first stage"
    );
}

/// Behaviour: `d1` failed `a` and is unhealthy. It holds the slot of `a`'s rollout, so `a` on
/// another daemon waits for the failed host (`stage_failed`) and `b` on a storage host and on
/// `d1` wait for `a`'s rollout (`rollout_elsewhere`). With `max_updating` of 1 for daemons, only
/// `d1` may be told a new version of the service it failed (its own slot is not counted against
/// it) and `a` on `d2` is held with `no_slot`. Once `d1` heartbeats healthy the failure stands,
/// `a` still has `d2` to roll, and `b` still waits; when an operator acknowledges the failure
/// every service orders again. Catches a failure of one service or role not holding the next
/// order of the others, and a failed host that releases its stage by beating healthy.
#[test]
fn a_failed_unhealthy_host_blocks_every_services_next_order() {
    let failed = unhealthy(last(beat("1.0"), "2.0", Outcome::Failed));
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    record(&mut f, "a", "d1", role("daemon"), failed.clone(), 100);
    record(&mut f, "b", "d1", role("daemon"), beat("1.0"), 100);
    record(&mut f, "a", "d2", role("daemon"), beat("1.0"), 100);
    record(&mut f, "b", "s1", role("storage"), beat("1.0"), 100);
    let blocked = [
        ("a", "d2", Gate::StageFailed),
        ("b", "s1", Gate::RolloutElsewhere),
        ("b", "d1", Gate::RolloutElsewhere),
    ];
    for (service, host, held) in blocked {
        assert_eq!(
            why(&f, &all(), service, host, 100),
            gate(held),
            "{service} on {host}"
        );
    }
    assert_eq!(why(&f, &all(), "a", "d1", 100), skip(Skip::FailedHere));
    let mut newer = all();
    newer.insert("a".to_owned(), want_daemons("3.0", 1));
    assert_eq!(
        why(&f, &newer, "a", "d1", 100),
        Decision::Update {
            version: version("3.0")
        }
    );
    assert_eq!(why(&f, &newer, "a", "d2", 100), gate(Gate::NoSlot));

    let mut healthy = failed;
    healthy.health = Health::Healthy;
    record(&mut f, "a", "d1", role("daemon"), healthy, 115);
    for (service, host, held) in blocked {
        assert_eq!(
            why(&f, &all(), service, host, 115),
            gate(held),
            "{service} on {host}, the failure standing"
        );
    }

    let acked = BTreeSet::from([("d1".to_owned(), "2.0".to_owned())]);
    f.set_marks(Marks {
        acked: BTreeMap::from([("a".to_owned(), acked)]),
        ..Marks::default()
    });
    for (service, host, _) in blocked {
        assert_eq!(
            why(&f, &all(), service, host, 115),
            go(),
            "{service} on {host}, the failure acknowledged"
        );
    }
}

/// Behaviour: four daemons behind roll in stages of one, two and four hosts (the default plan
/// over four hosts). Each stage asks every host, the ones ordered report updating and then
/// done, and the next stage starts: the hosts of the stage that wait are held with
/// `stage_wait`, the first stage orders `d1` alone, the second one more host, the third the last
/// two together (the daemon `max_updating` default is 2). Every host is ordered exactly once.
/// Catches a roll that widens past its stage, or that stays one host at a time.
#[test]
fn one_service_of_four_daemons_rolls_in_stages_of_one_two_and_four_hosts() {
    let hosts = ["d1", "d2", "d3", "d4"];
    let dir = scratch("one-live-update-stages");
    let ops = ops_of(&dir);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    for host in hosts {
        record(&mut f, "wide", host, role("daemon"), beat("1.0"), 100);
    }
    let mut rounds = Vec::new();
    for (round, expected) in [vec!["d1"], vec!["d2"], vec!["d3", "d4"]]
        .into_iter()
        .enumerate()
    {
        let at = 100 + round as u64 * 30;
        let ordered: Vec<&str> = hosts
            .into_iter()
            .filter(|host| ask_wide(&mut f, &ops, &all(), host, at) == go())
            .collect();
        assert_eq!(ordered, expected, "round {round}");
        for host in &ordered {
            let busy = updating(beat("1.0"), "2.0");
            record(&mut f, "wide", host, role("daemon"), busy, at + 10);
        }
        for other in hosts.into_iter().filter(|h| !ordered.contains(h)) {
            let held =
                if f.get_in("wide", &id(other)).expect("known").beat.version == version("2.0") {
                    skip(Skip::UpToDate)
                } else {
                    gate(Gate::StageWait)
                };
            assert_eq!(
                why(&f, &all(), "wide", other, at + 10),
                held,
                "{other} while {ordered:?} update"
            );
        }
        for host in &ordered {
            record(&mut f, "wide", host, role("daemon"), beat("2.0"), at + 20);
        }
        rounds.push(ordered);
    }
    assert_eq!(rounds.concat(), hosts);
}

/// Behaviour: in a stage with several hosts to order, the desired state's `max_updating` of the
/// role caps how many hold a slot of the service. With the first two of four daemons done and
/// the stage of all four entered, a cap of 2 orders `d3` and `d4` together, a cap of 1 orders
/// `d3` and holds `d4` with `no_slot` through the order and the update until `d3` is done, and
/// a cap of 0 holds the role, so neither is ordered. Catches a cap that is not read, that is
/// read for another role or service, or that is ignored by the hosts that ask second.
#[test]
fn max_updating_caps_a_role_within_a_stage() {
    for (cap, first, second) in [
        (2, go(), go()),
        (1, go(), gate(Gate::NoSlot)),
        (0, gate(Gate::NoSlot), gate(Gate::NoSlot)),
    ] {
        let dir = scratch(&format!("one-live-update-cap-{cap}"));
        let ops = ops_of(&dir);
        let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
        for host in ["d1", "d2"] {
            record(&mut f, "wide", host, role("daemon"), beat("2.0"), 100);
        }
        for host in ["d3", "d4"] {
            record(&mut f, "wide", host, role("daemon"), beat("1.0"), 100);
        }
        let mut states = all();
        states.insert("wide".to_owned(), want_daemons("2.0", cap));
        assert_eq!(
            ask_wide(&mut f, &ops, &states, "d3", 100),
            first,
            "d3, {cap}"
        );
        assert_eq!(
            ask_wide(&mut f, &ops, &states, "d4", 100),
            second,
            "d4, {cap}"
        );
        if cap == 1 {
            record(
                &mut f,
                "wide",
                "d3",
                role("daemon"),
                updating(beat("1.0"), "2.0"),
                110,
            );
            let held = ask_wide(&mut f, &ops, &states, "d4", 110);
            assert_eq!(held, gate(Gate::NoSlot), "d4 while d3 updates");
            record(&mut f, "wide", "d3", role("daemon"), beat("2.0"), 120);
            let freed = ask_wide(&mut f, &ops, &states, "d4", 120);
            assert_eq!(freed, go(), "d4 once d3 is done");
        }
    }
}

const RESTART_SERVICES: &str = "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n";

/// The top-level (kbf) word of a host that runs `other` alone: kbf is not installed on it.
fn absent() -> Heartbeat {
    let mut b = beat("2.0");
    b.health = Health::Absent;
    b
}

fn who(host: &str) -> Identity {
    Identity {
        host_id: id(host),
        role: role("daemon"),
    }
}

/// `top`, also running `other` at `running`, updating it to `to` when given.
fn with_other(mut top: Heartbeat, running: &str, to: Option<&str>) -> Heartbeat {
    top.services = Some(vec![ServiceBeat {
        name: "other".to_owned(),
        version: version(running),
        health: Health::Healthy,
        last_update: None,
        updating: to.map(|t| updating(beat(running), t).updating.expect("updating")),
        step: None,
        commit: None,
        step_secs: Default::default(),
        promotion_pending: false,
        deferred: None,
    }]);
    top
}

fn start(rig: &Rig) -> crate::service::Coordinator {
    let list = config_text(STATIC_ISSUER) + "services = [\"kbf\", \"other\"]\n";
    Rig::start_with_allow_list(&rig.dir, &rig.clock, &list).with_services(
        Services::parse(RESTART_SERVICES)
            .expect("services")
            .with(kbf_def()),
    )
}

/// Behaviour: `d1`, which runs `other` alone (kbf is not installed on it), is ordered `other`
/// and reports updating it, so the rollout of `other` is live and `d2`, behind on kbf, is
/// held. The coordinator restarts mid-update and forgets the order. After the quiet start `d2`
/// is still held (`rollout_elsewhere`), because `d1`'s beats in the quiet period said it is
/// updating; once `d1` reports `other` done, `d2` is ordered kbf. Catches a restarted coordinator
/// counting rollouts per service again, or ordering on what it forgot.
#[tokio::test]
async fn a_restart_mid_update_resumes_with_the_one_update_rule() {
    let mut rig = Rig::new("one-live-update-restart");
    rig.coordinator = start(&rig);
    for body in [
        r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#,
        r#"{"service":"other","version":"3.0","min_up":{"storage":0,"frontend":0}}"#,
    ] {
        rig.set(body).await;
    }
    let kbf_order = ServiceReply::Update {
        service: None,
        version: version("2.0"),
    };
    let beat_at = |rig: &Rig, at: u64, host: &str, b: Heartbeat| {
        rig.at(at);
        rig.coordinator
            .heartbeat(who(host), b)
            .expect("a heartbeat")
            .reply
    };

    let first = beat_at(&rig, 2_000, "d1", with_other(absent(), "1.0", None));
    assert_eq!(
        first,
        ServiceReply::Update {
            service: Some("other".to_owned()),
            version: version("3.0"),
        }
    );
    let d1_updating = with_other(absent(), "1.0", Some("3.0"));
    beat_at(&rig, 2_010, "d1", d1_updating.clone());
    let before = beat_at(&rig, 2_010, "d2", beat("1.0"));
    assert_eq!(before, ServiceReply::Nothing, "held before the restart");

    rig.at(2_020);
    rig.coordinator = start(&rig);
    beat_at(&rig, 2_035, "d1", d1_updating.clone());
    let quiet = beat_at(&rig, 2_035, "d2", beat("1.0"));
    assert_eq!(quiet, ServiceReply::Nothing, "the quiet start");

    beat_at(&rig, 2_050, "d1", d1_updating);
    let resumed = beat_at(&rig, 2_050, "d2", beat("1.0"));
    assert_eq!(resumed, ServiceReply::Nothing, "held after the restart");
    let status = rig
        .coordinator
        .status(Some(crate::testutil::OPERATOR))
        .expect("status");
    let hold = status.per_service["kbf"]
        .hosts
        .iter()
        .find(|h| h.host.node_id == "d2")
        .and_then(|h| h.hold);
    assert_eq!(hold, Some(Why::Gate(Gate::RolloutElsewhere)));

    beat_at(&rig, 2_065, "d1", with_other(absent(), "3.0", None));
    let released = beat_at(&rig, 2_065, "d2", beat("1.0"));
    assert_eq!(released, kbf_order, "ordered once the update is done");
}
