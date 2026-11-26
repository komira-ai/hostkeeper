//! The parts of the farm check that are functions of their input: the reason of the run, the words
//! of a deferral, the verdict of a probe, and the record made of what a Mac reported. The runs
//! through a fake coordinator are in tests/cli/xcode_farm_add.rs, xcode_farm_choose.rs and
//! xcode_farm_verify.rs.

use super::read::{self, Probe, Step};
use super::{
    Found, MAX_REASON_BYTES, Subject, archive_failure, clean, judged, reason_of, record,
    removal_reason, shell_word, skip_token,
};
use crate::Stop;
use crate::xcode_record::{Archive, Farm, FarmStep, Output};

/// A step of a probe.
fn step(name: &str, exit_code: Option<i32>, output: &str) -> Step {
    Step {
        name: name.to_owned(),
        exit_code,
        output: output.to_owned(),
        cut: false,
    }
}

/// A probe of the run `run` with `steps`.
fn probe(run: &str, steps: Vec<Step>) -> Probe {
    Probe {
        run_id: run.to_owned(),
        probed_unix: 1_790_000_000,
        steps,
    }
}

/// The archive of the tests.
fn archive() -> Archive {
    Archive {
        name: "Xcode_27.2.xip".to_owned(),
        size: 10,
        sha256: "3f1c0e9a77b1aabbccddeeff00112233445566778899aabbccddeeff00112233".to_owned(),
        kind: "xar!".to_owned(),
    }
}

/// The check of the archive on `mac-1`, for the release 27.2 (27B52).
fn subject(archive: &Archive) -> Subject<'_> {
    Subject {
        verb: "xcode add",
        mac: "mac-1",
        archive,
        version: "27.2",
        build: "27B52",
    }
}

/// What a Mac's probe said, as `judged` hands it on.
fn found(probe: Option<Probe>, verdict: &str, deferral: &str) -> Found {
    Found {
        probe,
        pass: verdict == "pass",
        reason: String::new(),
        verdict: verdict.to_owned(),
        deferral: deferral.to_owned(),
    }
}

/// Proves: a reason has no control character, is trimmed, and is cut at 256 bytes without cutting a
/// character in two, so the coordinator always takes it; and a reason that is short is as it was.
#[test]
fn a_reason_is_one_clean_line_of_at_most_256_bytes() {
    assert_eq!(clean("  check Xcode\n\t27.2\u{7f}  "), "check Xcode27.2");
    let long = "é".repeat(200);
    let cut = clean(&long);
    assert_eq!(cut.len(), MAX_REASON_BYTES);
    assert!(cut.chars().all(|c| c == 'é'));
    assert_eq!(clean(&"a".repeat(300)).len(), MAX_REASON_BYTES);
    assert_eq!(clean("fine"), "fine");
}

/// Proves: the reason of the run names the verb, the release and the first 12 hex digits of the
/// archive's SHA-256, and stays a reason the coordinator takes when the release's own text is long;
/// the reason of a removal names the Mac, the run and what failed.
#[test]
fn the_reasons_name_the_release_the_digits_the_mac_and_the_run() {
    let archive = archive();
    assert_eq!(
        reason_of(&subject(&archive)),
        "hk xcode add: check Xcode 27.2 (27B52), sha256 3f1c0e9a77b1"
    );
    let huge = "9".repeat(400);
    let long = Subject {
        version: &huge,
        ..subject(&archive)
    };
    assert_eq!(reason_of(&long).len(), MAX_REASON_BYTES);
    assert_eq!(
        removal_reason("mac-1", "run-7", "xcode_signature_invalid"),
        "hk xcode add: the check on mac-1 (run run-7) failed: xcode_signature_invalid"
    );
}

/// Proves: a path is one word of a shell command as it is when it holds only plain characters, and
/// in single quotes, with a quote inside written as `'\''`, when it holds a space, a quote or
/// anything else; an empty one is `''`.
#[test]
fn a_path_is_a_word_a_shell_takes_whole() {
    assert_eq!(
        shell_word("/scratch/op/Downloads/Xcode_27.2.xip"),
        "/scratch/op/Downloads/Xcode_27.2.xip"
    );
    assert_eq!(shell_word("Xcode-27.2+beta.xip"), "Xcode-27.2+beta.xip");
    assert_eq!(
        shell_word("/scratch/op/My Downloads/x.xip"),
        "'/scratch/op/My Downloads/x.xip'"
    );
    assert_eq!(shell_word("it's.xip"), "'it'\\''s.xip'");
    assert_eq!(shell_word("a;b"), "'a;b'");
    assert_eq!(shell_word(""), "''");
}

/// Proves: a deferral is an archive failure only when the token after `exit 75, ` is
/// `xcode_archive_mismatch` or `xcode_archive_encrypted`: its detail is the text after `: `, empty
/// when there is none; any other token, a reason with no `exit 75, ` and one with no token are not.
#[test]
fn a_deferral_names_the_archive_only_by_its_two_tokens() {
    assert_eq!(
        archive_failure("stage: exit 75, xcode_archive_mismatch: size 5, not 6"),
        Some((
            "xcode_archive_mismatch".to_owned(),
            "size 5, not 6".to_owned()
        ))
    );
    assert_eq!(
        archive_failure("stage: exit 75, xcode_archive_encrypted"),
        Some(("xcode_archive_encrypted".to_owned(), String::new()))
    );
    assert_eq!(
        archive_failure("stage: exit 75, xcode_archive_encrypted detail with no colon"),
        Some(("xcode_archive_encrypted".to_owned(), String::new()))
    );
    for other in [
        "stage: exit 75, free_space_short: 3 GB",
        "stage: exit 75, xcode_archive_mismatched: x",
        "stage: exit 75, : nothing",
        "stage: exit 75, ",
        "stage: exit 1, xcode_archive_mismatch: x",
        "xcode_archive_mismatch: x",
        "",
    ] {
        assert_eq!(archive_failure(other), None, "{other}");
    }
}

/// Proves: a skip is named as the coordinator's token, in lower case, and a name with no prefix is
/// kept as it is.
#[test]
fn a_skip_is_named_by_its_token() {
    assert_eq!(
        skip_token("MAINTENANCE_SKIP_NO_UPDATE_ACCOUNT"),
        "no_update_account"
    );
    assert_eq!(skip_token(""), "");
    assert_eq!(skip_token("HELD"), "held");
}

/// Proves: the verdict step is read as the daemon writes it: exit 0 with `pass` is a pass; exit 1 is
/// a failure whose reason is the output before its first `: ` (the whole output when it has none);
/// the probe of another run, one with no verdict step, no exit, another exit, and exit 0 with other
/// words are not a verdict.
#[test]
fn the_verdict_step_of_the_probe_is_the_verdict() {
    let pass = judged(probe("r", vec![step("verdict", Some(0), "pass")]), "r", "m").expect("pass");
    assert!(pass.pass);
    assert_eq!((pass.reason.as_str(), pass.verdict.as_str()), ("", "pass"));
    let failed = judged(
        probe(
            "r",
            vec![step(
                "verdict",
                Some(1),
                "xcode_build_mismatch: the app is X",
            )],
        ),
        "r",
        "m",
    )
    .expect("a failure");
    assert!(!failed.pass);
    assert_eq!(failed.reason, "xcode_build_mismatch");
    assert_eq!(failed.verdict, "fail: xcode_build_mismatch: the app is X");
    let bare = judged(
        probe("r", vec![step("verdict", Some(1), "xcode_expand_failed")]),
        "r",
        "m",
    )
    .expect("a failure");
    assert_eq!(bare.reason, "xcode_expand_failed");
    let older = "m's probe is not an archive check of run r: its hostkeeper made its reads, which \
                 means it is older than this hk, or the store no longer held the archive when its \
                 turn started";
    for (made, run) in [
        (probe("other", vec![step("verdict", Some(0), "pass")]), "r"),
        (probe("r", vec![step("sw_vers", Some(0), "")]), "r"),
        (probe("r", vec![]), "r"),
    ] {
        assert_eq!(judged(made, run, "m").err().as_deref(), Some(older));
    }
    let unfinished = judged(
        probe(
            "r",
            vec![step("verdict", None, "xcode_check_unfinished: killed")],
        ),
        "r",
        "m",
    );
    assert_eq!(
        unfinished.err().as_deref(),
        Some("m's check did not finish (run r): xcode_check_unfinished: killed")
    );
    for (exit, output) in [(Some(2), "odd"), (Some(0), "fine"), (Some(-1), "")] {
        let said = judged(probe("r", vec![step("verdict", exit, output)]), "r", "m");
        assert!(
            said.err()
                .is_some_and(|why| why.contains("which this hk does not read")),
            "{exit:?}"
        );
    }
}

/// Proves: the record of a pass holds the Mac's `sw_vers` and `uname` as the macOS section, the
/// expansion of the `xip`, `app`, `version` and `codesign` steps, every step under `farm`, the
/// Mac and the run, and the archive; the sections no step gives are null.
#[test]
fn the_record_of_a_probe_is_the_probe_in_the_sections_that_have_a_place_for_it() {
    let archive = archive();
    let steps = vec![
        step("sw_vers", Some(0), "ProductName: macOS"),
        step("uname", Some(0), "Darwin arm64"),
        step("xip", Some(0), "expanded"),
        step("app", None, "Xcode.app, Xcode-beta.app"),
        step("version", None, "27.2 27B52"),
        step("codesign", Some(3), "rejected"),
        step("verdict", Some(0), "pass"),
    ];
    let made = record::build(
        "1.2.3",
        &subject(&archive),
        "r",
        &found(Some(probe("r", steps)), "pass", ""),
    );
    assert_eq!(made.record, "hk-xcode-verify-v1");
    assert_eq!(made.hk, "1.2.3");
    assert_eq!(made.macos.sw_vers.stdout, "ProductName: macOS");
    assert_eq!(made.macos.uname.exit, Some(0));
    assert!(made.programs.is_none() && made.signature.is_none());
    assert!(made.tamper.is_none() && made.toc.is_none());
    let expansion = made.expansion.expect("an expansion");
    assert_eq!(expansion.exit, Some(0));
    assert_eq!(expansion.output.as_deref(), Some("expanded"));
    assert_eq!(expansion.apps, ["Xcode.app", "Xcode-beta.app"]);
    assert_eq!(expansion.version.as_deref(), Some("27.2"));
    assert_eq!(expansion.build.as_deref(), Some("27B52"));
    let codesign: Output = expansion.codesign.expect("codesign");
    assert_eq!(
        (codesign.exit, codesign.stdout.as_str()),
        (Some(3), "rejected")
    );
    let farm: Farm = made.farm.expect("farm");
    assert_eq!((farm.host.as_str(), farm.run.as_str()), ("mac-1", "r"));
    assert_eq!(farm.probed_unix, Some(1_790_000_000));
    assert_eq!(farm.steps.len(), 7);
    assert_eq!(
        farm.steps.last(),
        Some(&FarmStep {
            name: "verdict".to_owned(),
            exit: Some(0),
            output: "pass".to_owned(),
            cut: false,
        })
    );
    assert!(farm.reason.is_empty());
    assert_eq!(made.archive.sha256, archive.sha256);
}

/// Proves: what a step does not give is not made up: with no `xip` step there is no expansion; with
/// one, an `app` step of `none` or none at all is no app, a `version` step that is not two words of
/// a release (a reason, three words, a version that is no number) gives neither, and no
/// `codesign` step is no codesign; a step the Mac did not report is `not reported by <mac>`.
#[test]
fn what_a_step_does_not_give_is_left_out_of_the_record() {
    let archive = archive();
    let none = record::build(
        "h",
        &subject(&archive),
        "r",
        &found(
            Some(probe("r", vec![step("pkgutil", Some(1), "x")])),
            "fail: x",
            "",
        ),
    );
    assert!(none.expansion.is_none());
    assert_eq!(none.macos.sw_vers.stderr, "not reported by mac-1");
    assert_eq!(none.macos.uname.stdout, "");
    for (apps, version) in [
        ("none", "no version.plist"),
        ("", "27.2 27B52 extra"),
        ("none", "27.x 27B52"),
        ("none", "27.2 not/a/build"),
        ("none", "27.2"),
        ("none", ""),
    ] {
        let steps = vec![
            step("xip", Some(0), ""),
            step("app", None, apps),
            step("version", None, version),
        ];
        let made = record::build(
            "h",
            &subject(&archive),
            "r",
            &found(Some(probe("r", steps)), "fail: y", ""),
        );
        let expansion = made.expansion.expect("an expansion");
        assert!(expansion.apps.is_empty(), "{apps}");
        assert_eq!(
            (expansion.version, expansion.build),
            (None, None),
            "{version}"
        );
        assert!(expansion.codesign.is_none());
    }
    let bare = vec![step("xip", Some(1), "xip: failed")];
    let made = record::build(
        "h",
        &subject(&archive),
        "r",
        &found(Some(probe("r", bare)), "fail: z", ""),
    );
    let expansion = made.expansion.expect("an expansion");
    assert!(expansion.apps.is_empty());
    assert_eq!((expansion.version, expansion.build), (None, None));
}

/// Proves: the record of a deferred turn has no probe: `probed_unix` is null, there are no steps,
/// the Mac's reason is in `farm`, the macOS section says the Mac reported nothing, and there is no
/// expansion.
#[test]
fn the_record_of_a_deferral_holds_the_reason_and_no_steps() {
    let archive = archive();
    let reason = "stage: exit 75, xcode_archive_mismatch: size";
    let made = record::build(
        "h",
        &subject(&archive),
        "r",
        &found(None, "fail: xcode_archive_mismatch: size", reason),
    );
    assert!(made.expansion.is_none());
    let farm = made.farm.expect("farm");
    assert_eq!(farm.probed_unix, None);
    assert!(farm.steps.is_empty());
    assert_eq!(farm.reason, reason);
    assert_eq!(made.macos.sw_vers.stderr, "not reported by mac-1");
    assert_eq!(made.verdict, "fail: xcode_archive_mismatch: size");
}

/// Proves: a probe is read as the gateway writes it: `probedUnix` a string, or a number, an exit code
/// left out when the program did not exit by itself, and every field left out as empty; text that
/// is no number for `probedUnix` is an error.
#[test]
fn a_probe_is_read_as_the_gateway_writes_it() {
    let text = r#"{"runId":"r","probedUnix":"1790000000","steps":[
        {"name":"xip","exitCode":0,"output":"ok"},{"name":"app","output":"Xcode.app","cut":true}]}"#;
    let read: Probe = serde_json::from_str(text).expect("a probe");
    assert_eq!(
        (read.run_id.as_str(), read.probed_unix),
        ("r", 1_790_000_000)
    );
    assert_eq!(read.steps.len(), 2);
    assert_eq!(read.step("xip").map(|s| s.exit_code), Some(Some(0)));
    let app = read.step("app").expect("app");
    assert_eq!(
        (app.exit_code, app.output.as_str(), app.cut),
        (None, "Xcode.app", true)
    );
    assert!(read.step("verdict").is_none());
    let number: Probe = serde_json::from_str(r#"{"probedUnix":42}"#).expect("a number");
    assert_eq!((number.probed_unix, number.run_id.as_str()), (42, ""));
    assert!(serde_json::from_str::<Probe>(r#"{"probedUnix":"soon"}"#).is_err());
}

/// Proves: what a stopped call says is its message, or its exit when it has none.
#[test]
fn a_stopped_call_says_its_message_or_its_exit() {
    assert_eq!(read::why(&Stop::fail("no answer")), "no answer");
    assert_eq!(read::why(&Stop::usage("a flag")), "a flag");
    assert_eq!(read::why(&Stop::Exit(3)), "it exited 3");
}
