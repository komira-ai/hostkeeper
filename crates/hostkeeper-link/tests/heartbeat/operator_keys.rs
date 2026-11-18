//! The operator keys of `operator-keys-v1`: announced and reported only by a daemon that keeps
//! them, handed over only while the coordinator advertises the feature, and never holding the
//! order of the same reply.

use hostkeeper_link::testing::World;
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, FACTS, Feature, Fitted, Heartbeat, KeysRefusal, KeysReport, LinkError,
    MAX_HEARTBEAT_BYTES, Oversize, PRIVATE_REPOSITORY, PushedKeys, REPOSITORIES, Reply, fit,
};
use serde_json::json;

use crate::{Rig, abort, recorded, reply, update};

/// A digest that is only a test's text: 64 lower-case hex digits.
fn digest(digit: char) -> String {
    digit.to_string().repeat(64)
}

/// One key line whose blob is made here, so that no key material is committed.
fn key(comment: &str) -> String {
    format!("ssh-ed25519 {} {comment}", "A".repeat(68))
}

fn set(digit: char, keys: usize) -> PushedKeys {
    PushedKeys {
        digest: digest(digit),
        keys: (0..keys).map(|n| key(&format!("ops laptop {n}"))).collect(),
    }
}

fn sent(features: &[Feature], keys: PushedKeys) -> Reply {
    Reply {
        features: features.to_vec(),
        operator_keys: Some(keys),
        ..Reply::default()
    }
}

fn keeping(name: &str) -> Rig {
    Rig::with_world(name, World::default().keeping_operator_keys())
}

fn refusing() -> KeysReport {
    KeysReport {
        digest: digest('a'),
        refused: Some(KeysRefusal {
            digest: digest('b'),
            reason: "key 2 is not an ssh public key".to_owned(),
        }),
        failed: "the file could not be written".to_owned(),
    }
}

/// Proves a daemon with no operator-keys sink announces exactly [`ANNOUNCED`] and sends no
/// report, and that a reply which advertises `operator-keys-v1` and carries a set changes
/// neither.
#[test]
fn a_host_with_no_operator_keys_announces_none_and_sends_no_report() {
    let rig = Rig::new("operator-keys-none");
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(sent(&[Feature::OperatorKeysV1], set('c', 2))));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats.len(), 2);
    for beat in beats {
        assert_eq!(beat.features, ANNOUNCED.to_vec());
        assert_eq!(beat.operator_keys, None);
        assert_eq!(beat.to_json().get("operator_keys"), None);
    }
    assert!(rig.world.operator_keys.applied().is_empty());
}

/// Proves a daemon with a sink announces `operator-keys-v1` last, after every other feature a
/// daemon may announce, and that the whole list of a daemon that announces them all is in the
/// documented order.
#[test]
fn a_host_with_operator_keys_announces_the_feature_last() {
    let rig = keeping("operator-keys-announce");
    rig.heartbeater().beat_once();
    let mut features = ANNOUNCED.to_vec();
    features.push(Feature::OperatorKeysV1);
    assert_eq!(rig.world.link.beats()[0].features, features);

    let all = Rig::with_world(
        "operator-keys-announce-all",
        World::default()
            .managing_repositories()
            .keeping_facts()
            .keeping_operator_keys(),
    );
    all.heartbeater().beat_once();
    let announced = all.world.link.beats()[0].features.clone();
    let names: Vec<&str> = announced.iter().map(|feature| feature.as_str()).collect();
    assert_eq!(
        names,
        [
            "deferred-v1",
            "halt-v1",
            "heartbeat-v2",
            "forced-v1",
            "order-v1",
            "verdict-acks-v1",
            "grpc-v1",
            "install-v1",
            "abort-v1",
            "clear-record-v1",
            "target-v1",
            "repositories-v1",
            "facts-v1",
            "private-repository-v1",
            "operator-keys-v1",
        ]
    );
    assert_eq!(
        announced[ANNOUNCED.len()..],
        [
            REPOSITORIES,
            FACTS,
            PRIVATE_REPOSITORY,
            Feature::OperatorKeysV1
        ]
    );
}

/// Proves the sink's report is in every heartbeat as the sink words it, and follows the sink
/// when its report changes: first an empty report, then one with a refusal and a failure.
#[test]
fn the_sinks_report_is_in_every_heartbeat() {
    let rig = keeping("operator-keys-report");
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    rig.world.operator_keys.set_report(refusing());
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].operator_keys, Some(KeysReport::default()));
    assert_eq!(beats[1].operator_keys, Some(refusing()));
    assert_eq!(beats[2].operator_keys, Some(refusing()));
}

/// Proves a set of a reply that advertises `operator-keys-v1` goes to the sink whole and
/// unchanged, once per reply (the empty set too), and that a reply with no set gives it none.
#[test]
fn a_set_is_given_to_the_sink_once_when_the_reply_advertises_the_feature() {
    let rig = keeping("operator-keys-apply");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(
        &[Feature::HaltV1, Feature::OperatorKeysV1],
        set('d', 2),
    )));
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::OperatorKeysV1])));
    rig.world
        .link
        .answer(Ok(sent(&[Feature::OperatorKeysV1], set('e', 0))));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    assert_eq!(
        rig.world.operator_keys.applied(),
        vec![set('d', 2), set('e', 0)]
    );
}

/// Proves a set whose reply does not advertise `operator-keys-v1` is given to nobody, whether
/// the reply advertises other features or none, and that a later advertising reply gives its own.
#[test]
fn a_set_from_a_coordinator_that_does_not_advertise_the_feature_is_ignored() {
    let rig = keeping("operator-keys-unadvertised");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[], set('1', 1))));
    rig.world
        .link
        .answer(Ok(sent(&[Feature::FactsV1, Feature::HaltV1], set('2', 1))));
    for _ in 0..2 {
        heartbeater.beat_once();
    }
    assert!(rig.world.operator_keys.applied().is_empty());
    rig.world
        .link
        .answer(Ok(sent(&[Feature::OperatorKeysV1], set('3', 1))));
    heartbeater.beat_once();
    assert_eq!(rig.world.operator_keys.applied(), vec![set('3', 1)]);
}

/// Proves a daemon with no sink ignores a set from a coordinator that advertises the feature:
/// the order of that reply is started as for any reply, and the next heartbeat carries no
/// report.
#[test]
fn a_host_with_no_operator_keys_ignores_a_set() {
    let rig = Rig::new("operator-keys-ignored");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let mut answer = sent(&[Feature::OperatorKeysV1], set('f', 1));
    answer.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(answer));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert_eq!(rig.world.link.beats()[1].operator_keys, None);
}

/// Proves a set does not hold the order of its reply the way host facts do: the order is
/// started, an update as well as an abort, and the set is given to the sink once.
/// Catches a loop that treats a set like facts and holds the order of its reply.
#[test]
fn a_set_in_the_reply_of_an_order_does_not_hold_the_order() {
    let rig = keeping("operator-keys-with-order");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let mut with_update = sent(&[Feature::OperatorKeysV1], set('a', 1));
    with_update.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(with_update));
    let mut with_abort = sent(&[Feature::OperatorKeysV1], set('b', 1));
    with_abort.order = Some(abort("a-1", "x"));
    rig.world.link.answer(Ok(with_abort));
    heartbeater.beat_once();
    assert_eq!(rig.world.operator_keys.applied(), vec![set('a', 1)]);
    assert_eq!(rig.world.dispatch.pending(), 1);
    heartbeater.beat_once();
    assert_eq!(
        rig.world.operator_keys.applied(),
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

/// Proves the heartbeat's JSON carries `operator_keys` with the digest, the refusal (null, or an
/// object of its digest and reason) and the failure, exactly, and has no such key when there is
/// no report.
#[test]
fn the_report_is_in_the_json_as_digest_refused_and_failed() {
    let rig = keeping("operator-keys-json");
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    rig.world.operator_keys.set_report(refusing());
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(
        beats[0].to_json().get("operator_keys"),
        Some(&json!({ "digest": "", "refused": null, "failed": "" }))
    );
    assert_eq!(
        beats[1].to_json().get("operator_keys"),
        Some(&json!({
            "digest": digest('a'),
            "refused": { "digest": digest('b'), "reason": "key 2 is not an ssh public key" },
            "failed": "the file could not be written",
        }))
    );
    let mut none = beats[0].clone();
    none.operator_keys = None;
    assert_eq!(none.to_json().get("operator_keys"), None);
}

/// The first heartbeat of a daemon that keeps operator keys, with `report` as its report.
fn first_beat(report: Option<KeysReport>) -> Heartbeat {
    let rig = keeping("operator-keys-bound");
    rig.heartbeater().beat_once();
    let mut beat = rig.world.link.beats()[0].clone();
    beat.operator_keys = report;
    beat
}

/// Proves the report counts toward the 16 KiB bound: the JSON the bound measures grows by the
/// bytes of the report, a report that fits is sent whole, and one that alone is over the bound
/// is refused with the length it has, never cut, and the loop makes no call.
#[test]
fn the_report_counts_toward_the_bound() {
    let plain = first_beat(None);
    let small = first_beat(Some(refusing()));
    assert!(small.encoded_len() > plain.encoded_len() + 100);
    assert_eq!(
        fit(small.clone()),
        Ok(Fitted {
            beat: small,
            dropped: Vec::new()
        })
    );

    let oversize = KeysReport {
        failed: "f".repeat(MAX_HEARTBEAT_BYTES),
        ..KeysReport::default()
    };
    let crowded = first_beat(Some(oversize.clone()));
    let counted = crowded.encoded_len();
    assert!(counted > MAX_HEARTBEAT_BYTES);
    assert_eq!(fit(crowded), Err(Oversize::Bytes { len: counted }));

    let rig = keeping("operator-keys-refused");
    rig.world.operator_keys.set_report(oversize);
    assert_eq!(
        rig.heartbeater().beat_once(),
        BeatOutcome::NotSent(LinkError::TooLarge(Oversize::Bytes { len: counted }))
    );
    assert!(rig.world.link.calls().is_empty());
}
