//! The host's facts of a reply and the generation a heartbeat reports: announced and written only
//! by a daemon that keeps a facts file, written only while the coordinator advertises
//! `facts-v1`, and written before the order of the same reply is acted on.

use std::collections::BTreeMap;

use hostkeeper_link::testing::World;
use hostkeeper_link::{ANNOUNCED, FACTS, Feature, PushedFacts, REPOSITORIES, Reply};

use crate::{Rig, abort, recorded, reply, update};

fn facts(generation: u64) -> PushedFacts {
    PushedFacts {
        generation,
        host_id: "web-04".to_owned(),
        host_group: "web".to_owned(),
        labels: BTreeMap::from([("rack".to_owned(), "r12".to_owned())]),
    }
}

fn sent(features: &[Feature], generation: u64) -> Reply {
    Reply {
        features: features.to_vec(),
        facts: Some(facts(generation)),
        ..Reply::default()
    }
}

fn keeping(name: &str) -> Rig {
    Rig::with_world(name, World::default().keeping_facts())
}

/// Proves a daemon with no facts file announces exactly [`ANNOUNCED`], reports no generation, and
/// writes no facts, even from a reply that advertises `facts-v1` and carries them.
#[test]
fn a_host_with_no_facts_file_announces_none_and_reports_no_generation() {
    let rig = Rig::new("host-facts-none");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[Feature::FactsV1], 3)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].features, ANNOUNCED.to_vec());
    assert_eq!(beats[0].facts_generation, None);
    assert_eq!(beats[1].facts_generation, None);
    assert!(rig.world.facts.applied().is_empty());
}

/// Proves a daemon with a facts file announces `facts-v1` after the others (after
/// `repositories-v1` when it announces that too), reports no generation before its file holds
/// any, and from the heartbeat after a write reports the generation it wrote.
#[test]
fn a_host_with_a_facts_file_announces_the_feature_and_reports_what_it_wrote() {
    let rig = keeping("host-facts-report");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[Feature::FactsV1], 7)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    let mut features = ANNOUNCED.to_vec();
    features.push(FACTS);
    assert_eq!(beats[0].features, features);
    assert_eq!(beats[0].facts_generation, None);
    assert_eq!(beats[1].facts_generation, Some(7));

    let both = Rig::with_world(
        "host-facts-and-repositories",
        World::default().managing_repositories().keeping_facts(),
    );
    let mut heartbeater = both.heartbeater();
    heartbeater.beat_once();
    let mut features = ANNOUNCED.to_vec();
    features.extend([REPOSITORIES, FACTS]);
    assert_eq!(both.world.link.beats()[0].features, features);
}

/// Proves the facts of a reply go to the facts file only while that reply advertises `facts-v1`,
/// whole and unchanged, once per reply, and that a later reply without facts writes none.
#[test]
fn facts_are_written_only_when_the_reply_advertises_the_feature() {
    let rig = keeping("host-facts-apply");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[], 1)));
    rig.world
        .link
        .answer(Ok(sent(&[Feature::HaltV1, Feature::FactsV1], 2)));
    rig.world.link.answer(Ok(reply(None, &[Feature::FactsV1])));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    assert_eq!(rig.world.facts.applied(), vec![(facts(2), 0)]);
    let beats = rig.world.link.beats();
    assert_eq!(
        beats[1].facts_generation, None,
        "facts not advertised were dropped"
    );
    assert_eq!(beats[2].facts_generation, Some(2));
}

/// Proves the facts of a reply that also carries an order are written before the order is acted
/// on: when the file took them, no job was started yet, and the order was started after.
/// Catches a loop that starts the order first, so a config package's cutover could read the
/// facts before them.
#[test]
fn facts_are_written_before_the_order_of_the_same_reply() {
    let rig = keeping("host-facts-before-order");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let mut answer = sent(&[Feature::FactsV1], 4);
    answer.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(answer));
    heartbeater.beat_once();
    assert_eq!(
        rig.world.facts.applied(),
        vec![(facts(4), 0)],
        "the file took the facts while no job had been started"
    );
    assert_eq!(
        rig.world.dispatch.pending(),
        1,
        "then the order was started"
    );
}

/// Proves that when the facts file cannot take the facts of a reply, the order of that reply is
/// not started and the loop says why, the generation reported stays the one before, and the next
/// reply, whose write succeeds, has its order started.
#[test]
fn an_order_waits_while_its_facts_cannot_be_written() {
    let rig = keeping("host-facts-unwritten");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world.facts.set_generation(Some(3));
    rig.world.facts.set_failing(true);
    let mut heartbeater = rig.heartbeater();
    let mut first = sent(&[Feature::FactsV1], 4);
    first.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(first.clone()));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        rig.world.log.lines(),
        ["order o-1 try 1 of x waits: the facts file does not hold the facts of its reply"]
    );

    rig.world.facts.set_failing(false);
    rig.world.link.answer(Ok(first));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].facts_generation, Some(3));
    assert_eq!(
        beats[1].facts_generation,
        Some(3),
        "the failed write reported nothing new"
    );
    assert_eq!(beats[2].facts_generation, Some(4));
}

/// Proves an `abort` is acted on even when the facts of its reply cannot be written: it starts
/// nothing that reads the file, and it is the operator's way to stop what runs.
#[test]
fn an_abort_is_acted_on_though_its_facts_cannot_be_written() {
    let rig = keeping("host-facts-abort");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world.facts.set_failing(true);
    let mut heartbeater = rig.heartbeater();
    let mut answer = sent(&[Feature::FactsV1], 4);
    answer.order = Some(abort("a-1", "x"));
    rig.world.link.answer(Ok(answer));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
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

/// Proves a reply with no facts leaves the order of a reply as it was: started, with nothing
/// written, from a coordinator that advertises `facts-v1` as from one that does not.
#[test]
fn a_reply_without_facts_has_its_order_started() {
    let rig = keeping("host-facts-absent");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 1, "x", "2.0")),
        &[Feature::FactsV1],
    )));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.facts.applied().is_empty());
}

/// Proves a reply's `facts-v1` is a feature the daemon knows by its name, though it is not among
/// those every daemon announces, and every feature a daemon may announce fits the wire's limit.
#[test]
fn the_feature_reads_back_from_its_name_and_is_not_always_announced() {
    assert_eq!(Feature::FactsV1.as_str(), "facts-v1");
    assert_eq!(Feature::parse("facts-v1"), Some(Feature::FactsV1));
    assert!(!ANNOUNCED.contains(&Feature::FactsV1));
    assert!(ANNOUNCED.len() + 2 <= hostkeeper_wire::wire::MAX_FEATURES);
}

/// Proves the generation counts toward the heartbeat's bound: the JSON the bound measures has it
/// once the file holds facts, and has no key for it before.
#[test]
fn the_generation_is_in_the_json_the_bound_measures() {
    let rig = keeping("host-facts-bound");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[Feature::FactsV1], 9)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].to_json().get("facts_generation"), None);
    assert_eq!(
        beats[1].to_json().get("facts_generation"),
        Some(&serde_json::json!(9))
    );
}
