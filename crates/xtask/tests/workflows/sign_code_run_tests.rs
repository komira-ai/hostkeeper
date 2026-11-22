//! The steps of `sign-code` that decide whether the job's keychain can sign (the import of the
//! intermediate, [`IMPORT_INTERMEDIATE`] to [`SAME_AS_ALREADY_EXISTS`], then
//! [`FIND_VALID_IDENTITIES`] and [`IDENTITY_IS_VALID`], which the lint holds the committed file
//! to), run with bash as a runner runs a step (`bash --noprofile --norc -eo pipefail`) against a
//! fake
//! `security` (tests/bin/gh_fixture.rs) that gives each answer the real one can: the import
//! passes on a runner without the intermediate and on one that holds it, and on nothing else;
//! the identity passes only when `find-identity -v` lists it, by its exact name, as valid.

use std::path::Path;
use std::process::Command;

use super::code_signing::{
    ALREADY_EXISTS, CODE_IDENTITY, FIND_VALID_IDENTITIES, IDENTITY_IS_VALID, IMPORT_FAILED_IF,
    IMPORT_ID, IMPORT_INTERMEDIATE, SAME_AS_ALREADY_EXISTS, SHOW_IMPORT, WRITE_ALREADY_EXISTS,
};
use super::testdir::TestDir;

/// The fixture that answers as `security` when started by that name.
const FIXTURE: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");

/// What a step did: its exit status, its stdout, and the command lines `security` was given.
#[derive(Debug, PartialEq)]
struct Ran {
    status: Option<i32>,
    stdout: String,
    calls: String,
}

/// What the fake `security` answers to the verb of the step.
struct Answer<'a> {
    stdout: &'a str,
    stderr: &'a str,
    status: u8,
}

/// Runs `script` with bash in a scratch directory whose `bin/security` is the fixture, which
/// answers `verb` with `answer`.
fn run(name: &str, script: &str, verb: &str, answer: &Answer) -> Ran {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("signing-steps");
    let root = TestDir::under(&base, name);
    let answers = root.join("bin/security-answers");
    for dir in [answers.as_path(), &root.join("temp")] {
        std::fs::create_dir_all(dir).expect("make a scratch directory");
    }
    std::fs::hard_link(FIXTURE, root.join("bin/security")).expect("install the fake security");
    for (part, text) in [
        ("stdout", answer.stdout.to_owned()),
        ("stderr", answer.stderr.to_owned()),
        ("status", answer.status.to_string()),
    ] {
        std::fs::write(answers.join(format!("{verb}.{part}")), text).expect("an answer");
    }
    let out = Command::new("bash")
        .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", script])
        .current_dir(&root)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("RUNNER_TEMP", "temp")
        .env("IDENTITY", CODE_IDENTITY)
        .output()
        .expect("run bash");
    Ran {
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        calls: std::fs::read_to_string(answers.join("calls")).unwrap_or_default(),
    }
}

/// The command line the import step gives `security`.
const IMPORT_CALL: &str = "import temp/DeveloperIDG2CA.cer -k temp/code-signing.keychain-db\n";

/// The four import steps as a runner runs them: the import with `continue-on-error` (its
/// failure only sets its outcome), the print of its answer, then, only when the import failed
/// ([`IMPORT_FAILED_IF`]), the duplicate answer written and compared with the import's. A step
/// that fails stops the job.
fn import(name: &str, answer: &Answer) -> Ran {
    assert_eq!(
        IMPORT_FAILED_IF,
        format!("${{{{ steps.{IMPORT_ID}.outcome == 'failure' }}}}")
    );
    let script = format!(
        "if {IMPORT_INTERMEDIATE}; then outcome=success; else outcome=failure; fi\n{SHOW_IMPORT}\nif test \"$outcome\" = failure; then\n{WRITE_ALREADY_EXISTS}\n{SAME_AS_ALREADY_EXISTS}\nfi"
    );
    run(name, &script, "import", answer)
}

#[test]
fn the_intermediate_imported_into_the_keychain_passes() {
    // Catches: the step failing on a runner that does not hold the intermediate, or importing
    // it anywhere but the job's keychain.
    let ran = import(
        "imported",
        &Answer {
            stdout: "",
            stderr: "1 certificate imported.\n",
            status: 0,
        },
    );
    assert_eq!(
        ran,
        Ran {
            status: Some(0),
            stdout: "1 certificate imported.\n".to_owned(),
            calls: IMPORT_CALL.to_owned(),
        }
    );
}

#[test]
fn a_runner_that_holds_the_intermediate_already_passes() {
    // Catches: the duplicate answer of a hosted runner that holds Apple's intermediate failing
    // the release, as it failed the first signing run on main.
    let ran = import(
        "already-exists",
        &Answer {
            stdout: "",
            stderr: &format!("{ALREADY_EXISTS}\n"),
            status: 1,
        },
    );
    assert_eq!(
        ran,
        Ran {
            status: Some(0),
            stdout: format!("{ALREADY_EXISTS}\n"),
            calls: IMPORT_CALL.to_owned(),
        }
    );
}

#[test]
fn any_other_failure_of_the_import_stops_the_job_after_printing_what_security_said() {
    // Catches: a judge that passes every failure, or one that takes any answer naming the
    // duplicate, with more said beside it, or without its line break, for the duplicate alone.
    // The answer is printed before the job stops.
    let other = "security: SecKeychainItemImport: One or more parameters passed to a function were not valid.";
    let more = format!(
        "{ALREADY_EXISTS}\nsecurity: SecKeychainItemImport: Unable to decode the provided data."
    );
    let suffixed = format!("{ALREADY_EXISTS} (-25299)\n");
    for (name, stderr) in [
        ("other", format!("{other}\n")),
        ("more", format!("{more}\n")),
        ("suffixed", suffixed),
        ("unterminated", ALREADY_EXISTS.to_owned()),
        ("empty", String::new()),
    ] {
        let ran = import(
            name,
            &Answer {
                stdout: "",
                stderr: &stderr,
                status: 1,
            },
        );
        assert_eq!(ran.status, Some(1), "{name}");
        assert_eq!(ran.calls, IMPORT_CALL, "{name}");
        assert!(ran.stdout.starts_with(&stderr), "{name}: {:?}", ran.stdout);
    }
}

/// The command line the identity steps give `security`.
const FIND_CALL: &str = "find-identity -v -p codesigning temp/code-signing.keychain-db\n";

/// Runs the two identity steps in order, as a runner does: the second only after the first
/// passed.
fn find(name: &str, stdout: &str, status: u8) -> Ran {
    run(
        name,
        &format!("{FIND_VALID_IDENTITIES}\n{IDENTITY_IS_VALID}"),
        "find-identity",
        &Answer {
            stdout,
            stderr: "",
            status,
        },
    )
}

#[test]
fn the_identity_listed_as_valid_passes_and_prints_nothing() {
    // Catches: the steps refusing the identity security lists as valid, or asking for another
    // policy or keychain, or printing the certificate's hash.
    let listed = format!(
        "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"{CODE_IDENTITY}\"\n     1 valid identities found\n"
    );
    let ran = find("valid", &listed, 0);
    assert_eq!(
        ran,
        Ran {
            status: Some(0),
            stdout: String::new(),
            calls: FIND_CALL.to_owned(),
        }
    );
}

#[test]
fn an_identity_not_listed_as_valid_by_its_exact_name_stops_the_job() {
    // Catches: a chain that does not build (no valid identity), another team's or the
    // installer's identity, or a longer name that starts with the identity's, passing; and a
    // `security` that fails although it printed the identity. Each fails the job quietly: the
    // failing step's name says why, and no certificate hash reaches the log.
    let installer = "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Developer ID Installer: Komira AI, Inc. (U7RTX4XD3X)\"\n     1 valid identities found\n".to_owned();
    let longer = format!(
        "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"{CODE_IDENTITY} 2\"\n     1 valid identities found\n"
    );
    let listed = format!(
        "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"{CODE_IDENTITY}\"\n     1 valid identities found\n"
    );
    for (name, stdout, status) in [
        ("none", "     0 valid identities found\n".to_owned(), 0),
        ("installer", installer, 0),
        ("longer", longer, 0),
        ("failed", listed, 1),
    ] {
        let ran = find(name, &stdout, status);
        assert_eq!(
            ran,
            Ran {
                status: Some(1),
                stdout: String::new(),
                calls: FIND_CALL.to_owned(),
            },
            "{name}"
        );
    }
}

#[test]
fn the_fake_security_answers_a_verb_it_was_not_given_with_status_2() {
    // Catches: a fake that answers a verb the test did not set up as a success, so a step
    // asking security something else would pass these tests.
    let ran = run(
        "unknown-verb",
        "security delete-keychain x",
        "import",
        &Answer {
            stdout: "",
            stderr: "",
            status: 0,
        },
    );
    assert_eq!(
        ran,
        Ran {
            status: Some(2),
            stdout: String::new(),
            calls: "delete-keychain x\n".to_owned(),
        }
    );
}
