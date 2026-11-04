//! The recorded heartbeat sequences: what kbf v0.0.64's coordinator answered, replayed against
//! this one through the HTTP surface, byte for byte, except where a rule this coordinator cut
//! decided.

mod support;

use support::replay::{Sequence, check_changed_list, replay, sequence_files};

/// The recorded sequences in which a rule this coordinator cut decided: each records this
/// coordinator's answer in `expect_status`/`expect_body` and kbf's beside it in
/// `kbf_status`/`kbf_body`. Every other sequence is kbf's bytes, and any difference in it fails.
/// `tests/fixtures/recorded/README.md` says, for each, the rule and the old and the new reason.
const CHANGED: &[&str] = &[
    "24-cut-never-last-does-not-delay-an-order",
    "25-cut-min-up-does-not-delay-an-order",
    "26-cut-role-order-does-not-delay-an-order",
    "27-cut-a-role-limit-of-zero-does-not-hold-the-role",
    "28-cut-convergence-does-not-need-min-up",
    "29-cut-service-order-does-not-delay-an-order",
    "30-cut-require-min-up-does-not-refuse-a-body",
];

/// Proves: this coordinator, built from the recorded flags and the recorded rollout file as
/// `config::build` builds one, answers every call of every recorded sequence with the status and
/// the body bytes kbf's coordinator answered, except in the sequences of `CHANGED`, where a rule
/// this coordinator cut (the minimum of hosts up, the `never_last` floor, the order of roles and
/// of services, a role's limit of zero, the required `min_up` keys) decided and the recorded
/// answer is this coordinator's, with kbf's kept beside it. The sequences cover halts (raise,
/// hold, operator clear, the generations in an answer), an automatic rollback to the last good
/// version and the version marked bad, bake, canary, deferral and its backoff, one live update
/// at a time, an updater that sends `services` and one that does not, and kbf's status and
/// convergence bytes. Every differing step of every sequence is reported in one failure, with
/// the answer in full.
/// Catches: a change of an answer that is not a cut rule's, and a cut rule whose answer moved
/// back.
#[tokio::test]
async fn decisions_equal_kbf_except_where_a_cut_rule_decided() {
    let files = sequence_files();
    assert!(
        files.len() >= 30,
        "the recorded sequences are missing: {files:?}"
    );
    let mut sequences = Vec::new();
    for file in &files {
        let stem = file.file_stem().and_then(|s| s.to_str()).expect("a name");
        sequences.push((stem.to_owned(), Sequence::load(file)));
    }
    check_changed_list(CHANGED, &sequences).unwrap_or_else(|why| panic!("{why}"));
    let mut differences = Vec::new();
    for (stem, sequence) in &sequences {
        assert!(
            sequence.from.starts_with("kbf-coordinator/"),
            "{stem}: `from` must name the kbf test or file"
        );
        if let Err(why) = replay(sequence, stem).await {
            differences.push(why);
        }
    }
    assert!(
        differences.is_empty(),
        "{} of {} recorded sequences differ from what is recorded; every differing step \
         follows, with the actual answer in full. A step that is right in this build and differs \
         from kbf's is a rule this build cut: its sequence belongs in CHANGED with kbf's answer \
         in kbf_status/kbf_body.\n{}",
        differences.len(),
        sequences.len(),
        differences.join("\n")
    );
}

/// A one-step sequence: a heartbeat of a healthy host, with `expect` as the members that say
/// what is recorded of the answer.
fn heartbeat_sequence(call: &str, expect: &str) -> Sequence {
    let beat = r#"{\"platform\":\"linux-x86_64\",\"version\":\"1.0\",\"health\":\"healthy\"}"#;
    let text = format!(
        r#"{{"from":"kbf-coordinator/x","steps":[{{"at_secs":2000,"call":"{call}","host":"d1","role":"daemon","body":"{beat}",{expect}}}]}}"#
    );
    Sequence::parse(&text).expect("a sequence")
}

/// Catches: a replay that cannot fail. A sequence whose recorded reply is not the one the
/// coordinator gives, a recorded status that is not its status, a fragment it does not hold, a
/// fragment it must not hold and a call that does not exist are each reported at their step.
#[tokio::test]
async fn the_replay_reports_the_step_that_differs() {
    let right = heartbeat_sequence(
        "heartbeat",
        r#""expect_status":200,"expect_body":"{\"reply\":\"nothing\"}""#,
    );
    replay(&right, "replay-right")
        .await
        .expect("the recorded reply");
    let cases = [
        (
            r#""expect_status":200,"expect_body":"{\"reply\":\"update\",\"version\":\"2.0\"}""#,
            "recorded",
        ),
        (
            r#""expect_status":200,"expect_contains":["update"]"#,
            "lacks",
        ),
        (
            r#""expect_status":200,"expect_not_contains":["nothing"]"#,
            "has",
        ),
        (r#""expect_status":404"#, "status 200, recorded 404"),
    ];
    for (n, (expect, word)) in cases.into_iter().enumerate() {
        let wrong = heartbeat_sequence("heartbeat", expect);
        let why = replay(&wrong, &format!("replay-wrong-{n}"))
            .await
            .expect_err("a mismatch");
        assert!(why.contains("step 0") && why.contains(word), "{why}");
    }
    let unknown = heartbeat_sequence("teleport", r#""expect_status":200"#);
    let why = replay(&unknown, "replay-call")
        .await
        .expect_err("no such call");
    assert!(why.contains("no call named teleport"), "{why}");
}

/// Catches: a replay that stops at the first difference, so that one run shows one step: two
/// steps that both differ are both reported, each with the sequence's name, its index and the
/// actual body in full.
#[tokio::test]
async fn the_replay_reports_every_step_that_differs() {
    let beat = r#"{\"platform\":\"linux-x86_64\",\"version\":\"1.0\",\"health\":\"healthy\"}"#;
    let wrong = r#""expect_status":200,"expect_body":"{\"reply\":\"update\",\"version\":\"9.0\"}""#;
    let step = |host: &str| {
        format!(
            r#"{{"at_secs":2000,"call":"heartbeat","host":"{host}","role":"daemon","body":"{beat}",{wrong}}}"#
        )
    };
    let text = format!(
        r#"{{"from":"kbf-coordinator/x","steps":[{},{}]}}"#,
        step("d1"),
        step("d2")
    );
    let sequence = Sequence::parse(&text).expect("a sequence");
    let why = replay(&sequence, "replay-two")
        .await
        .expect_err("two mismatches");
    assert!(why.contains("replay-two: step 0"), "{why}");
    assert!(why.contains("replay-two: step 1"), "{why}");
    assert!(
        why.contains(r#"{"reply":"nothing"}"#),
        "the actual body is in full: {why}"
    );
}

/// A sequence of `steps` named `stem`.
fn named(stem: &str, steps: &str) -> (String, Sequence) {
    let text = format!(r#"{{"from":"kbf-coordinator/x","steps":[{steps}]}}"#);
    (stem.to_owned(), Sequence::parse(&text).expect("a sequence"))
}

const PLAIN: &str = r#"{"at_secs":1,"call":"status","expect_status":200,"expect_body":"b"}"#;
const KEPT: &str =
    r#"{"at_secs":1,"call":"status","expect_status":200,"expect_body":"b","kbf_body":"a"}"#;
const SAME: &str =
    r#"{"at_secs":1,"call":"status","expect_status":200,"expect_body":"b","kbf_body":"b"}"#;

/// Catches: a list of changed sequences that agrees with nothing: a name that is no recorded
/// sequence, a listed sequence that keeps no answer of kbf's, an unlisted one that keeps one, and
/// a kept answer equal to this coordinator's are each refused, naming the sequence; a list that
/// agrees is accepted.
#[test]
fn the_list_of_changed_sequences_is_checked() {
    check_changed_list(&["one"], &[named("one", KEPT), named("two", PLAIN)]).expect("agrees");
    let missing = check_changed_list(&["nope"], &[named("one", PLAIN)]).expect_err("no such file");
    assert!(
        missing.contains("nope") && missing.contains("no recorded sequence"),
        "{missing}"
    );
    let bare = check_changed_list(&["one"], &[named("one", PLAIN)]).expect_err("nothing kept");
    assert!(
        bare.contains("one is in CHANGED but no step keeps"),
        "{bare}"
    );
    let stray = check_changed_list(&[], &[named("one", KEPT)]).expect_err("not listed");
    assert!(
        stray.contains("one keeps kbf's answer in a step but is not in CHANGED"),
        "{stray}"
    );
    let same = check_changed_list(&["one"], &[named("one", SAME)]).expect_err("the same answer");
    assert!(
        same.contains("kbf_body is the same as this coordinator's"),
        "{same}"
    );
}
