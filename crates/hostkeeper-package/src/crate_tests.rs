//! Checks over the crate's own sources and README: the documented reason tokens, no signer
//! written into the code, and one way only to a `VerifiedArchive`. Each check is also run on a
//! text that breaks it.

use std::path::Path;

use crate::REASONS;
use crate::testing::tlv;

/// The reason tokens that have shipped. A token is never removed or renamed (formats change
/// additively only), so this list only grows, and each entry must stay in `REASONS`.
const SHIPPED: [&str; 5] = [
    "digest_mismatch",
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
/// `*_tests.rs` files.
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
        .filter(|name| name.ends_with(".rs") & !name.ends_with("_tests.rs"))
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

    let extra = "# x\n\n## Failures\n\n| token |\n|---|\n| `digest_mismatch` |\n| `new_token` |\n\n## Next\n\n| `fetch` |\n";
    assert_eq!(documented_tokens(extra), ["digest_mismatch", "new_token"]);
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

/// The public functions of `text` whose signature names `VerifiedArchive`, by their head
/// (`pub fn name`).
fn public_constructors(text: &str) -> Vec<String> {
    text.match_indices("pub fn ")
        .filter_map(|(at, _)| {
            let rest = text.get(at..)?;
            let signature = rest.get(..rest.find(['{', ';'])?)?;
            let head = signature.split('(').next().unwrap_or_default().trim();
            signature
                .contains("VerifiedArchive")
                .then(|| head.to_owned())
        })
        .collect()
}

/// Catches a second way to a `VerifiedArchive`: the only public function of the crate whose
/// signature names it is `prove`, its fields are private, and it derives neither `Clone` nor
/// `Default`.
#[test]
fn verified_archive_has_no_public_constructor() {
    let mut found = Vec::new();
    for (name, text) in sources() {
        for head in public_constructors(&text) {
            found.push(format!("{name}: {head}"));
        }
    }
    assert_eq!(found, ["prove.rs: pub fn prove"]);

    let prove = read("src/prove.rs");
    let start = prove
        .find("#[derive(Debug)]\npub struct VerifiedArchive {")
        .expect("the struct, deriving Debug only");
    let body = prove.get(start..).expect("the struct");
    let body = body
        .get(..body.find("\n}").expect("its end"))
        .expect("its fields");
    assert_eq!(body.matches("pub ").count(), 1, "a public field: {body}");

    let forged = "impl VerifiedArchive {\n    pub fn forge(path: PathBuf) -> VerifiedArchive {\n";
    assert_eq!(public_constructors(forged), ["pub fn forge"]);
    assert!(public_constructors("pub fn other() -> u8;").is_empty());
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
        assert_eq!(clock.boot_id(), "boot-2");
    }
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
