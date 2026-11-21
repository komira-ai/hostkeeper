//! The code-signature check: the output of a program signed as the release signs it passes, each
//! way it differs is refused with its own line, and `codesign` is run as the check needs.

use std::path::Path;

use super::*;
use crate::release::fakes::{FakeCodeSign, FakeExec};
use crate::release::mac_programs::MAC_PROGRAMS;
use crate::release::ports::Adapters;
use crate::release::tests::{empty, verb};

const BINARY: &str = "unpacked/hostkeeper";

/// What `codesign --display --verbose=2` prints (to stderr) of hostkeeper signed by the release.
const DISPLAY: &str = "\
Executable=/Users/runner/unpacked/hostkeeper
Identifier=dev.hostkeeper.hostkeeper
Format=Mach-O thin (arm64)
CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=30+2 location=embedded
Signature size=9012
Authority=Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)
Authority=Developer ID Certification Authority
Authority=Apple Root CA
Timestamp=Nov 21, 2026 at 10:00:00
Info.plist entries=3
TeamIdentifier=U7RTX4XD3X
Sealed Resources=none
Internal requirements count=1 size=200
";

/// What `codesign --display --verbose=2` prints of a program the linker signed ad hoc.
const ADHOC: &str = "\
Executable=/Users/runner/unpacked/hostkeeper
Identifier=hostkeeper-5555555555555555
Format=Mach-O thin (arm64)
CodeDirectory v=20400 size=800 flags=0x20002(adhoc,linker-signed) hashes=20+0 location=embedded
Signature=adhoc
Info.plist=not bound
TeamIdentifier=not set
Sealed Resources=none
Internal requirements=none
";

fn requirements(program: &str) -> String {
    let want = MacProgram::named(program).expect("a program").requirement();
    format!("Executable=/Users/runner/unpacked/{program}\ndesignated => {want}\n")
}

fn ran(success: bool, text: &str) -> Result<Codesigned, PortError> {
    Ok(Codesigned {
        success,
        text: text.to_owned(),
    })
}

/// A `codesign` whose three answers are given, `--verify` succeeding.
fn tools(display: &str, requirements: &str) -> FakeCodeSign {
    FakeCodeSign {
        verify: Some(ran(true, "unpacked/hostkeeper: valid on disk\n")),
        display: Some(ran(true, display)),
        requirements: Some(ran(true, requirements)),
        ..FakeCodeSign::default()
    }
}

/// [`DISPLAY`] with the one occurrence of `from` replaced by `to`.
fn display_with(from: &str, to: &str) -> String {
    assert_eq!(DISPLAY.matches(from).count(), 1, "{from:?}");
    DISPLAY.replacen(from, to, 1)
}

fn refused(display: &str, requirements: &str) -> Vec<String> {
    match check(
        &tools(display, requirements),
        Path::new(BINARY),
        "hostkeeper",
    ) {
        Err(Error::Refused(lines)) => lines,
        other => panic!("not refused: {other:?}"),
    }
}

fn line(problem: &str) -> String {
    format!("{BINARY}: {problem}")
}

#[test]
fn a_program_signed_as_the_release_signs_it_passes() {
    // Catches: the parser refusing what the release's own signature prints, or the check not
    // reading the three codesign answers.
    let t = tools(DISPLAY, &requirements("hostkeeper"));
    assert_eq!(
        check(&t, Path::new(BINARY), "hostkeeper"),
        Ok(vec![format!(
            "{BINARY}: dev.hostkeeper.hostkeeper signed by Developer ID Application: Komira AI, Inc. (U7RTX4XD3X) under Developer ID Certification Authority and Apple Root CA, with the hardened runtime and a trusted timestamp"
        )])
    );
    assert_eq!(
        t.calls.borrow().as_slice(),
        [
            format!("verify {BINARY}"),
            format!("display {BINARY}"),
            format!("requirements {BINARY}")
        ]
    );
}

#[test]
fn every_program_of_the_table_passes_under_its_own_identifier_and_no_other() {
    // Catches: a check that holds every program to hostkeeper's identifier or requirement.
    for program in MAC_PROGRAMS {
        let display = DISPLAY.replace("dev.hostkeeper.hostkeeper", program.identifier);
        let t = tools(&display, &requirements(program.name));
        assert!(
            check(&t, Path::new(BINARY), program.name).is_ok(),
            "{}",
            program.name
        );
        let other = if program.name == "hk" { "hello" } else { "hk" };
        assert!(
            check(&t, Path::new(BINARY), other).is_err(),
            "{} read as {other}",
            program.name
        );
    }
}

#[test]
fn a_name_outside_the_table_is_refused_before_codesign_runs() {
    // Catches: a program name that is not in the table reaching codesign.
    let t = tools(DISPLAY, &requirements("hostkeeper"));
    assert_eq!(
        check(&t, Path::new(BINARY), "gh"),
        Err(Error::Failed(
            "--program \"gh\" is not a program of the release's table".to_owned()
        ))
    );
    assert!(t.calls.borrow().is_empty());
}

#[test]
fn an_ad_hoc_signature_is_refused_by_name_with_its_identifier_team_chain_timestamp_and_runtime() {
    // Catches: the linker's ad hoc signature passing as a release signature.
    assert_eq!(
        refused(ADHOC, &requirements("hostkeeper")),
        vec![
            line("the signature is ad hoc, not a Developer ID signature"),
            line(
                "the identifier is [\"hostkeeper-5555555555555555\"], not \"dev.hostkeeper.hostkeeper\""
            ),
            line("the team is [\"not set\"], not \"U7RTX4XD3X\""),
            line(
                "the certificate chain is [], not [\"Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)\", \"Developer ID Certification Authority\", \"Apple Root CA\"]"
            ),
            line("the signature has no secure timestamp (no Timestamp= line)"),
            line("the hardened runtime is not on (no runtime flag in the CodeDirectory flags)"),
        ]
    );
}

#[test]
fn another_identifier_is_refused() {
    // Catches: a program signed under an identifier the privacy profile does not name.
    let display = display_with(
        "Identifier=dev.hostkeeper.hostkeeper",
        "Identifier=hostkeeper",
    );
    assert_eq!(
        refused(&display, &requirements("hostkeeper")),
        vec![line(
            "the identifier is [\"hostkeeper\"], not \"dev.hostkeeper.hostkeeper\""
        )]
    );
}

#[test]
fn another_team_is_refused() {
    // Catches: a program signed by a team other than the release's.
    let display = display_with("TeamIdentifier=U7RTX4XD3X", "TeamIdentifier=ABCDE12345");
    assert_eq!(
        refused(&display, &requirements("hostkeeper")),
        vec![line("the team is [\"ABCDE12345\"], not \"U7RTX4XD3X\"")]
    );
}

#[test]
fn another_signing_identity_is_refused() {
    // Catches: a Developer ID identity of another organisation passing because only the team
    // line was read, or a chain that does not end at Apple's root.
    let display = display_with(
        "Authority=Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)",
        "Authority=Developer ID Application: Other Org (ABCDE12345)",
    );
    assert_eq!(
        refused(&display, &requirements("hostkeeper")),
        vec![line(
            "the certificate chain is [\"Developer ID Application: Other Org (ABCDE12345)\", \"Developer ID Certification Authority\", \"Apple Root CA\"], not [\"Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)\", \"Developer ID Certification Authority\", \"Apple Root CA\"]"
        )]
    );
}

#[test]
fn a_signature_without_a_secure_timestamp_is_refused() {
    // Catches: a signature made with --timestamp=none, which stops being valid when the
    // certificate expires.
    let display = display_with(
        "Timestamp=Nov 21, 2026 at 10:00:00\n",
        "Signed Time=Nov 21, 2026\n",
    );
    assert_eq!(
        refused(&display, &requirements("hostkeeper")),
        vec![line(
            "the signature has no secure timestamp (no Timestamp= line)"
        )]
    );
}

#[test]
fn a_signature_without_the_hardened_runtime_is_refused() {
    // Catches: a signature made without --options runtime, whether its flags are 0x0(none) or
    // name other flags, or have no names at all.
    for flags in ["flags=0x0(none)", "flags=0x2(adhoc)", "flags=0x0"] {
        let display = display_with("flags=0x10000(runtime)", flags);
        assert_eq!(
            refused(&display, &requirements("hostkeeper")),
            vec![line(
                "the hardened runtime is not on (no runtime flag in the CodeDirectory flags)"
            )],
            "{flags}"
        );
    }
}

#[test]
fn the_runtime_flag_among_other_flags_is_seen() {
    // Catches: a runtime flag listed with others (`library-validation`) read as absent.
    let display = display_with(
        "flags=0x10000(runtime)",
        "flags=0x12000(library-validation,runtime)",
    );
    assert!(
        check(
            &tools(&display, &requirements("hostkeeper")),
            Path::new(BINARY),
            "hostkeeper"
        )
        .is_ok()
    );
}

#[test]
fn a_requirement_that_pins_a_hash_is_refused() {
    // Catches: a requirement with a cdhash, which names one build and so would stop matching
    // the privacy profile at the next release.
    let pinned = "designated => cdhash H\"0123456789abcdef0123456789abcdef01234567\"\n";
    assert_eq!(
        refused(DISPLAY, pinned),
        vec![line(&format!(
            "the designated requirement is [\"cdhash H\\\"0123456789abcdef0123456789abcdef01234567\\\"\"], not {:?}",
            MacProgram::named("hostkeeper")
                .expect("a program")
                .requirement()
        ))]
    );
}

#[test]
fn a_program_with_no_designated_requirement_is_refused() {
    // Catches: output with no requirement line (an unsigned program) passing.
    assert_eq!(
        refused(DISPLAY, "Executable=/x\n"),
        vec![line(&format!(
            "the designated requirement is [], not {:?}",
            MacProgram::named("hostkeeper")
                .expect("a program")
                .requirement()
        ))]
    );
}

#[test]
fn a_failed_verify_is_refused_with_what_codesign_said() {
    // Catches: a program whose signature does not verify, or a failed codesign taken as success.
    let mut t = tools(DISPLAY, &requirements("hostkeeper"));
    t.verify = Some(ran(
        false,
        "\nunpacked/hostkeeper: a sealed resource is missing\n",
    ));
    assert_eq!(
        check(&t, Path::new(BINARY), "hostkeeper"),
        Err(Error::Refused(vec![line(
            "codesign --verify fails: unpacked/hostkeeper: a sealed resource is missing"
        )]))
    );
}

#[test]
fn an_unsigned_program_is_refused_naming_each_codesign_that_failed() {
    // Catches: an unsigned program whose failed display and requirements runs are skipped or
    // read as empty-but-fine.
    let t = FakeCodeSign {
        verify: Some(ran(
            false,
            "unpacked/hostkeeper: code object is not signed at all\n",
        )),
        display: Some(ran(
            false,
            "unpacked/hostkeeper: code object is not signed at all\n",
        )),
        requirements: Some(ran(
            false,
            "unpacked/hostkeeper: code object is not signed at all\n",
        )),
        ..FakeCodeSign::default()
    };
    assert_eq!(
        check(&t, Path::new(BINARY), "hostkeeper"),
        Err(Error::Refused(vec![
            line("codesign --verify fails: unpacked/hostkeeper: code object is not signed at all"),
            line("codesign --display fails: unpacked/hostkeeper: code object is not signed at all"),
            line(
                "codesign --display --requirements fails: unpacked/hostkeeper: code object is not signed at all"
            ),
        ]))
    );
}

#[test]
fn a_codesign_that_cannot_run_is_an_error_of_the_port_not_a_pass() {
    // Catches: a missing codesign (a check run off a Mac) taken as a program with no problems.
    let t = FakeCodeSign::default();
    assert_eq!(
        check(&t, Path::new(BINARY), "hostkeeper"),
        Err(Error::Port(PortError::new("codesign", "not scripted")))
    );
    let mut t = tools(DISPLAY, "");
    t.display = None;
    assert_eq!(
        check(&t, Path::new(BINARY), "hostkeeper"),
        Err(Error::Port(PortError::new("codesign", "not scripted")))
    );
    let mut t = tools(DISPLAY, "");
    t.requirements = None;
    assert_eq!(
        check(&t, Path::new(BINARY), "hostkeeper"),
        Err(Error::Port(PortError::new("codesign", "not scripted")))
    );
}

const VERIFY: [&str; 4] = ["codesign", "--verify", "--strict", "--verbose=2"];

#[test]
fn the_adapter_runs_the_three_codesign_commands_and_reads_both_streams() {
    // Catches: another codesign flag, the binary's path lost, or the stream codesign prints
    // `--display` text to (stderr) not read.
    let exec = FakeExec::default()
        .on(
            &[VERIFY[0], VERIFY[1], VERIFY[2], VERIFY[3], "p/hk"],
            true,
            "",
            "p/hk: valid on disk\n",
        )
        .on(
            &["codesign", "--display", "--verbose=2", "p/hk"],
            true,
            "",
            "Identifier=dev.hostkeeper.hk\n",
        )
        .on(
            &["codesign", "--display", "--requirements", "-", "p/hk"],
            true,
            "designated => x\n",
            "Executable=p/hk\n",
        );
    let cli = CodesignCli::new(exec);
    let path = Path::new("p/hk");
    assert_eq!(cli.verify(path), ran(true, "\np/hk: valid on disk\n"));
    assert_eq!(
        cli.display(path),
        ran(true, "\nIdentifier=dev.hostkeeper.hk\n")
    );
    assert_eq!(
        cli.requirements(path),
        ran(true, "designated => x\n\nExecutable=p/hk\n")
    );
}

#[test]
fn the_adapter_keeps_a_nonzero_exit_as_an_answer_and_a_spawn_error_as_an_error() {
    // Catches: a codesign that exited 1 passed, its text dropped, or one that could not start
    // taken as an answer.
    let failing = FakeExec::default().on(
        &["codesign", "--display", "--verbose=2", "p/hk"],
        false,
        "",
        "p/hk: code object is not signed at all\n",
    );
    assert_eq!(
        CodesignCli::new(failing).display(Path::new("p/hk")),
        ran(false, "\np/hk: code object is not signed at all\n")
    );
    let absent = CodesignCli::new(FakeExec::default());
    assert_eq!(
        absent.display(Path::new("p/hk")),
        Err(PortError::new(
            "codesign --display --verbose=2 p/hk",
            "not scripted: codesign --display --verbose=2 p/hk"
        ))
    );
}

#[test]
fn the_adapter_refuses_a_path_that_is_not_utf8() {
    // Catches: a lossy path handed to codesign, which would check another file.
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(std::ffi::OsStr::from_bytes(b"p/\xff"));
    let cli = CodesignCli::new(FakeExec::default());
    assert_eq!(
        cli.verify(path),
        Err(PortError::new(
            format!("codesign --verify --strict --verbose=2 {}", path.display()),
            "the path is not UTF-8"
        ))
    );
}

#[test]
fn check_code_signature_reads_the_binary_and_the_program_from_its_command_line() {
    // Catches: the binary path or `--program` not reaching the check, or the check run without
    // the port that runs codesign.
    let fakes = empty();
    let v = verb(&["check-code-signature", "dist/hk", "--program", "hk"]);
    assert_eq!(
        crate::release::run_with(v, &fakes.ports()),
        Err(Error::Port(PortError::new("codesign", "not scripted")))
    );
    assert_eq!(fakes.codesign.calls.borrow().as_slice(), ["verify dist/hk"]);
    let unknown = verb(&["check-code-signature", "dist/gh", "--program", "gh"]);
    assert!(matches!(
        crate::release::run_with(unknown, &fakes.ports()),
        Err(Error::Failed(_))
    ));
    assert_eq!(fakes.codesign.calls.borrow().len(), 1);
}
