//! The host prerequisites of `prerequisites-v1`: announced and reported only by a daemon that
//! keeps them, handed over only while the coordinator advertises the feature, and never holding
//! the order of the same reply.

use hostkeeper_link::testing::World;
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, FACTS, Feature, Fitted, Heartbeat, LinkError, MAX_HEARTBEAT_BYTES,
    MAX_PREREQUISITE_REASON, MAX_PREREQUISITE_VERSION, Oversize, PRIVATE_REPOSITORY,
    PrerequisiteState, PrerequisiteStatus, PrerequisitesRefusal, PrerequisitesReport,
    PushedPrerequisites, REPOSITORIES, Reply, fit,
};
use serde_json::json;

use crate::{Rig, abort, recorded, reply, update};

const TOOLS: &str = "xcode-command-line-tools";

/// A digest that is only a test's text: 64 lower-case hex digits.
fn digest(digit: char) -> String {
    digit.to_string().repeat(64)
}

fn set(digit: char, names: usize) -> PushedPrerequisites {
    PushedPrerequisites {
        digest: digest(digit),
        names: (0..names).map(|n| format!("name-{n}")).collect(),
    }
}

fn sent(features: &[Feature], names: PushedPrerequisites) -> Reply {
    Reply {
        features: features.to_vec(),
        prerequisites: Some(names),
        ..Reply::default()
    }
}

fn keeping(name: &str) -> Rig {
    Rig::with_world(name, World::default().keeping_prerequisites())
}

fn failed_report() -> PrerequisitesReport {
    PrerequisitesReport {
        digest: digest('a'),
        refused: Some(PrerequisitesRefusal {
            digest: digest('b'),
            reason: "a name is twice in the set".to_owned(),
        }),
        states: vec![
            PrerequisiteState {
                name: TOOLS.to_owned(),
                status: PrerequisiteStatus::Failed,
                version: String::new(),
                reason: "softwareupdate --install exited 1".to_owned(),
                failures: 2,
                next_try_unix: 2_000_003_600,
                seen_unix: 2_000_000_000,
            },
            PrerequisiteState {
                name: "other".to_owned(),
                status: PrerequisiteStatus::Present,
                version: "16.2".to_owned(),
                ..PrerequisiteState::default()
            },
        ],
    }
}

/// Proves a daemon with no prerequisites sink announces exactly [`ANNOUNCED`] and sends no
/// report, and that a reply which advertises `prerequisites-v1` and carries a set changes
/// neither.
#[test]
fn a_host_with_no_prerequisites_announces_none_and_sends_no_report() {
    let rig = Rig::new("prerequisites-none");
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(sent(&[Feature::PrerequisitesV1], set('c', 2))));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats.len(), 2);
    for beat in beats {
        assert_eq!(beat.features, ANNOUNCED.to_vec());
        assert_eq!(beat.prerequisites, None);
        assert_eq!(beat.to_json().get("prerequisites"), None);
    }
    assert!(rig.world.prerequisites.applied().is_empty());
}

/// Proves a daemon with a sink announces `prerequisites-v1` last, after every other feature a
/// daemon may announce, in the documented order.
#[test]
fn a_host_with_prerequisites_announces_the_feature_last() {
    let rig = keeping("prerequisites-announce");
    rig.heartbeater().beat_once();
    let mut features = ANNOUNCED.to_vec();
    features.push(Feature::PrerequisitesV1);
    assert_eq!(rig.world.link.beats()[0].features, features);

    let all = Rig::with_world(
        "prerequisites-announce-all",
        World::default()
            .managing_repositories()
            .keeping_facts()
            .keeping_operator_keys()
            .keeping_prerequisites(),
    );
    all.heartbeater().beat_once();
    let announced = all.world.link.beats()[0].features.clone();
    let names: Vec<&str> = announced.iter().map(|feature| feature.as_str()).collect();
    assert_eq!(
        names[names.len() - 2..],
        ["operator-keys-v1", "prerequisites-v1"]
    );
    assert_eq!(
        announced[ANNOUNCED.len()..],
        [
            REPOSITORIES,
            FACTS,
            PRIVATE_REPOSITORY,
            Feature::OperatorKeysV1,
            Feature::PrerequisitesV1
        ]
    );
    assert!(announced.len() <= hostkeeper_wire::wire::MAX_FEATURES);
}

/// Proves the sink's report is in every heartbeat as the sink words it, and follows the sink
/// when its report changes: first an empty report, then one with a refusal and states.
#[test]
fn the_sinks_report_is_in_every_heartbeat() {
    let rig = keeping("prerequisites-report");
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    rig.world.prerequisites.set_report(failed_report());
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].prerequisites, Some(PrerequisitesReport::default()));
    assert_eq!(beats[1].prerequisites, Some(failed_report()));
    assert_eq!(beats[2].prerequisites, Some(failed_report()));
}

/// Proves a set of a reply that advertises `prerequisites-v1` goes to the sink whole and
/// unchanged, once per reply (the empty set too), and that a reply with no set gives it none.
#[test]
fn a_set_is_given_to_the_sink_once_when_the_reply_advertises_the_feature() {
    let rig = keeping("prerequisites-apply");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(
        &[Feature::HaltV1, Feature::PrerequisitesV1],
        set('d', 2),
    )));
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::PrerequisitesV1])));
    rig.world
        .link
        .answer(Ok(sent(&[Feature::PrerequisitesV1], set('e', 0))));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    assert_eq!(
        rig.world.prerequisites.applied(),
        vec![set('d', 2), set('e', 0)]
    );
}

/// Proves a set whose reply does not advertise `prerequisites-v1` is given to nobody, whether
/// the reply advertises other features (operator keys among them) or none, and that a later
/// advertising reply gives its own.
#[test]
fn a_set_from_a_coordinator_that_does_not_advertise_the_feature_is_ignored() {
    let rig = keeping("prerequisites-unadvertised");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[], set('1', 1))));
    rig.world.link.answer(Ok(sent(
        &[Feature::OperatorKeysV1, Feature::HaltV1],
        set('2', 1),
    )));
    for _ in 0..2 {
        heartbeater.beat_once();
    }
    assert!(rig.world.prerequisites.applied().is_empty());
    rig.world
        .link
        .answer(Ok(sent(&[Feature::PrerequisitesV1], set('3', 1))));
    heartbeater.beat_once();
    assert_eq!(rig.world.prerequisites.applied(), vec![set('3', 1)]);
}

/// Proves a daemon with no sink ignores a set from a coordinator that advertises the feature:
/// the order of that reply is started as for any reply, and the next heartbeat carries no
/// report.
#[test]
fn a_host_with_no_prerequisites_ignores_a_set() {
    let rig = Rig::new("prerequisites-ignored");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let mut answer = sent(&[Feature::PrerequisitesV1], set('f', 1));
    answer.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(answer));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert_eq!(rig.world.link.beats()[1].prerequisites, None);
}

/// Proves a set does not hold the order of its reply the way host facts do: the order is
/// started, an update as well as an abort, and the set is given to the sink once.
/// Catches a loop that treats a set like facts and holds the order of its reply.
#[test]
fn a_set_in_the_reply_of_an_order_does_not_hold_the_order() {
    let rig = keeping("prerequisites-with-order");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let mut with_update = sent(&[Feature::PrerequisitesV1], set('a', 1));
    with_update.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(with_update));
    let mut with_abort = sent(&[Feature::PrerequisitesV1], set('b', 1));
    with_abort.order = Some(abort("a-1", "x"));
    rig.world.link.answer(Ok(with_abort));
    heartbeater.beat_once();
    assert_eq!(rig.world.prerequisites.applied(), vec![set('a', 1)]);
    assert_eq!(rig.world.dispatch.pending(), 1);
    heartbeater.beat_once();
    assert_eq!(
        rig.world.prerequisites.applied(),
        vec![set('a', 1), set('b', 1)]
    );
    assert_eq!(rig.world.dispatch.pending(), 2);
    assert!(
        rig.world
            .log
            .lines()
            .iter()
            .all(|line| !line.contains("waits")),
        "{:?}",
        rig.world.log.lines()
    );
}

/// Proves the heartbeat's JSON carries `prerequisites` with the digest, the refusal (null, or an
/// object of its digest and reason) and each state by the gRPC message's field names with the
/// status by its wire name, exactly, and has no such key when there is no report.
#[test]
fn the_report_is_in_the_json_as_digest_refused_and_states() {
    let rig = keeping("prerequisites-json");
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    rig.world.prerequisites.set_report(failed_report());
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(
        beats[0].to_json().get("prerequisites"),
        Some(&json!({ "digest": "", "refused": null, "states": [] }))
    );
    assert_eq!(
        beats[1].to_json().get("prerequisites"),
        Some(&json!({
            "digest": digest('a'),
            "refused": { "digest": digest('b'), "reason": "a name is twice in the set" },
            "states": [
                {
                    "name": TOOLS,
                    "status": "PREREQUISITE_STATUS_FAILED",
                    "version": "",
                    "reason": "softwareupdate --install exited 1",
                    "failures": 2,
                    "next_try_unix": 2_000_003_600_u64,
                    "seen_unix": 2_000_000_000_u64,
                },
                {
                    "name": "other",
                    "status": "PREREQUISITE_STATUS_PRESENT",
                    "version": "16.2",
                    "reason": "",
                    "failures": 0,
                    "next_try_unix": 0,
                    "seen_unix": 0,
                },
            ],
        }))
    );
    let mut none = beats[0].clone();
    none.prerequisites = None;
    assert_eq!(none.to_json().get("prerequisites"), None);
}

/// Proves the wire name of every status in the JSON, and the limits on a reason and a version
/// the coordinator reads.
#[test]
fn every_status_has_its_wire_name_and_the_limits_are_those_of_the_coordinator() {
    let names = [
        (PrerequisiteStatus::Pending, "PREREQUISITE_STATUS_PENDING"),
        (PrerequisiteStatus::Present, "PREREQUISITE_STATUS_PRESENT"),
        (
            PrerequisiteStatus::Installing,
            "PREREQUISITE_STATUS_INSTALLING",
        ),
        (PrerequisiteStatus::Failed, "PREREQUISITE_STATUS_FAILED"),
        (
            PrerequisiteStatus::NotApplicable,
            "PREREQUISITE_STATUS_NOT_APPLICABLE",
        ),
        (PrerequisiteStatus::Unknown, "PREREQUISITE_STATUS_UNKNOWN"),
    ];
    for (status, name) in names {
        assert_eq!(status.as_str(), name);
    }
    assert_eq!(PrerequisiteStatus::default(), PrerequisiteStatus::Pending);
    assert_eq!(MAX_PREREQUISITE_REASON, 256);
    assert_eq!(MAX_PREREQUISITE_VERSION, 64);
}

/// The first heartbeat of a daemon that keeps prerequisites, with `report` as its report.
fn first_beat(report: Option<PrerequisitesReport>) -> Heartbeat {
    let rig = keeping("prerequisites-bound");
    rig.heartbeater().beat_once();
    let mut beat = rig.world.link.beats()[0].clone();
    beat.prerequisites = report;
    beat
}

/// Proves the report counts toward the 16 KiB bound: the JSON the bound measures grows by the
/// bytes of the report, a report that fits is sent whole, and one that alone is over the bound
/// is refused with the length it has, never cut, and the loop makes no call.
#[test]
fn the_report_counts_toward_the_bound() {
    let plain = first_beat(None);
    let small = first_beat(Some(failed_report()));
    assert!(small.encoded_len() > plain.encoded_len() + 200);
    assert_eq!(
        fit(small.clone()),
        Ok(Fitted {
            beat: small,
            dropped: Vec::new()
        })
    );

    let oversize = PrerequisitesReport {
        states: vec![PrerequisiteState {
            reason: "f".repeat(MAX_HEARTBEAT_BYTES),
            ..PrerequisiteState::default()
        }],
        ..PrerequisitesReport::default()
    };
    let crowded = first_beat(Some(oversize.clone()));
    let counted = crowded.encoded_len();
    assert!(counted > MAX_HEARTBEAT_BYTES);
    assert_eq!(fit(crowded), Err(Oversize::Bytes { len: counted }));

    let rig = keeping("prerequisites-refused");
    rig.world.prerequisites.set_report(oversize);
    assert_eq!(
        rig.heartbeater().beat_once(),
        BeatOutcome::NotSent(LinkError::TooLarge(Oversize::Bytes { len: counted }))
    );
    assert!(rig.world.link.calls().is_empty());
}
