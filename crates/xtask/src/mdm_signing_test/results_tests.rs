//! The tests of `record`, `report` and the verdict of a run.

use std::path::Path;

use super::results::{Answer, Outcome, RESULTS_FILE, Results, cleanup, record, report, verdict};
use super::tests::{FakeHub, kit};
use super::{Error, PKGS, Step, Verb, run};
use crate::release::fakes::scratch;
use crate::release::ports::{Exec, Ran};

struct NoExec;

impl Exec for NoExec {
    fn spawn(&self, program: &str, _: &[String]) -> std::io::Result<Ran> {
        Err(std::io::Error::other(format!("ran {program}")))
    }
}

fn outcome(answer: Answer, installed: Option<bool>) -> Outcome {
    Outcome {
        answer,
        installed,
        trusted: None,
        note: None,
    }
}

/// A run with each pkg sent once and recorded `Acknowledged` with `installed[n]`, pkg 3 with
/// `trusted`.
fn run_of(installed: [bool; 4], trusted: Option<bool>) -> Results {
    let mut results = Results::default();
    results.add_send(Step::RootProfile, "R", None);
    results.sends[0].outcome = Some(outcome(Answer::Acknowledged, None));
    for (spec, got) in PKGS.iter().zip(installed) {
        results.add_send(spec.step, &format!("U{}", spec.number), None);
        let mut o = outcome(Answer::Acknowledged, Some(got));
        if spec.step == Step::Pkg3 {
            o.trusted = trusted;
        }
        results.sends.last_mut().expect("a send").outcome = Some(o);
    }
    results
}

/// Records through the verb, as the command line does.
fn record_verb(
    dir: &Path,
    step: Step,
    answer: Answer,
    installed: Option<bool>,
) -> Result<Vec<String>, Error> {
    run(
        Verb::Record {
            kit: dir.to_path_buf(),
            step,
            answer,
            installed,
            trusted: None,
            note: None,
        },
        &NoExec,
        |_, _| FakeHub::default(),
    )
}

#[test]
fn the_verdict_reads_the_run_as_section_7_2_does() {
    // Catches: a verdict that calls the farm identity good when pkg 4 also installs (the Mac
    // did not judge the chain), calls for Developer ID when the Mac never trusted pkg 3's
    // chain, or drops the line saying an unsigned pkg installed.
    let farm = "Verdict: farm identity: pkg 3 installs and pkg 4 does not; the root's payload is \
                what makes it pass. Task M15 builds `hostkeeper-mdm pkg add`";
    let developer_id = "Verdict: Developer ID Installer: the Mac trusts pkg 3's chain and still \
                        does not install it. Task M15 builds the Developer ID job; question Q4 \
                        asks for the identity";
    let no_chain = "Verdict: inconclusive: pkg 4 installed although its root is installed \
                    nowhere, so the Mac did not judge the chain and pkg 3 proves nothing";
    let untrusted = "Verdict: inconclusive: pkg 3 did not install and the Mac was not shown to \
                     trust its chain with the root installed (record --trusted from pkgutil \
                     --check-signature); the identity, not the MDM, may be what is refused";
    let unsigned = |n: u8, what: &str| {
        format!("Also: pkg {n}, an {what}, installed: this Mac installs an unsigned pkg by MDM")
    };
    assert_eq!(verdict(&run_of([false, false, true, false], None)), [farm]);
    assert_eq!(
        verdict(&run_of([false, false, false, false], Some(true))),
        [developer_id]
    );
    assert_eq!(
        verdict(&run_of([false, false, false, false], Some(false))),
        [untrusted]
    );
    assert_eq!(
        verdict(&run_of([false, false, false, false], None)),
        [untrusted]
    );
    assert_eq!(
        verdict(&run_of([false, false, true, true], None)),
        [no_chain]
    );
    assert_eq!(
        verdict(&run_of([true, true, true, true], None)),
        [
            no_chain.to_owned(),
            unsigned(1, "unsigned component pkg"),
            unsigned(2, "unsigned distribution pkg"),
        ]
    );
    assert_eq!(
        verdict(&run_of([false, true, true, false], None)),
        [farm.to_owned(), unsigned(2, "unsigned distribution pkg")]
    );
}

#[test]
fn a_run_with_a_pkg_unrecorded_or_its_command_refused_decides_nothing() {
    // Catches: a missing pkg read as not installed, or a command the Mac refused (a manifest
    // fault, not a signature) read as a refusal of the signature.
    let mut results = run_of([false, false, false, false], Some(true));
    results.sends[4].outcome = None;
    assert_eq!(
        verdict(&results),
        ["Verdict: incomplete: pkg-4 has no recorded outcome"]
    );
    let mut results = run_of([false, false, false, false], Some(true));
    results.sends[3].outcome = Some(outcome(Answer::CommandFormatError, Some(false)));
    assert_eq!(
        verdict(&results),
        [
            "Verdict: inconclusive: the Mac answered pkg-3 pkg 3 with CommandFormatError: the \
             command, not the signature, was refused; read its answer and send it again"
        ]
    );
    // The last send of a step is the one read: a re-send that was acknowledged decides.
    results.add_send(Step::Pkg3, "U3b", None);
    results.sends.last_mut().expect("send").outcome = Some(Outcome {
        trusted: Some(true),
        ..outcome(Answer::Acknowledged, Some(true))
    });
    assert!(verdict(&results)[0].starts_with("Verdict: farm identity"));
}

#[test]
fn record_updates_the_last_send_of_the_step() {
    // Catches: an outcome written to the first send of a step that was sent twice, or a second
    // record adding an entry instead of replacing the first.
    let dir = kit("signing-record");
    let hub = FakeHub::default();
    for step in [Step::Pkg1, Step::Pkg1] {
        let (id, host) = (
            "M".parse().expect("id"),
            "mdm.example.net".parse().expect("host"),
        );
        let target = super::Target {
            enrollment_id: &id,
            mdm_host: &host,
        };
        super::send(&dir, step, &target, Some(&hub)).expect("sent");
    }
    let lines = record_verb(&dir, Step::Pkg1, Answer::NotNow, Some(false)).expect("recorded");
    let uuid = Results::read(&dir).expect("results").sends[1]
        .command_uuid
        .clone();
    assert_eq!(lines, [format!("recorded pkg-1 (command {uuid})")]);
    record_verb(&dir, Step::Pkg1, Answer::Acknowledged, Some(true)).expect("recorded");
    let results = Results::read(&dir).expect("results");
    assert_eq!(results.sends.len(), 2);
    assert_eq!(results.sends[0].outcome, None);
    assert_eq!(
        results.sends[1].outcome,
        Some(outcome(Answer::Acknowledged, Some(true)))
    );
}

#[test]
fn record_refuses_what_does_not_fit_the_step() {
    // Catches: a pkg recorded without whether it installed (which the verdict needs), an
    // install recorded for a profile, --trusted for a pkg other than 3, or an outcome for a
    // step never sent.
    let dir = kit("signing-record-bad");
    let refused = |step, o: Outcome| match record(&dir, step, o) {
        Err(Error::Refused(why)) => why,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        refused(Step::Pkg2, outcome(Answer::Acknowledged, None)),
        "pkg-2 needs --installed: whether the Mac holds the pkg's receipt"
    );
    assert_eq!(
        refused(Step::RootProfile, outcome(Answer::Acknowledged, Some(true))),
        "root-profile installs no pkg: --installed is for pkg-1 to pkg-4"
    );
    assert_eq!(
        refused(
            Step::Pkg4,
            Outcome {
                trusted: Some(true),
                ..outcome(Answer::Acknowledged, Some(false))
            }
        ),
        "--trusted is for pkg-3 alone: the Mac's reading of its chain with the root installed"
    );
    assert_eq!(
        refused(Step::Pkg1, outcome(Answer::Acknowledged, Some(true))),
        "pkg-1 has not been sent: `send --apply` it first"
    );
    assert!(!dir.join(RESULTS_FILE).exists());
}

#[test]
fn the_report_is_the_run_as_a_table_and_its_verdict() {
    // Catches: a send missing from the table, a pipe in a note breaking the table, the push
    // failure or the Mac's reading of the chain left out, or the verdict not printed.
    let dir = scratch("signing-report");
    let mut results = run_of([false, false, true, false], Some(true));
    results.sends[1].push_error = Some("no push".to_owned());
    results.sends[4].outcome.as_mut().expect("o").note = Some("install log | refused".to_owned());
    results.add_send(Step::RemoveRootProfile, "X", None);
    results.write(&dir).expect("written");
    let lines = run(
        Verb::Report {
            kit: dir.to_path_buf(),
        },
        &NoExec,
        |_, _| FakeHub::default(),
    )
    .expect("report");
    assert_eq!(
        lines,
        [
            "| Step | Command | Answer | Installed | Note |",
            "|---|---|---|---|---|",
            "| root-profile | R | Acknowledged |  |  |",
            "| pkg-1 | U1 | Acknowledged | no | push failed: no push. |",
            "| pkg-2 | U2 | Acknowledged | no |  |",
            "| pkg-3 | U3 | Acknowledged | yes | chain trusted on the Mac. |",
            "| pkg-4 | U4 | Acknowledged | no | install log / refused |",
            "| remove-root-profile | X | not recorded |  |  |",
            "",
            "Verdict: farm identity: pkg 3 installs and pkg 4 does not; the root's payload is \
             what makes it pass. Task M15 builds `hostkeeper-mdm pkg add`",
            "Cleanup: root A's profile is still on the Mac: send remove-root-profile and record \
             it Acknowledged",
        ]
    );
    let mut results = run_of([false, false, false, false], Some(false));
    results.sends[3].outcome.as_mut().expect("o").note = Some("refused".to_owned());
    let dir = scratch("signing-report-untrusted");
    results.write(&dir).expect("written");
    let lines = report(&dir).expect("report");
    assert_eq!(
        lines[5],
        "| pkg-3 | U3 | Acknowledged | no | chain not trusted on the Mac. refused |"
    );
}

#[test]
fn a_results_file_that_does_not_parse_is_refused_and_none_is_an_empty_run() {
    // Catches: a hand-edited results.toml read as a run with fewer sends, or a missing one
    // refused before the first send.
    let dir = scratch("signing-results");
    assert_eq!(Results::read(&dir), Ok(Results::default()));
    assert_eq!(
        report(&dir).expect("report").last().map(String::as_str),
        Some("Verdict: incomplete: pkg-1 has no recorded outcome")
    );
    std::fs::write(dir.join(RESULTS_FILE), "[[send]]\nstep = \"pkg-9\"\n").expect("write");
    assert!(matches!(Results::read(&dir), Err(Error::Refused(why)) if why.contains(RESULTS_FILE)));
    std::fs::remove_file(dir.join(RESULTS_FILE)).expect("remove");
    std::fs::create_dir(dir.join(RESULTS_FILE)).expect("a directory in its place");
    assert!(matches!(Results::read(&dir), Err(Error::Io { .. })));
    assert!(matches!(
        Results::default().write(&dir),
        Err(Error::Io { .. })
    ));
}

#[test]
fn the_build_verb_reaches_build() {
    // Catches: the verb's flags not reaching `build` (the kit's directory, the identifier).
    let dir = scratch("signing-build-verb");
    let got = run(
        Verb::Build {
            out: dir.to_path_buf(),
            profile_identifier: "com.example.x".parse().expect("id"),
        },
        &NoExec,
        |_, _| FakeHub::default(),
    );
    assert_eq!(
        got,
        Err(Error::Refused(format!(
            "{} exists: the kit is built into a new directory",
            dir.display()
        )))
    );
}

#[test]
fn the_report_says_when_root_a_is_left_on_the_mac() {
    // Catches: a run that ends with a verdict while the test root is still trusted on the Mac
    // system-wide, or a removal before the last install counted as the clean-up.
    let still = Some(
        "Cleanup: root A's profile is still on the Mac: send remove-root-profile and record it \
         Acknowledged"
            .to_owned(),
    );
    assert_eq!(cleanup(&Results::default()), None);
    let mut results = run_of([false, false, true, false], None);
    assert_eq!(cleanup(&results), still);
    results.add_send(Step::RemoveRootProfile, "X", None);
    assert_eq!(cleanup(&results), still);
    results.sends.last_mut().expect("send").outcome = Some(outcome(Answer::Acknowledged, None));
    assert_eq!(cleanup(&results), None);
    results.add_send(Step::RootProfile, "R2", None);
    results.sends.last_mut().expect("send").outcome = Some(outcome(Answer::Acknowledged, None));
    assert_eq!(cleanup(&results), still);
}

#[test]
fn an_answer_is_reported_by_its_flag_name() {
    // Catches: the report printing an answer in another spelling than the one `record` takes.
    use clap::ValueEnum;
    for answer in Answer::value_variants() {
        let flag = answer.to_possible_value().expect("a value");
        assert_eq!(flag.get_name(), answer.name());
    }
}
