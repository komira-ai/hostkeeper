//! The flow against a fake coordinator, through the real `hostkeeper ctl`.

use hostkeeper_proto::hostkeeper::operator::v1::{
    ConvergeRequest, ConvergenceState, SetDesiredRequest,
};
use prost::Message as _;
use tonic::Status;

use crate::flow::execute;
use crate::testing::{Fake, convergence, flags, role, run_case, scratch, settings, stored};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn in_progress() -> crate::testing::Reply {
    convergence(
        Some("1.2.3"),
        ConvergenceState::InProgress,
        vec![role("web", 1, 1, 0, 0)],
    )
}

fn converged() -> crate::testing::Reply {
    convergence(
        Some("1.2.3"),
        ConvergenceState::Converged,
        vec![role("web", 2, 0, 0, 0)],
    )
}

fn polls(said: &[String]) -> Vec<&str> {
    said.iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("poll "))
        .collect()
}

/// Catches a call that reuses a token, sends it as anything but the bearer of the call it was
/// issued for, leaves the token file open to others or behind, or loses a field of the request.
#[test]
fn a_converged_fleet_ends_converged_with_one_token_per_call() {
    let ran = run_case(
        "converged",
        stored("1.2.3"),
        vec![in_progress(), converged()],
        false,
        |_| {},
    );
    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.events(),
        [
            "token 1",
            "SetDesired",
            "token 2",
            "Converge",
            "pause 10",
            "token 3",
            "Converge"
        ]
    );
    let log = ran.log.lock().expect("lock");
    let bearers: Vec<&str> = log.calls.iter().map(|c| c.bearer.as_str()).collect();
    assert_eq!(
        bearers,
        ["Bearer token-1", "Bearer token-2", "Bearer token-3"]
    );
    let paths: Vec<&str> = log.calls.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/hostkeeper.operator.v1.Releases/SetDesired",
            "/hostkeeper.operator.v1.Releases/Converge",
            "/hostkeeper.operator.v1.Releases/Converge"
        ]
    );
    let set = SetDesiredRequest::decode(log.calls[0].request.clone()).expect("a SetDesired");
    assert_eq!(set.service, "svc");
    assert_eq!(set.package_digest.as_deref(), Some(DIGEST));
    assert_eq!(set.desired.expect("desired").version, "1.2.3");
    for call in &log.calls[1..] {
        let read = ConvergeRequest::decode(call.request.clone()).expect("a Converge");
        assert_eq!(read.service, "svc");
    }
    // The file ctl read held the token, was private while the call ran, and each call had a
    // file of its own.
    assert_eq!(
        log.token_files,
        [
            (0o600, "token-1".to_owned()),
            (0o600, "token-2".to_owned()),
            (0o600, "token-3".to_owned())
        ]
    );
    assert_eq!(
        log.audiences,
        ["hostkeeper-ci", "hostkeeper-ci", "hostkeeper-ci"]
    );
    assert_eq!(
        log.targets,
        [
            (
                "coordinator.example".to_owned(),
                443,
                ran.dir.path().join("ca.pem")
            ),
            (
                "coordinator.example".to_owned(),
                443,
                ran.dir.path().join("ca.pem")
            ),
            (
                "coordinator.example".to_owned(),
                443,
                ran.dir.path().join("ca.pem")
            ),
        ]
    );
    drop(log);
    assert!(!ran.token_file_exists());
    assert_eq!(
        ran.outputs.as_deref(),
        Some("desired_version=1.2.3\nconverged=true\nreason=converged\n")
    );
    assert_eq!(
        ran.said(),
        [
            "::add-mask::token-1",
            "The desired version of svc is 1.2.3",
            "::add-mask::token-2",
            "poll 1: in_progress, desired 1.2.3, web: 1 at the version, 1 updating, 0 failed, 0 down",
            "::add-mask::token-3",
            "poll 2: converged, desired 1.2.3, web: 2 at the version, 0 updating, 0 failed, 0 down",
            "The hosts of svc run 1.2.3",
        ]
    );
    let request = std::fs::read_to_string(ran.dir.path().join("set-desired.json")).expect("file");
    assert_eq!(request, r#"{"desired":{"version":"1.2.3"}}"#);
}

/// Catches an index request that loses the index text or its digest, sends the text changed,
/// still sends a package digest, or changes how the rest of the flow runs.
#[test]
fn an_index_release_is_sent_whole_with_its_digest_and_no_package_digest() {
    let text = "{\r\n \"service\": \"svc\", \"note\": \"caf\u{e9} \\\" \u{1f680}\"\r\n}\n\n";
    let index_digest = "c".repeat(64);
    let ran = run_case(
        "index-release",
        stored("1.2.3"),
        vec![converged()],
        false,
        |f| {
            let file = f.work_dir.join("index.json");
            std::fs::write(&file, text).expect("write the index");
            f.package_digest = None;
            f.release_index_file = Some(file);
            f.release_index_digest = Some("c".repeat(64));
        },
    );
    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.events(),
        ["token 1", "SetDesired", "token 2", "Converge"]
    );
    let log = ran.log.lock().expect("lock");
    let set = SetDesiredRequest::decode(log.calls[0].request.clone()).expect("a SetDesired");
    assert_eq!(set.service, "svc");
    assert_eq!(set.package_digest, None);
    assert_eq!(set.release_index.as_deref(), Some(text));
    assert_eq!(
        set.release_index_digest.as_deref(),
        Some(index_digest.as_str())
    );
    assert_eq!(set.desired.expect("desired").version, "1.2.3");
    drop(log);
    let request: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(ran.dir.path().join("set-desired.json")).expect("file"),
    )
    .expect("JSON");
    assert_eq!(request["releaseIndex"], text);
    assert_eq!(request["releaseIndexDigest"], index_digest);
    assert_eq!(request["desired"]["version"], "1.2.3");
    assert_eq!(request.as_object().expect("an object").len(), 3);
}

/// Catches the coordinator's index refusals being reported as anything but their token, with the
/// version not set and no convergence read.
#[test]
fn a_refused_index_is_reported_with_its_token() {
    for token in ["index_digest_mismatch", "index_invalid"] {
        let ran = run_case(
            token,
            Err(Status::invalid_argument(token)),
            vec![converged()],
            false,
            |f| {
                let file = f.work_dir.join("index.json");
                std::fs::write(&file, "{}").expect("write the index");
                f.package_digest = None;
                f.release_index_file = Some(file);
                f.release_index_digest = Some("c".repeat(64));
            },
        );
        assert_eq!(ran.code, 1, "{token}");
        assert_eq!(ran.events(), ["token 1", "SetDesired"], "{token}");
        assert_eq!(
            ran.outputs.as_deref(),
            Some(format!("desired_version=\nconverged=false\nreason={token}\n").as_str()),
            "{token}"
        );
    }
}

/// Catches a poll loop that reads more or fewer than `--max-polls` times, pauses before the
/// first read or after the last, or ends a fleet that never converged as a success.
#[test]
fn the_poll_stops_at_max_polls_with_not_converged() {
    let ran = run_case(
        "max-polls",
        stored("1.2.3"),
        vec![in_progress()],
        false,
        |f| f.max_polls = 3,
    );
    assert_eq!(ran.code, 1);
    let events = ran.events();
    assert_eq!(events.iter().filter(|e| *e == "Converge").count(), 3);
    assert_eq!(events.iter().filter(|e| *e == "pause 10").count(), 2);
    assert_eq!(
        ran.outputs.as_deref(),
        Some("desired_version=1.2.3\nconverged=false\nreason=not_converged\n")
    );
    let said = ran.said();
    assert_eq!(polls(&said).len(), 1);
    assert_eq!(
        said.last().map(String::as_str),
        Some("::error::the version 1.2.3 of svc was set but did not converge in 3 reads")
    );
}

/// Catches a refused write that is polled anyway, or that leaves the version looking set.
#[test]
fn a_refused_set_desired_sets_nothing_and_never_polls() {
    let ran = run_case(
        "refused",
        Err(Status::failed_precondition("held_bad")),
        vec![converged()],
        false,
        |_| {},
    );
    assert_eq!(ran.code, 1);
    assert_eq!(ran.events(), ["token 1", "SetDesired"]);
    assert_eq!(
        ran.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=held_bad\n")
    );
    assert_eq!(
        ran.said().last().map(String::as_str),
        Some("::error::the coordinator refused the version 1.2.3 of svc: held_bad; it is not set")
    );
    assert!(!ran.token_file_exists());
}

/// Catches a wait that goes on after the service halted or got another version, and a refusal
/// of a read that is not reported with its token.
#[test]
fn a_halt_or_another_desired_version_ends_the_wait() {
    let halted = run_case(
        "halted",
        stored("1.2.3"),
        vec![convergence(Some("1.2.3"), ConvergenceState::Halted, vec![])],
        false,
        |_| {},
    );
    assert_eq!(halted.code, 1);
    assert_eq!(
        halted.outputs.as_deref(),
        Some("desired_version=1.2.3\nconverged=false\nreason=halted\n")
    );
    assert_eq!(
        halted.said().last().map(String::as_str),
        Some("::error::the version 1.2.3 of svc was set, but the service is halted")
    );
    let superseded = run_case(
        "superseded",
        stored("1.2.3"),
        vec![convergence(
            Some("other"),
            ConvergenceState::Converged,
            vec![],
        )],
        false,
        |_| {},
    );
    assert_eq!(superseded.code, 1);
    assert_eq!(
        superseded.outputs.as_deref(),
        Some("desired_version=1.2.3\nconverged=false\nreason=superseded\n")
    );
    assert_eq!(
        superseded.said().last().map(String::as_str),
        Some("::error::the version 1.2.3 of svc was set, but another desired version replaced it")
    );
    let refused = run_case(
        "read-refused",
        stored("1.2.3"),
        vec![Err(Status::permission_denied("not_ci"))],
        false,
        |_| {},
    );
    assert_eq!(refused.code, 1);
    assert_eq!(
        refused.outputs.as_deref(),
        Some("desired_version=1.2.3\nconverged=false\nreason=not_ci\n")
    );
    assert_eq!(
        refused.said().last().map(String::as_str),
        Some(
            "::error::the version 1.2.3 of svc was set, but the coordinator refused to report its convergence: not_ci"
        )
    );
    assert_eq!(polls(&refused.said()), ["poll 1: refused: not_ci"]);
}

/// Catches a `--wait false` that reads the convergence, or that reports the version as
/// converged.
#[test]
fn without_wait_the_version_is_set_and_not_waited_for() {
    let ran = run_case("no-wait", stored("1.2.3"), vec![converged()], false, |f| {
        f.wait = false
    });
    assert_eq!(ran.code, 0);
    assert_eq!(ran.events(), ["token 1", "SetDesired"]);
    assert_eq!(
        ran.outputs.as_deref(),
        Some("desired_version=1.2.3\nconverged=false\nreason=not_waited\n")
    );
    assert_eq!(
        ran.said(),
        ["::add-mask::token-1", "The desired version of svc is 1.2.3"]
    );
}

/// Catches a read without an answer that ends the wait, and an observation printed again each
/// time it is seen.
#[test]
fn a_read_without_an_answer_counts_as_a_poll_and_repeats_are_printed_once() {
    let ran = run_case(
        "repeats",
        stored("1.2.3"),
        vec![
            Err(Status::unavailable("")),
            in_progress(),
            in_progress(),
            converged(),
        ],
        false,
        |_| {},
    );
    assert_eq!(ran.code, 0);
    assert_eq!(
        polls(&ran.said()),
        [
            "poll 1: no answer",
            "poll 2: in_progress, desired 1.2.3, web: 1 at the version, 1 updating, 0 failed, 0 down",
            "poll 4: converged, desired 1.2.3, web: 2 at the version, 0 updating, 0 failed, 0 down",
        ]
    );
    assert_eq!(
        ran.outputs.as_deref(),
        Some("desired_version=1.2.3\nconverged=true\nreason=converged\n")
    );
}

/// Catches a flow that calls the coordinator without a token, and one that does not say that
/// the version is not set.
#[test]
fn no_token_means_no_call() {
    let ran = run_case("no-token", stored("1.2.3"), vec![converged()], true, |_| {});
    assert_eq!(ran.code, 1);
    assert_eq!(ran.events(), Vec::<String>::new());
    assert!(ran.log.lock().expect("lock").calls.is_empty());
    assert_eq!(
        ran.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=no_answer\n")
    );
    assert_eq!(
        ran.said(),
        [
            "::error::cannot get an OIDC token: no OIDC token request variables: the calling job must grant id-token: write; the version 1.2.3 of svc is not set"
        ]
    );
}

/// Catches a write that counts as done when the coordinator stored another version, or none.
#[test]
fn the_coordinator_must_store_the_version() {
    let other = run_case(
        "stored-other",
        stored("1.2.4"),
        vec![converged()],
        false,
        |_| {},
    );
    assert_eq!(other.code, 1);
    assert_eq!(other.events(), ["token 1", "SetDesired"]);
    assert_eq!(
        other.outputs.as_deref(),
        Some("desired_version=1.2.4\nconverged=false\nreason=not_stored\n")
    );
    assert_eq!(
        other.said().last().map(String::as_str),
        Some("::error::the coordinator did not store the version 1.2.3 of svc; it is not set")
    );
    // A stored version that is not a version is not echoed to the job's outputs.
    let odd = run_case("stored-odd", stored("a b\nc"), vec![], false, |_| {});
    assert_eq!(
        odd.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=not_stored\n")
    );
}

/// Catches a lost answer reported as a refusal or as a version that is surely not set, and a
/// failure to write the token file or the request file that is not reported at all.
#[test]
fn a_call_that_cannot_be_made_or_answered_is_reported_as_no_answer() {
    let lost = run_case("lost", Err(Status::unavailable("")), vec![], false, |_| {});
    assert_eq!(lost.code, 1);
    assert_eq!(
        lost.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=no_answer\n")
    );
    assert_eq!(
        lost.said().last().map(String::as_str),
        Some(
            "::error::no answer to the request to set the version 1.2.3 of svc (ctl exited 1: error: UNAVAILABLE:); whether it is set is not known"
        )
    );
    // A refusal that is not a token is no refusal.
    let odd = run_case(
        "odd-refusal",
        Err(Status::failed_precondition("Not A Token")),
        vec![],
        false,
        |_| {},
    );
    assert_eq!(
        odd.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=no_answer\n")
    );
    assert_eq!(odd.events(), ["token 1", "SetDesired"]);
    // Nor is one that is too long to be one.
    let long = run_case(
        "long-refusal",
        Err(Status::failed_precondition("a".repeat(65))),
        vec![],
        false,
        |_| {},
    );
    assert_eq!(
        long.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=no_answer\n")
    );

    // A directory where the token file goes: it cannot be made.
    let dir = scratch("token-file");
    let f = flags(dir.path());
    std::fs::create_dir(dir.path().join("hostkeeper-oidc-token")).expect("mkdir");
    let mut fake = Fake::new(dir.path(), stored("1.2.3"), vec![]);
    let outcome = execute(&settings(&f), &mut fake);
    assert_eq!(outcome.reason, "no_answer");
    assert_eq!(fake.log.lock().expect("lock").events, ["token 1"]);
    let said = fake.log.lock().expect("lock").said.clone();
    assert_eq!(said.len(), 2);
    assert!(
        said[1].starts_with("::error::cannot write the token file "),
        "{said:?}"
    );
    assert!(
        said[1].ends_with("; the version 1.2.3 of svc is not set"),
        "{said:?}"
    );

    // A directory where the request file goes: it cannot be written, and no token is spent.
    let dir = scratch("request-file");
    let f = flags(dir.path());
    std::fs::create_dir(dir.path().join("set-desired.json")).expect("mkdir");
    let mut fake = Fake::new(dir.path(), stored("1.2.3"), vec![]);
    let outcome = execute(&settings(&f), &mut fake);
    assert_eq!(outcome.reason, "no_answer");
    assert!(fake.log.lock().expect("lock").events.is_empty());
    let said = fake.log.lock().expect("lock").said.clone();
    assert_eq!(said.len(), 1);
    assert!(
        said[0].starts_with("::error::cannot write the request file "),
        "{said:?}"
    );
}

/// Catches a token file left by an earlier run that is reused (with its mode and its content)
/// instead of made fresh.
#[test]
fn an_old_token_file_is_replaced_by_a_fresh_private_one() {
    let dir = scratch("old-token");
    let f = flags(dir.path());
    let old = dir.path().join("hostkeeper-oidc-token");
    std::fs::write(&old, "stale").expect("write");
    let mut fake = Fake::new(dir.path(), stored("1.2.3"), vec![]);
    let mut settings = settings(&f);
    settings.wait = false;
    let outcome = execute(&settings, &mut fake);
    assert_eq!(outcome.reason, "not_waited");
    assert_eq!(
        fake.log.lock().expect("lock").token_files,
        [(0o600, "token-1".to_owned())]
    );
    assert!(!old.exists());
}

/// Catches job outputs that are lost, or a failure to write them that exits 0.
#[test]
fn outputs_that_cannot_be_written_fail_the_job() {
    let ran = run_case("outputs-dir", stored("1.2.3"), vec![], false, |f| {
        f.wait = false;
        let path = f.work_dir.join("outputs-is-a-dir");
        std::fs::create_dir(&path).expect("mkdir");
        f.github_output = Some(path);
    });
    assert_eq!(ran.code, 1);
    let said = ran.said();
    assert!(
        said.last()
            .is_some_and(|l| l.starts_with("::error::cannot write the job outputs to ")),
        "{said:?}"
    );
    // Without an output file the status alone says how it went.
    let none = run_case("no-outputs", stored("1.2.3"), vec![], false, |f| {
        f.wait = false;
        f.github_output = None;
    });
    assert_eq!(none.code, 0);
    assert_eq!(none.outputs, None);
}

/// Catches outputs that replace what earlier steps of the job wrote to the same file.
#[test]
fn outputs_are_appended_to_the_file() {
    let ran = run_case("append", stored("1.2.3"), vec![], false, |f| {
        f.wait = false;
        let path = f.work_dir.join("earlier-output");
        std::fs::write(&path, "earlier=1\n").expect("write");
        f.github_output = Some(path);
    });
    assert_eq!(
        ran.outputs.as_deref(),
        Some("earlier=1\ndesired_version=1.2.3\nconverged=false\nreason=not_waited\n")
    );
}
