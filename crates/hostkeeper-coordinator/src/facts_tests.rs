//! The facts sent to a host ([`crate::facts`]): the generation of a host's facts and what raises
//! it, the facts a heartbeat over gRPC is answered with, and the rules that decide whether they
//! are sent. The answer over gRPC is in `grpc/tests/facts.rs`.

use std::collections::HashMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1::ApproveRequest;

use crate::enrollment::tests::support::{
    HostKey, Rig as EnrollmentRig, approval, coordinator_over, enroll_request,
};
use crate::facts::{FEATURE, Generations};
use crate::marks_support::{rig, who};
use crate::membership_tests::{labels, operate};
use crate::ops::{AuditEntry, OpsStore};
use crate::testutil::{Rig, kbf_services, scratch};

/// An entry of `action` for `node`, numbered `seq`.
fn entry(action: &str, node: Option<&str>, seq: u64) -> AuditEntry {
    let mut entry = AuditEntry::new("operator", action);
    entry.node_id = node.map(str::to_owned);
    entry.seq = seq;
    entry
}

/// A heartbeat over gRPC of a daemon that announces `features` and reports `generation`.
fn said(features: &[&str], generation: Option<u64>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        features: features.iter().map(|f| (*f).to_owned()).collect(),
        facts_generation: generation,
        ..agent::HeartbeatRequest::default()
    }
}

/// The facts the coordinator of `rig` sends the host `host` of the role `role` that announces
/// the feature and reports `generation`.
fn facts_for(
    rig: &Rig,
    host: &str,
    role: &str,
    generation: Option<u64>,
) -> Option<agent::HostFacts> {
    rig.coordinator.host_facts(
        &who(host, role),
        &said(&["heartbeat-v2", FEATURE], generation),
    )
}

fn labelled(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The generation of the host `host` in the ops store of `rig`.
fn generation(rig: &Rig, host: &str) -> u64 {
    rig.coordinator.ops.facts_generation(host)
}

/// Operates on a bare ops store: `body` as the operator.
fn act(store: &OpsStore, body: &str) -> Result<AuditEntry, crate::ops::OperateError> {
    crate::operate::operate(
        store,
        &kbf_services(),
        &crate::host_groups::HostGroups::default(),
        "operator",
        body.as_bytes(),
    )
}

fn set_labels(node: &str, set: &str) -> String {
    format!(r#"{{"action":"set_labels","node_id":"{node}","labels":{set}}}"#)
}

/// Proves: every host's generation is the base until a change raises it; an entry of a raising
/// action (`set_labels`, `enroll_approve`, `enroll_replace`, `enrollment.auto_approved`) takes
/// its own number for its node, and a later number of another host leaves this one alone.
///
/// Catches: a raise that moves every host, one that ignores an action that changes the facts,
/// or one that goes back to a lower number.
#[test]
fn a_raising_entry_gives_its_node_its_number_and_no_other() {
    let generations = Generations::from(7);
    assert_eq!((generations.of("a"), generations.of("b")), (7, 7));
    for (i, action) in [
        "set_labels",
        "enroll_approve",
        "enroll_replace",
        "enrollment.auto_approved",
    ]
    .into_iter()
    .enumerate()
    {
        let seq = 10 + i as u64;
        generations.raise(&[entry(action, Some("a"), seq)]);
        assert_eq!(
            (generations.of("a"), generations.of("b")),
            (seq, 7),
            "{action}"
        );
    }
    generations.raise(&[entry("set_labels", Some("b"), 20)]);
    assert_eq!((generations.of("a"), generations.of("b")), (13, 20));
    generations.raise(&[entry("set_labels", Some("a"), 5)]);
    assert_eq!(generations.of("a"), 13, "never back");
}

/// Proves: an entry of another action (an exclusion), an entry that names no node, and the base
/// of a store reopened on a higher number are the only things that leave or set a generation:
/// the first two raise nothing.
///
/// Catches: an exclusion or a node-less entry that resends every host its facts.
#[test]
fn other_entries_raise_nothing() {
    let generations = Generations::from(3);
    generations.raise(&[
        entry("exclude", Some("a"), 9),
        entry("set_labels", None, 10),
        entry("enroll_approve", None, 11),
    ]);
    assert_eq!((generations.of("a"), generations.of("b")), (3, 3));
}

/// Proves: a `set_labels` through `operate` raises its host's generation to the number of the
/// entry it returns, and no other host's; an exclusion, a refused `set_labels` (nine labels, an
/// unknown node) and a clear of a host without labels' neighbour raise nothing.
///
/// Catches: a raise before the commit, for a refused change, or for a change that is not the
/// host's facts.
#[test]
fn a_set_labels_raises_its_host_and_a_refusal_or_an_exclusion_raises_nothing() {
    let dir = scratch("facts-ops-raise");
    let store = OpsStore::open(&dir.path().join("ops.json")).expect("opened");
    let base = store.facts_generation("d1");
    assert_eq!(store.facts_generation("d2"), base);

    let first = act(&store, &set_labels("d1", r#"{"zone":"a"}"#)).expect("set");
    assert!(first.seq > base);
    assert_eq!(store.facts_generation("d1"), first.seq);
    assert_eq!(store.facts_generation("d2"), base);

    let exclusion = act(
        &store,
        r#"{"action":"exclude","service":"kbf","node_id":"d2","reason":"disk replaced"}"#,
    )
    .expect("excluded");
    assert!(exclusion.seq > first.seq);
    assert_eq!(store.facts_generation("d2"), base, "an exclusion");
    assert_eq!(store.facts_generation("d1"), first.seq);

    let nine = (1..=9)
        .map(|n| format!(r#""k{n}":"v""#))
        .collect::<Vec<_>>()
        .join(",");
    assert!(act(&store, &set_labels("d1", &format!("{{{nine}}}"))).is_err());
    assert!(act(&store, &set_labels("Not A Node!", r#"{"zone":"a"}"#)).is_err());
    assert_eq!(store.facts_generation("d1"), first.seq, "refused");
    assert_eq!(store.facts_generation("Not A Node!"), base);

    let second = act(&store, &set_labels("d1", "{}")).expect("emptied");
    assert_eq!(store.facts_generation("d1"), second.seq);
    assert!(second.seq > exclusion.seq);
}

/// Proves: a store opened again on the same file starts every host at the last audit number,
/// which is at least every generation a host was given before, so a generation a host holds is
/// never above the coordinator's after a restart.
///
/// Catches: a generation that goes back to 0 at a start, so that a host holding a higher
/// number is sent nothing, or never again until the number catches up.
#[test]
fn a_store_opened_again_starts_every_host_at_or_above_every_generation_given() {
    let dir = scratch("facts-ops-reopen");
    let file = dir.path().join("ops.json");
    let store = OpsStore::open(&file).expect("opened");
    act(&store, &set_labels("d1", r#"{"zone":"a"}"#)).expect("set");
    let last = act(&store, &set_labels("d2", r#"{"zone":"b"}"#)).expect("set");
    let given = [store.facts_generation("d1"), store.facts_generation("d2")];
    drop(store);

    let again = OpsStore::open(&file).expect("opened again");
    for host in ["d1", "d2", "d3"] {
        assert_eq!(again.facts_generation(host), last.seq, "{host}");
    }
    assert!(given.iter().all(|g| *g <= again.facts_generation("d1")));
    let next = act(&again, &set_labels("d1", r#"{"zone":"c"}"#)).expect("set");
    assert_eq!(again.facts_generation("d1"), next.seq);
    assert!(next.seq > last.seq);
}

/// Proves: a daemon that announces the feature and reports no generation is sent its facts: the
/// generation, its host id, its group (the role of its certificate, with no enrollment), and no
/// labels; one that reports the current generation is sent none; one that reports another is
/// sent them again.
///
/// Catches: facts that are never sent, sent at every heartbeat, or not resent to a host whose
/// number is not the current one.
#[test]
fn a_daemon_is_sent_its_facts_until_it_reports_their_generation() {
    let rig = rig("facts-sent", "");
    let current = generation(&rig, "d1");
    let facts = facts_for(&rig, "d1", "daemon", None).expect("facts");
    assert_eq!(
        facts,
        agent::HostFacts {
            generation: current,
            host_id: "d1".to_owned(),
            host_group: "daemon".to_owned(),
            labels: HashMap::new(),
        }
    );
    assert_eq!(facts_for(&rig, "d1", "daemon", Some(current)), None);
    labels(&rig, "d2", r#"{"zone":"a"}"#);
    let newer = generation(&rig, "d2");
    assert!(newer > current);
    let facts = facts_for(&rig, "d2", "daemon", Some(current)).expect("facts again");
    assert_eq!(facts.generation, newer);
    assert_eq!(
        facts_for(&rig, "d2", "daemon", Some(newer + 1)).map(|f| f.generation),
        Some(newer)
    );
}

/// Proves: a daemon that does not announce the feature (an old daemon) is sent none, and so is
/// one when the coordinator does not advertise it, whatever the daemon announces.
///
/// Catches: a field an older daemon reads strictly and refuses, or one sent for a feature the
/// coordinator does not offer.
#[test]
fn no_facts_go_to_a_daemon_that_did_not_announce_the_feature_or_when_it_is_not_advertised() {
    let mut rig = rig("facts-feature", "");
    let plain = said(&["heartbeat-v2"], None);
    assert_eq!(
        rig.coordinator.host_facts(&who("d1", "daemon"), &plain),
        None
    );
    assert_eq!(
        rig.coordinator
            .host_facts(&who("d1", "daemon"), &said(&[], None)),
        None
    );
    assert!(facts_for(&rig, "d1", "daemon", None).is_some());

    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_features(&["heartbeat-v2"]);
    assert_eq!(facts_for(&rig, "d1", "daemon", None), None);
}

/// Proves: the labels an operator set appear in the facts with a raised generation; a second set
/// replaces them; an emptied set gives empty labels with a generation raised again; the labels
/// of another host are not in them.
///
/// Catches: labels read from the wrong host, a stale set after a change, or an emptied set that
/// is not told to the host (its generation not raised).
#[test]
fn the_labels_of_a_host_are_sent_and_an_emptied_set_is_sent_empty() {
    let rig = rig("facts-labels", "");
    let before = facts_for(&rig, "d1", "daemon", None).expect("facts");
    labels(&rig, "d1", r#"{"zone":"a","rack":"r1"}"#);
    labels(&rig, "d2", r#"{"zone":"z"}"#);
    let set = facts_for(&rig, "d1", "daemon", Some(before.generation)).expect("facts");
    assert_eq!(set.labels, labelled(&[("zone", "a"), ("rack", "r1")]));
    assert!(set.generation > before.generation);

    labels(&rig, "d1", r#"{"zone":"b"}"#);
    let replaced = facts_for(&rig, "d1", "daemon", Some(set.generation)).expect("facts");
    assert_eq!(replaced.labels, labelled(&[("zone", "b")]));
    assert!(replaced.generation > set.generation);

    labels(&rig, "d1", "{}");
    let emptied = facts_for(&rig, "d1", "daemon", Some(replaced.generation)).expect("facts");
    assert_eq!(emptied.labels, HashMap::new());
    assert!(emptied.generation > replaced.generation);

    operate(
        &rig,
        r#"{"action":"exclude","service":"web","node_id":"d1","reason":"disk replaced"}"#,
    );
    assert_eq!(
        facts_for(&rig, "d1", "daemon", Some(emptied.generation)),
        None,
        "an exclusion is not a fact"
    );
}

/// The enrolment of `key` as `host`, approved by the operator for `role`.
fn approve_for(rig: &Rig, key: &HostKey, host: &str, role: &str, replace: bool) {
    let request = ApproveRequest {
        role: role.to_owned(),
        services: Vec::new(),
        replace,
        ..approval(key, host)
    };
    let caller = crate::operator::Caller::Operator("operator".parse().expect("a token name"));
    rig.coordinator
        .enrollment_approve(&caller, request)
        .expect("approved");
}

/// Proves: with enrollment, the group sent is the role the enrollment record holds, not the one
/// the certificate names; an approval raises the host's generation to the number of its
/// `enroll_approve` entry; an approval with `replace` for another role raises it again
/// (`enroll_replace`) and changes the group; the facts of a host the store holds no record of
/// come from its certificate's role.
///
/// Catches: a group taken from the certificate once the record says otherwise, or an approval
/// that leaves the host holding facts the record has since changed.
#[tokio::test]
async fn the_group_is_the_role_of_the_enrollment_record_and_an_approval_raises_the_generation() {
    let enrollment = EnrollmentRig::new("facts-enrolled");
    let rig = coordinator_over("facts-enrolled-coordinator", &enrollment).await;
    let (first, second) = (HostKey::new(), HostKey::new());
    for (key, machine) in [(&first, "machine-1"), (&second, "machine-2")] {
        enrollment
            .enrollment
            .enroll(enroll_request(key, machine))
            .expect("enrolled");
    }
    let before = generation(&rig, "s1");

    approve_for(&rig, &first, "s1", "storage", false);
    let approved = generation(&rig, "s1");
    assert!(approved > before);
    let entry = rig
        .coordinator
        .ops
        .state()
        .audit
        .into_iter()
        .rev()
        .find(|e| e.action == "enroll_approve")
        .expect("the approval is audited");
    assert_eq!(approved, entry.seq);
    assert_eq!(generation(&rig, "s2"), before, "another host");

    let facts = facts_for(&rig, "s1", "frontend", Some(before)).expect("facts");
    assert_eq!(
        (facts.generation, facts.host_group.as_str()),
        (approved, "storage"),
        "the record, not the certificate"
    );

    approve_for(&rig, &second, "s1", "frontend", true);
    let replaced = generation(&rig, "s1");
    assert!(replaced > approved);
    let facts = facts_for(&rig, "s1", "storage", Some(approved)).expect("facts");
    assert_eq!(
        (facts.generation, facts.host_group.as_str()),
        (replaced, "frontend")
    );

    // No record: the certificate's role.
    let facts = facts_for(&rig, "farm-1", "daemon", None).expect("facts");
    assert_eq!(facts.host_group, "daemon");
}

/// Proves: a host record the store cannot read gives no facts this heartbeat (and nothing
/// else breaks); once it reads again the facts are sent.
///
/// Catches: facts sent with a group guessed from the certificate when the record is unreadable,
/// or an unreadable record that fails the heartbeat.
#[tokio::test]
async fn an_unreadable_enrollment_record_gives_no_facts_for_that_beat() {
    let enrollment = EnrollmentRig::new("facts-unreadable");
    let rig = coordinator_over("facts-unreadable-coordinator", &enrollment).await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    approve_for(&rig, &key, "s1", "storage", false);
    assert!(facts_for(&rig, "s1", "storage", None).is_some());

    let file = enrollment.files.store.join("hosts").join("s1.pb");
    let good = std::fs::read(&file).expect("the host record");
    std::fs::write(&file, b"\xff\xff\xff").expect("damaged");
    assert_eq!(facts_for(&rig, "s1", "storage", None), None);
    std::fs::write(&file, good).expect("restored");
    assert!(facts_for(&rig, "s1", "storage", None).is_some());
}

/// Catches: `facts-v1` advertised and not acted on: a daemon that announces it is sent its facts,
/// and the same daemon that does not announce it is sent none.
pub(crate) async fn facts_v1_is_acted_on() {
    let rig = rig("facts-feature-acted-on", "");
    assert!(facts_for(&rig, "d1", "daemon", None).is_some());
    let silent = said(&["heartbeat-v2"], None);
    assert_eq!(
        rig.coordinator.host_facts(&who("d1", "daemon"), &silent),
        None
    );
}
