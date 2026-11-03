//! The recorded heartbeat sequences: what kbf v0.0.64's coordinator answered, replayed against
//! this one through the HTTP surface, byte for byte.

mod support;

use support::replay::{Sequence, replay, sequence_files};

/// Proves: this coordinator, built from the recorded flags and the recorded kbf services file as
/// `config::build` builds one, answers every call of every recorded sequence with the status and
/// the body bytes kbf's coordinator answered (kbf's tests at the tag pinned them; each sequence
/// file names its source test in `from`). The sequences cover halts (raise, hold, operator
/// clear, the generations in an answer), an automatic rollback to the last good version and the
/// version marked bad, bake, canary, deferral and its backoff, the order of the roles (a
/// frontend waits for storage), one live update at a time, an updater that sends `services` and
/// one that does not, and kbf's status and convergence bytes.
#[tokio::test]
async fn decisions_equal_kbf_on_recorded_heartbeat_sequences() {
    let files = sequence_files();
    assert!(
        files.len() >= 20,
        "the recorded sequences are missing: {files:?}"
    );
    for file in files {
        let sequence = Sequence::load(&file);
        assert!(
            sequence.from.starts_with("kbf-coordinator/"),
            "{}: `from` must name the kbf test",
            file.display()
        );
        let stem = file.file_stem().and_then(|s| s.to_str()).expect("a name");
        replay(&sequence, stem)
            .await
            .unwrap_or_else(|why| panic!("{}: {why}", file.display()));
    }
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
