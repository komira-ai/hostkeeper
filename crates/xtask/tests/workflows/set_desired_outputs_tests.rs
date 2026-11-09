//! The two jq programs of the real `set-desired.yml`, run with jaq, with no shell: the outputs
//! program on one fixture per row of the table of docs/deploy/ci-client.md, "Outputs", and the
//! boundary cases beside each row; and the request program on each way a caller names, or fails
//! to name, the release.
//!
//! A fixture is what the steps before the outputs step leave: the outcome of each call step, the
//! answer each wrote with `--response-file`, the diagnostics each wrote to its own file (hkctl's
//! `refused: <token>` line among them), and the inputs the program names. A file a call did not
//! leave is read as `/dev/null` by the workflow, so it is empty here.

use super::jq::run_on;
pub(super) use super::jq::{Ended, Ran};
use super::set_desired_calls::{OUTPUTS_ARGS, outputs_program, request_program};
use super::set_desired_tests::real;
use super::yaml;

/// Runs `program` with jq's `--null-input`, its variables bound to the texts of `vars`.
pub(super) fn run(program: &str, vars: &[(&str, String)]) -> Ran {
    run_on(program, vars, "null")
}

/// What the steps before the outputs step leave, by the variable of [`OUTPUTS_ARGS`] each is.
#[derive(Clone)]
struct Left<'a> {
    service: &'a str,
    version: &'a str,
    wait: &'a str,
    set_outcome: &'a str,
    wait_outcome: &'a str,
    set_body: &'a str,
    set_err: &'a str,
    wait_body: &'a str,
    wait_err: &'a str,
}

/// A set that succeeded and stored version `1.2.3` of `hello`, and a wait that was not run.
const SET: Left<'static> = Left {
    service: "hello",
    version: "1.2.3",
    wait: "true",
    set_outcome: "success",
    wait_outcome: "skipped",
    set_body: "{\"desired\":{\"version\":\"1.2.3\",\"maxUpdating\":1},\"record\":{\"generation\":\"4\"}}\n",
    set_err: "",
    wait_body: "",
    wait_err: "",
};

/// A set that failed: no answer file, its diagnostics `err`; the wait skipped.
fn set_failed(err: &str) -> Left<'_> {
    Left {
        set_outcome: "failure",
        set_body: "",
        set_err: err,
        ..SET
    }
}

/// [`SET`] with a wait that ran and ended `outcome`, answering `body` with diagnostics `err`.
fn waited<'a>(outcome: &'a str, body: &'a str, err: &'a str) -> Left<'a> {
    Left {
        wait_outcome: outcome,
        wait_body: body,
        wait_err: err,
        ..SET
    }
}

/// hkctl's diagnostics of a refusal with the reason `refused:` line it prints.
const REFUSED_SET: &str =
    "hkctl: the coordinator answered FAILED_PRECONDITION\nrefused: not_in_channel\n";

/// The outputs program of the real workflow, run on `left`.
fn outputs(left: &Left) -> Ran {
    let doc = yaml::load(&real()).expect("the workflow parses");
    let program = outputs_program(&doc).expect("the outputs step has its program");
    let vars: Vec<(&str, String)> = OUTPUTS_ARGS
        .iter()
        .map(|&(_, name, _)| {
            let value = match name {
                "service" => left.service,
                "version" => left.version,
                "wait" => left.wait,
                "set_outcome" => left.set_outcome,
                "wait_outcome" => left.wait_outcome,
                "set_body" => left.set_body,
                "set_err" => left.set_err,
                "wait_body" => left.wait_body,
                "wait_err" => left.wait_err,
                other => panic!("the test binds no variable {other}"),
            };
            (name, value.to_owned())
        })
        .collect();
    run(program, &vars)
}

/// The three output lines, and whether the step ends with exit 1.
fn want(desired: &str, converged: bool, reason: &str, fails: bool) -> Ran {
    (
        vec![
            format!("desired_version={desired}"),
            format!("converged={converged}"),
            format!("reason={reason}"),
        ],
        if fails { Ended::Halted(1) } else { Ended::Done },
    )
}

#[test]
fn a_refused_set_writes_its_token_and_no_version() {
    // Row 1: the set failed with `refused: <token>`. The version is not set, so
    // desired_version stays empty, as the output's contract says; the job fails. Catches the
    // requested version written out for a refused set, and a refusal read from another line.
    assert_eq!(
        outputs(&set_failed(REFUSED_SET)),
        want("", false, "not_in_channel", true)
    );
    // An unauthenticated caller exits 3 with its token the same way.
    assert_eq!(
        outputs(&set_failed(
            "hkctl: the coordinator answered UNAUTHENTICATED\nrefused: unauthorized\n"
        )),
        want("", false, "unauthorized", true)
    );
}

#[test]
fn a_set_with_no_refusal_token_is_no_answer() {
    // Row 2: the set failed otherwise. Catches a transport failure, a reason outside the token
    // grammar (which hkctl drops, and the program must not trust), a usage error and a
    // cancelled set read as anything but `no_answer`, and any of them writing a version.
    for err in [
        "curl: (7) Failed to connect to coordinator.example port 443\nhkctl: no answer from https://coordinator.example (curl exited 7)\n",
        "hkctl: the coordinator answered FAILED_PRECONDITION\nrefused: Not A Token\n",
        "hkctl: the coordinator answered FAILED_PRECONDITION\nrefused: \n",
        "hkctl: the answer (HTTP 502) is not the gateway's\n",
        "hkctl: --endpoint takes https://<host>[:<port>]\n",
        "",
    ] {
        assert_eq!(
            outputs(&set_failed(err)),
            want("", false, "no_answer", true),
            "{err:?}"
        );
    }
    let cancelled = Left {
        set_outcome: "cancelled",
        ..set_failed("")
    };
    assert_eq!(outputs(&cancelled), want("", false, "no_answer", true));
    // A token of 65 characters is outside the grammar too.
    let long = format!("refused: {}\n", "a".repeat(65));
    assert_eq!(
        outputs(&set_failed(&long)),
        want("", false, "no_answer", true)
    );
    let longest = format!("refused: {}\n", "a".repeat(64));
    assert_eq!(
        outputs(&set_failed(&longest)),
        want("", false, &"a".repeat(64), true)
    );
}

#[test]
fn a_set_that_did_not_run_writes_no_reason() {
    // Before the set: a verification, the install, the CA or the request failed, and the set
    // step was skipped. Catches that read as an answer of the coordinator: the reason stays
    // empty, as today, and the job fails.
    for outcome in ["skipped", ""] {
        let skipped = Left {
            set_outcome: outcome,
            ..set_failed("")
        };
        assert_eq!(outputs(&skipped), want("", false, "", true), "{outcome:?}");
    }
}

#[test]
fn another_stored_version_is_not_stored_with_the_stored_one_when_it_is_a_version() {
    // Row 3: the coordinator answered with another version. Catches the stored version dropped,
    // the requested one written instead, and a stored value that is not a version (a newline
    // would add a line of its own to the job's outputs) written out.
    let stored = |body: &'static str| Left::<'static> {
        set_body: body,
        ..SET
    };
    assert_eq!(
        outputs(&stored("{\"desired\":{\"version\":\"1.2.2\"}}\n")),
        want("1.2.2", false, "not_stored", true)
    );
    for body in [
        "{\"desired\":{\"version\":\"1.2.2\\nreason=converged\"}}\n",
        "{\"desired\":{\"version\":\"1.2.2\\n\"}}\n",
        "{\"desired\":{\"version\":\"-1\"}}\n",
        "{\"desired\":{\"version\":7}}\n",
        "{\"desired\":{}}\n",
        "{\"desired\":\"1.2.3\"}\n",
        "[1]\n",
        "not json\n",
        "",
    ] {
        assert_eq!(
            outputs(&stored(body)),
            want("", false, "not_stored", true),
            "{body:?}"
        );
    }
}

#[test]
fn a_set_not_waited_for_succeeds_with_the_version() {
    // Row 4: `wait` is false. Catches the job failing when it was not asked to wait, and the
    // version not written.
    let no_wait = Left {
        wait: "false",
        ..SET
    };
    assert_eq!(outputs(&no_wait), want("1.2.3", false, "not_waited", false));
}

#[test]
fn a_refused_wait_writes_its_token() {
    // Row 5: the wait failed with `refused: <token>`: the version is set, so it is written.
    // Catches a max_polls above 180 or a full listener reported as anything but its token.
    for (err, token) in [
        (
            "hkctl: the coordinator answered INVALID_ARGUMENT\nrefused: invalid_wait\n",
            "invalid_wait",
        ),
        (
            "hkctl: the coordinator answered FAILED_PRECONDITION\nrefused: too_many_waits\n",
            "too_many_waits",
        ),
    ] {
        assert_eq!(
            outputs(&waited("failure", "", err)),
            want("1.2.3", false, token, true)
        );
    }
}

#[test]
fn a_wait_with_no_outcome_is_no_answer() {
    // Row 6: the wait failed otherwise, or answered no outcome the table names. Catches a cut
    // wait, an absent, unspecified or unknown outcome read as converged or as the deadline.
    for (outcome, body, err) in [
        (
            "failure",
            "",
            "curl: (52) Empty reply from server\nhkctl: no answer from https://coordinator.example (curl exited 52)\n",
        ),
        ("failure", "", "refused: Not A Token\n"),
        ("cancelled", "", ""),
        ("success", "{}\n", ""),
        (
            "success",
            "{\"outcome\":\"WAIT_OUTCOME_UNSPECIFIED\"}\n",
            "",
        ),
        ("success", "{\"outcome\":\"WAIT_OUTCOME_PAUSED\"}\n", ""),
        ("success", "{\"outcome\":3}\n", ""),
        ("success", "not json\n", ""),
    ] {
        assert_eq!(
            outputs(&waited(outcome, body, err)),
            want("1.2.3", false, "no_answer", true),
            "{outcome} {body:?} {err:?}"
        );
    }
}

#[test]
fn each_outcome_of_the_wait_is_its_reason() {
    // Rows 7 and 8: converged, halted and superseded by name, the deadline as not_converged;
    // only converged is `converged=true` and a success. Catches an outcome mapped to another
    // reason, and a halt or a supersession counted as converged.
    for (body, reason, fails) in [
        (
            "{\"outcome\":\"WAIT_OUTCOME_CONVERGED\",\"convergence\":{\"state\":\"CONVERGED\"}}\n",
            "converged",
            false,
        ),
        ("{\"outcome\":\"WAIT_OUTCOME_HALTED\"}\n", "halted", true),
        (
            "{\"outcome\":\"WAIT_OUTCOME_SUPERSEDED\"}\n",
            "superseded",
            true,
        ),
        (
            "{\"outcome\":\"WAIT_OUTCOME_DEADLINE\"}\n",
            "not_converged",
            true,
        ),
    ] {
        assert_eq!(
            outputs(&waited("success", body, "")),
            want("1.2.3", reason == "converged", reason, fails),
            "{body}"
        );
    }
}

#[test]
fn a_requested_version_that_is_not_a_version_is_never_written() {
    // The version input is the caller's text. Catches it copied into the job's outputs, where a
    // newline would add a line of its own (`reason=converged`): only a version is written.
    let odd = Left {
        version: "1.2.3\nreason=converged",
        set_body: "{\"desired\":{\"version\":\"1.2.3\\nreason=converged\"}}\n",
        wait: "false",
        ..SET
    };
    assert_eq!(outputs(&odd), want("", false, "not_waited", false));
    // A last newline alone, which a `$` of the grammar would match before.
    let trailing = Left {
        version: "1.2.3\n",
        set_body: "{\"desired\":{\"version\":\"1.2.3\\n\"}}\n",
        wait: "false",
        ..SET
    };
    assert_eq!(outputs(&trailing), want("", false, "not_waited", false));
}

/// The request program of the real workflow, given the five texts of the release inputs.
fn request(package_digest: &str, index: &str, index_digest: &str, targets: &str) -> Ran {
    let doc = yaml::load(&real()).expect("the workflow parses");
    let program = request_program(&doc).expect("the request step has its program");
    run(
        program,
        &[
            ("version", "1.2.3".to_owned()),
            ("package_digest", package_digest.to_owned()),
            ("release_index", index.to_owned()),
            ("release_index_digest", index_digest.to_owned()),
            ("targets", targets.to_owned()),
        ],
    )
}

/// The one JSON object a request printed, once it ran to its end.
fn written(ran: Ran) -> serde_json::Value {
    let (printed, ended) = ran;
    assert_eq!(ended, Ended::Done);
    let [line] = printed.as_slice() else {
        panic!("not one line: {printed:?}");
    };
    serde_json::from_str(line).expect("one JSON object")
}

/// Targets of one archive entry, as `cargo xtask release targets` writes them.
const TARGETS: &str = "{\"linux-x86_64\":{\"entries\":[{\"installer\":\"hostkeeper-archive\",\
    \"bytes\":\"7\",\"manifest\":{\"minHostkeeper\":\"0.1.0\"}}]}}";

#[test]
fn the_request_names_the_release_one_way_exactly() {
    // Catches the request losing the version or the digest, the index text changed on its way
    // (it is one JSON string of its exact bytes), and a request sent with neither way of naming
    // the release, or with both (the coordinator would take a set with no digest at all).
    let digest = "a".repeat(64);
    assert_eq!(
        written(request(&digest, "", "", "")),
        serde_json::json!({"desired": {"version": "1.2.3"}, "packageDigest": digest})
    );
    let index = "{\"service\":\"hello\",\n \"note\":\"a \\\"quoted\\\" word\"}\n";
    assert_eq!(
        written(request("", index, &digest, "")),
        serde_json::json!({
            "desired": {"version": "1.2.3"},
            "releaseIndex": index,
            "releaseIndexDigest": digest,
        })
    );
}

#[test]
fn targets_go_into_the_request_as_an_object_alone_or_beside_the_index() {
    // Catches the targets sent as a JSON string (the coordinator reads `targets` as a map and
    // refuses a string), changed on their way (an entry's order or a field lost), the index and
    // its digest dropped from a set that has them (a daemon before targets would then be
    // ordered no archive through the release channel), or a set of targets alone refused.
    let digest = "a".repeat(64);
    let targets: serde_json::Value = serde_json::from_str(TARGETS).expect("JSON");
    assert_eq!(
        written(request("", "", "", TARGETS)),
        serde_json::json!({"desired": {"version": "1.2.3"}, "targets": targets})
    );
    assert_eq!(
        written(request("", "{}", &digest, TARGETS)),
        serde_json::json!({
            "desired": {"version": "1.2.3"},
            "releaseIndex": "{}",
            "releaseIndexDigest": digest,
            "targets": targets,
        })
    );
}

#[test]
fn a_request_with_no_way_a_package_digest_and_another_or_half_an_index_is_refused() {
    // Catches a request sent with no way of naming the release (the coordinator would take a
    // set with no digest at all), the package digest with the index or the targets (refused by
    // the coordinator as `index_invalid`), or half of the index, with or without targets: each
    // stops with jq's error (exit 5), so the step fails before any token exists.
    let digest = "a".repeat(64);
    let refused = (
        Vec::new(),
        Ended::Failed(
            "give package_digest, or release_index with release_index_digest, or targets alone \
             or with both of those; never package_digest with another"
                .to_owned(),
        ),
    );
    let d = digest.as_str();
    for (package, index, index_digest, targets) in [
        ("", "", "", ""),
        (d, "{}", d, ""),
        (d, "{}", "", ""),
        (d, "", d, ""),
        ("", "{}", "", ""),
        ("", "", d, ""),
        (d, "", "", TARGETS),
        (d, "{}", d, TARGETS),
        ("", "{}", "", TARGETS),
        ("", "", d, TARGETS),
    ] {
        assert_eq!(
            request(package, index, index_digest, targets),
            refused,
            "{package:?} {index:?} {index_digest:?} {targets:?}"
        );
    }
}

#[test]
fn targets_that_are_not_one_json_object_are_refused() {
    // Catches targets that are not JSON, a JSON value other than an object (an array, a string,
    // null), or two values sent on, which the coordinator would refuse only once a token was
    // spent, or which jq would write as something other than the map.
    let refused = (
        Vec::new(),
        Ended::Failed("targets is not one JSON object".to_owned()),
    );
    for targets in [
        "not json",
        "[1]",
        "\"{}\"",
        "null",
        "{\"a\":1} {\"b\":2}",
        "{\"a\":",
    ] {
        assert_eq!(request("", "", "", targets), refused, "{targets:?}");
    }
}

#[test]
fn every_pattern_of_the_outputs_program_ends_at_the_end_of_the_text() {
    // Catches a pattern anchored with `$`: jq's regexes (Oniguruma) match `$` before a last
    // newline too, so `"1.2.3\n"` would pass as a version on the runner and add a line to the
    // job's outputs; jaq's `$` does not, so the fixtures alone cannot show it. `\z` is the end of
    // the text in both.
    let doc = yaml::load(&real()).expect("the workflow parses");
    let program = outputs_program(&doc).expect("the outputs step has its program");
    let patterns: Vec<&str> = program
        .split("test(\"")
        .skip(1)
        .filter_map(|rest| rest.split("\")").next())
        .collect();
    assert_eq!(patterns.len(), 2, "{program}");
    for pattern in patterns {
        assert!(pattern.starts_with('^'), "{pattern}");
        assert!(pattern.ends_with("\\\\z"), "{pattern}");
    }
}
