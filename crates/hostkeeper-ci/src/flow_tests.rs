//! The flow against a fake Releases service.

use hostkeeper_proto::hostkeeper::operator::v1::ConvergenceState;
use tonic::Status;

use crate::flow::execute;
use crate::testing::{
    ConvReply, Fake, convergence, flags, role, run_case, scratch, settings, stored,
};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn in_progress() -> ConvReply {
    convergence(
        Some("1.2.3"),
        ConvergenceState::InProgress,
        vec![role("web", 1, 1, 0, 0)],
    )
}

fn converged() -> ConvReply {
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
/// issued for, loses a field of a request, leaves anything in the work directory, or calls a
/// target other than the endpoint and the CA file the flags name.
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
    assert_eq!(log.bearers, ["token-1", "token-2", "token-3"]);
    assert_eq!(log.sets.len(), 1);
    let set = &log.sets[0];
    assert_eq!(set.service, "svc");
    assert_eq!(set.package_digest.as_deref(), Some(DIGEST));
    assert_eq!(set.release_index, None);
    assert_eq!(set.release_index_digest, None);
    assert_eq!(set.desired.as_ref().expect("desired").version, "1.2.3");
    assert_eq!(log.converges.len(), 2);
    for read in &log.converges {
        assert_eq!(read.service, "svc");
    }
    assert_eq!(
        log.audiences,
        ["hostkeeper-ci", "hostkeeper-ci", "hostkeeper-ci"]
    );
    let target = (
        "https://coordinator.example".to_owned(),
        ran.dir.path().join("ca.pem"),
    );
    assert_eq!(log.targets, [target.clone(), target.clone(), target]);
    drop(log);
    // The token is in memory only: the work directory holds what the test put there, and the
    // job's outputs.
    assert_eq!(ran.work_dir_names(), ["github-output", "web-roots.pem"]);
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
    let set = &log.sets[0];
    assert_eq!(set.service, "svc");
    assert_eq!(set.package_digest, None);
    assert_eq!(set.release_index.as_deref(), Some(text));
    assert_eq!(
        set.release_index_digest.as_deref(),
        Some(index_digest.as_str())
    );
    assert_eq!(set.desired.as_ref().expect("desired").version, "1.2.3");
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
    assert_eq!(ran.work_dir_names(), ["github-output", "web-roots.pem"]);
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
    assert!(ran.log.lock().expect("lock").bearers.is_empty());
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

/// Catches a lost answer reported as a refusal or as a version that is surely not set, a status
/// text that loses its code or its message, and a token that is printed in it.
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
            "::error::no answer to the request to set the version 1.2.3 of svc (Unavailable); whether it is set is not known"
        )
    );
    // A message is in the text, and the token is not.
    let told = run_case(
        "lost-message",
        Err(Status::deadline_exceeded("bad token-1 given")),
        vec![],
        false,
        |_| {},
    );
    assert_eq!(
        told.said().last().map(String::as_str),
        Some(
            "::error::no answer to the request to set the version 1.2.3 of svc (DeadlineExceeded: bad <token> given); whether it is set is not known"
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
    // Nor is a token on a status the coordinator does not refuse with.
    let missing = run_case(
        "not-found",
        Err(Status::not_found("no_such_thing")),
        vec![],
        false,
        |_| {},
    );
    assert_eq!(
        missing.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=no_answer\n")
    );
}

/// Catches each of the four codes with which the coordinator refuses not being read as a refusal
/// with its token, and an empty message being one.
#[test]
fn four_codes_refuse_with_a_token() {
    for (name, status) in [
        ("invalid-argument", Status::invalid_argument("a_1")),
        ("failed-precondition", Status::failed_precondition("a_1")),
        ("unauthenticated", Status::unauthenticated("a_1")),
        ("permission-denied", Status::permission_denied("a_1")),
    ] {
        let ran = run_case(name, Err(status), vec![], false, |_| {});
        assert_eq!(
            ran.outputs.as_deref(),
            Some("desired_version=\nconverged=false\nreason=a_1\n"),
            "{name}"
        );
    }
    let empty = run_case(
        "empty-refusal",
        Err(Status::invalid_argument("")),
        vec![],
        false,
        |_| {},
    );
    assert_eq!(
        empty.outputs.as_deref(),
        Some("desired_version=\nconverged=false\nreason=no_answer\n")
    );
}

/// Catches a call that cannot be made (a CA that cannot be read, a bad endpoint) reported as a
/// lost answer, as a refusal, or without saying that the version is not set.
#[test]
fn a_call_that_is_not_sent_says_why_and_that_the_version_is_not_set() {
    let dir = scratch("not-sent");
    let f = flags(dir.path());
    let mut fake = Fake::new(stored("1.2.3"), vec![]);
    fake.not_sending("cannot read the CA from ca.pem: gone");
    let outcome = execute(&settings(&f), &mut fake);
    assert_eq!(outcome.reason, "no_answer");
    assert_eq!(outcome.desired_version, "");
    assert_eq!(
        fake.log.lock().expect("lock").said,
        [
            "::add-mask::token-1",
            "::error::cannot read the CA from ca.pem: gone; the version 1.2.3 of svc is not set"
        ]
    );
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
