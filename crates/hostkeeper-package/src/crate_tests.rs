//! Checks over the crate's own sources and README: the documented reason tokens and no signer
//! written into the code. Each check is also run on a text that breaks it.

use std::path::Path;

use crate::REASONS;
use crate::testing::tlv;

/// The reason tokens that have shipped. A token is never removed or renamed (formats change
/// additively only), so this list only grows, and each entry must stay in `REASONS`.
const SHIPPED: [&str; 8] = [
    "index_invalid",
    "index_digest_mismatch",
    "no_package_for_platform",
    "package_digest_mismatch",
    "fetch_failed",
    "attestation_unavailable",
    "no_space",
    "not_attested",
];

/// The strings that would pin a signer in the code.
const SIGNER_LITERALS: [&str; 2] = ["komira-ai", ".github/workflows"];

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).expect("a file of the crate")
}

/// The crate's non-test sources, `(file name, text)`, sorted: every `src/*.rs` but the
/// `*_tests.rs` files and `testing.rs`, the test support compiled only with `cfg(test)`.
fn sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("the sources")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| {
            name.ends_with(".rs") & !name.ends_with("_tests.rs") & (name != "testing.rs")
        })
        .map(|name| {
            let text = read(&format!("src/{name}"));
            (name, text)
        })
        .collect();
    out.sort();
    out
}

/// The tokens of the `Failures` table of a README: the first cell of each row, in backticks.
fn documented_tokens(readme: &str) -> Vec<String> {
    let section = readme
        .split("\n## ")
        .find(|section| section.starts_with("Failures"))
        .unwrap_or_default();
    let mut tokens: Vec<String> = section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`'))
        .map(|(token, _)| token.to_owned())
        .collect();
    tokens.sort();
    tokens
}

/// Catches a reason token added without documenting it, documented without being reported, or
/// a shipped token removed or renamed: the README's `Failures` table and `REASONS` hold the same
/// set, every shipped token is in it, and each is a token the hook contract accepts.
#[test]
fn outcome_tokens_are_the_documented_set() {
    let mut reasons: Vec<String> = REASONS.iter().map(|r| (*r).to_owned()).collect();
    reasons.sort();
    reasons.dedup();
    assert_eq!(reasons.len(), REASONS.len(), "a token is listed twice");
    assert_eq!(documented_tokens(&read("README.md")), reasons);
    for shipped in SHIPPED {
        assert!(
            REASONS.contains(&shipped),
            "the shipped token {shipped} is gone"
        );
    }
    assert!(REASONS.iter().all(|r| hostkeeper_hook::is_reason_token(r)));

    let extra = "# x\n\n## Failures\n\n| token |\n|---|\n| `index_invalid` |\n| `new_token` |\n\n## Next\n\n| `fetch` |\n";
    assert_eq!(documented_tokens(extra), ["index_invalid", "new_token"]);
    assert!(documented_tokens("no such section").is_empty());
}

/// The signer strings `text` holds.
fn signer_literals(text: &str) -> Vec<&'static str> {
    SIGNER_LITERALS
        .into_iter()
        .filter(|literal| text.contains(literal))
        .collect()
}

/// Catches an organisation, repository or workflow written into the code: no non-test source
/// holds `komira-ai` or `.github/workflows`, so the signer is only ever the daemon config's.
#[test]
fn no_signer_constant_exists_in_the_crate() {
    let sources = sources();
    assert!(
        sources.iter().any(|(name, _)| name == "gh.rs"),
        "the scan saw no sources"
    );
    for (name, text) in &sources {
        assert!(
            signer_literals(text).is_empty(),
            "{name}: {:?}",
            signer_literals(text)
        );
    }
    let pinned =
        "pub const REPO: &str = \"komira-ai/kbf\";\nconst W: &str = \"x/.github/workflows/a.yml\";";
    assert_eq!(signer_literals(pinned), SIGNER_LITERALS);
}

/// Catches a refusal whose text loses its outcome, class or reason.
#[test]
fn a_refusal_reads_as_its_outcome_class_and_reason() {
    use crate::{FETCH_FAILED, NO_SPACE, PACKAGE_DIGEST_MISMATCH, PackageFailure};
    assert_eq!(
        PackageFailure::deferred(FETCH_FAILED).to_string(),
        "Deferred, class environment, reason fetch_failed"
    );
    assert_eq!(
        PackageFailure::release(PACKAGE_DIGEST_MISMATCH).to_string(),
        "Failure, class release, reason package_digest_mismatch"
    );
    assert_eq!(
        PackageFailure::environment(NO_SPACE).to_string(),
        "Failure, class environment, reason no_space"
    );
}

/// Catches the tests' DER writer spelling a length wrongly, which would make every synthetic
/// certificate unreadable: the short form below 128, one length byte below 256, two above.
#[test]
fn the_test_der_writer_uses_each_length_form() {
    assert_eq!(tlv(0x04, &[7; 5])[..2], [0x04, 5]);
    assert_eq!(tlv(0x04, &[7; 200])[..3], [0x04, 0x81, 200]);
    assert_eq!(tlv(0x04, &[7; 300])[..4], [0x04, 0x82, 1, 44]);
    assert_eq!(tlv(0x04, &[7; 300]).len(), 304);
}

/// Catches an error that loses what failed in its text: each network error and each verifier
/// answer reads as what happened, with the status code and the verifier's complaint.
#[test]
fn each_error_reads_as_what_happened() {
    use crate::ports::{NetError, VerifyError};
    let texts: Vec<String> = [
        NetError::Connect,
        NetError::Status(404),
        NetError::Truncated,
        NetError::DeadlineExceeded,
        NetError::Io,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        texts,
        [
            "could not connect, or the request was not answered",
            "the server answered with status 404",
            "the body ended before the length the server announced",
            "the transfer did not end before its deadline",
            "reading the body failed",
        ]
    );
    let answers: Vec<String> = [
        VerifyError::Unavailable,
        VerifyError::TimedOut,
        VerifyError::Rejected("no match".to_owned()),
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        answers,
        [
            "the verifier is unavailable",
            "the verifier ran out of time",
            "not attested by the trusted signer: no match",
        ]
    );
}

/// Catches a test clock that moves on its own or not at all: a sleep adds its seconds and is
/// counted, whether or not the clock also yields the processor; a reboot changes the boot id.
#[test]
fn the_test_clock_moves_only_when_told() {
    use hostkeeper_steps::Clock;
    for clock in [
        crate::testing::FakeClock::at(5),
        crate::testing::FakeClock::yielding(5),
    ] {
        clock.sleep_secs(2);
        assert_eq!((clock.mono_secs(), clock.sleeps()), (7, 1));
        clock.reboot("boot-2");
        assert_eq!(clock.boot_id().expect("a boot id").as_str(), "boot-2");
    }
}

/// Catches a deadline compared across an unknown boot: a deadline of this boot has its seconds
/// left; one of an unknown boot, one read on a clock whose boot id cannot be read, and one of
/// another boot have none.
#[test]
fn a_deadline_of_another_or_an_unknown_boot_has_no_time_left() {
    use crate::ports::remaining_secs;
    use hostkeeper_steps::Stamp;
    let clock = crate::testing::FakeClock::at(10);
    let deadline = clock.stamp(100);
    assert_eq!(remaining_secs(&deadline, &clock), 90);
    let unknown = Stamp {
        boot_id: None,
        mono_secs: 100,
    };
    assert_eq!(remaining_secs(&unknown, &clock), 0);
    clock.reboot("");
    assert_eq!(clock.stamp(100).boot_id, None);
    assert_eq!(remaining_secs(&deadline, &clock), 0);
    clock.reboot("boot-2");
    assert_eq!(remaining_secs(&deadline, &clock), 0);
}

/// Catches the tests' failing writer failing to fail: every write is its error, and a flush
/// has nothing to do.
#[test]
fn the_test_writer_fails_every_write() {
    use std::io::Write;
    let mut writer = crate::testing::Failing(std::io::ErrorKind::StorageFull);
    let error = writer.write(b"x").expect_err("a full disk");
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    writer.flush().expect("nothing to flush");
}
