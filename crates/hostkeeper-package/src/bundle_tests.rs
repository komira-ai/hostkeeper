use super::*;
use crate::testing::{BundleSpec, bundle_json, certificate, statement};
use serde_json::json;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";
const DIGEST: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";

const NO_PAYLOAD: &str = "the bundle has no DSSE payload";
const BAD_PAYLOAD: &str = "the DSSE payload is not a JSON statement in base64";
const BAD_COMMIT: &str = "the provenance does not name exactly one commit of 40 or 64 hex digits";
const NO_CERTIFICATE: &str = "the bundle has no certificate in base64";
const NO_REF: &str = "the certificate names no source ref";

fn spec() -> BundleSpec<'static> {
    BundleSpec {
        organisation: "example-org",
        repository: "tool",
        workflow: "release.yml",
        source_ref: "refs/tags/v1.2.3",
        certificate_commit: COMMIT,
        provenance_commit: COMMIT,
        runner: "github-hosted",
        subject_name: "tool.tar",
        subject_sha256: DIGEST,
    }
}

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn certificate_b64() -> String {
    b64(&certificate(&spec()))
}

/// A statement naming `commits` as the git commits of its resolved dependencies.
fn statement_of(commits: &[&str]) -> Value {
    let dependencies: Vec<Value> = commits
        .iter()
        .map(|commit| json!({ "digest": { "gitCommit": commit } }))
        .collect();
    json!({
        "subject": [{ "name": "tool.tar", "digest": { "sha256": DIGEST } }],
        "predicate": { "buildDefinition": { "resolvedDependencies": dependencies } }
    })
}

/// A v0.3 bundle holding `statement` and the certificate `certificate`.
fn bundle_of(statement: &Value, certificate: &str) -> Vec<u8> {
    let payload = b64(statement.to_string().as_bytes());
    json!({
        "verificationMaterial": { "certificate": { "rawBytes": certificate } },
        "dsseEnvelope": { "payload": payload }
    })
    .to_string()
    .into_bytes()
}

/// A bundle of the synthetic spec yields its commit, ref and subject digest.
/// Catches a reader that takes the commit from the certificate or loses the subjects.
#[test]
fn a_bundle_yields_its_commit_ref_and_subjects() {
    let facts = read(bundle_json(&spec()).as_bytes()).expect("a bundle");
    assert_eq!(
        facts,
        Facts {
            commit: CommitId::new(COMMIT).expect("a commit"),
            source_ref: SourceRef::new("refs/tags/v1.2.3").expect("a ref"),
            subjects: vec![DIGEST.to_owned()],
        }
    );
}

/// Subject digests are lower-cased, and `lists` matches the lower-case digest only.
/// Catches a comparison that is case sensitive on the bundle's side or matches any digest.
#[test]
fn subject_digests_are_lowered_and_listed() {
    let upper = DIGEST.to_ascii_uppercase();
    let spec = BundleSpec {
        subject_sha256: &upper,
        ..spec()
    };
    let facts = read(bundle_json(&spec).as_bytes()).expect("a bundle");
    assert_eq!(facts.subjects, vec![DIGEST.to_owned()]);
    assert!(facts.lists(DIGEST));
    assert!(!facts.lists(&"0".repeat(64)));
    assert!(!facts.lists(&upper));
}

/// The certificate is read from the chain of a v0.1 or v0.2 bundle, and the chain is the
/// fallback when the v0.3 place does not hold text.
/// Catches a reader that knows only the v0.3 location.
#[test]
fn the_certificate_is_read_from_the_older_chain_location() {
    let payload = b64(statement(&spec()).as_bytes());
    let chain = json!({
        "verificationMaterial": {
            "x509CertificateChain": { "certificates": [{ "rawBytes": certificate_b64() }] }
        },
        "dsseEnvelope": { "payload": payload }
    });
    let facts = read(chain.to_string().as_bytes()).expect("a chain bundle");
    assert_eq!(facts.source_ref.as_str(), "refs/tags/v1.2.3");

    let both = json!({
        "verificationMaterial": {
            "certificate": { "rawBytes": 5 },
            "x509CertificateChain": { "certificates": [{ "rawBytes": certificate_b64() }] }
        },
        "dsseEnvelope": { "payload": payload }
    });
    let facts = read(both.to_string().as_bytes()).expect("a fallback bundle");
    assert_eq!(facts.commit.as_str(), COMMIT);
}

/// Text that is not JSON is refused.
/// Catches a reader that panics or reports a later, misleading reason.
#[test]
fn text_that_is_not_json_is_refused() {
    assert_eq!(read(b"not json"), Err("the bundle is not JSON"));
}

/// A bundle with no DSSE payload string is refused.
/// Catches a reader that treats a missing or non-text payload as an empty statement.
#[test]
fn a_bundle_without_a_payload_is_refused() {
    assert_eq!(read(b"{}"), Err(NO_PAYLOAD));
    assert_eq!(read(br#"{"dsseEnvelope":{"payload":5}}"#), Err(NO_PAYLOAD));
}

/// A payload that is not base64, or not JSON once decoded, is refused.
/// Catches a reader that accepts an undecodable statement.
#[test]
fn a_payload_that_is_not_a_base64_json_statement_is_refused() {
    let not_base64 = json!({ "dsseEnvelope": { "payload": "!!!" } }).to_string();
    assert_eq!(read(not_base64.as_bytes()), Err(BAD_PAYLOAD));
    let not_json = json!({ "dsseEnvelope": { "payload": b64(b"plain text") } }).to_string();
    assert_eq!(read(not_json.as_bytes()), Err(BAD_PAYLOAD));
}

/// A statement with no resolved dependencies, none with a commit, or two different commits has
/// no one commit.
/// Catches a reader that picks the first of several commits or invents one.
#[test]
fn a_statement_without_exactly_one_commit_is_refused() {
    let certificate = certificate_b64();
    let none = json!({ "subject": [] });
    assert_eq!(read(&bundle_of(&none, &certificate)), Err(BAD_COMMIT));

    let empty = statement_of(&[]);
    assert_eq!(read(&bundle_of(&empty, &certificate)), Err(BAD_COMMIT));

    let mut odd = statement_of(&[]);
    odd["predicate"]["buildDefinition"]["resolvedDependencies"] =
        json!([{ "uri": "x" }, { "digest": { "gitCommit": 5 } }]);
    assert_eq!(read(&bundle_of(&odd, &certificate)), Err(BAD_COMMIT));

    let two = statement_of(&[COMMIT, OTHER]);
    assert_eq!(read(&bundle_of(&two, &certificate)), Err(BAD_COMMIT));
}

/// The same commit named twice is one commit, and dependencies without a commit are ignored.
/// Catches a reader that counts entries rather than distinct commits.
#[test]
fn the_same_commit_twice_is_accepted() {
    let mut twice = statement_of(&[COMMIT, COMMIT]);
    twice["predicate"]["buildDefinition"]["resolvedDependencies"]
        .as_array_mut()
        .expect("a list")
        .push(json!({ "uri": "no digest" }));
    let facts = read(&bundle_of(&twice, &certificate_b64())).expect("one distinct commit");
    assert_eq!(facts.commit.as_str(), COMMIT);
}

/// A commit that is not 40 or 64 lower-case hex digits is refused; a 64-digit one is accepted.
/// Catches a reader that passes a malformed commit on to the verifier.
#[test]
fn a_commit_that_is_not_lower_case_hex_is_refused() {
    let certificate = certificate_b64();
    for bad in [&COMMIT.to_ascii_uppercase()[..], "abc", &COMMIT[1..], "zz"] {
        let statement = statement_of(&[bad]);
        assert_eq!(read(&bundle_of(&statement, &certificate)), Err(BAD_COMMIT));
    }
    let long = "a".repeat(64);
    let statement = statement_of(&[long.as_str()]);
    let facts = read(&bundle_of(&statement, &certificate)).expect("a 64-digit commit");
    assert_eq!(facts.commit.as_str(), long);
}

/// A bundle with no certificate, or one that is not base64, is refused.
/// Catches a reader that goes on without a certificate.
#[test]
fn a_missing_or_undecodable_certificate_is_refused() {
    let statement = statement_of(&[COMMIT]);
    let payload = b64(statement.to_string().as_bytes());
    let none = json!({ "dsseEnvelope": { "payload": payload } }).to_string();
    assert_eq!(read(none.as_bytes()), Err(NO_CERTIFICATE));
    assert_eq!(read(&bundle_of(&statement, "!!!")), Err(NO_CERTIFICATE));
}

/// A certificate with no source ref extension, or with a ref that is not a valid ref, is
/// refused.
/// Catches a reader that passes an unchecked ref on to the verifier as an argument.
#[test]
fn a_certificate_without_a_valid_ref_is_refused() {
    let statement = statement_of(&[COMMIT]);
    assert_eq!(read(&bundle_of(&statement, &b64(b"junk"))), Err(NO_REF));
    for bad in ["refs/heads/../x", "main"] {
        let spec = BundleSpec {
            source_ref: bad,
            ..spec()
        };
        let certificate = b64(&certificate(&spec));
        assert_eq!(read(&bundle_of(&statement, &certificate)), Err(NO_REF));
    }
}

/// A statement with no subject list gives no subjects and is still read; subjects without a
/// text SHA-256 are left out.
/// Catches a reader that refuses a statement for lacking subjects or lists a non-digest.
#[test]
fn a_statement_without_subjects_still_reads() {
    let mut statement = statement_of(&[COMMIT]);
    statement["subject"] = json!([{ "name": "a" }, { "digest": { "sha256": 7 } }]);
    let facts = read(&bundle_of(&statement, &certificate_b64())).expect("a bundle");
    assert_eq!(facts.subjects, Vec::<String>::new());

    statement
        .as_object_mut()
        .expect("an object")
        .remove("subject");
    let facts = read(&bundle_of(&statement, &certificate_b64())).expect("a bundle");
    assert_eq!(facts.subjects, Vec::<String>::new());
    assert!(!facts.lists(DIGEST));
}

/// Commits of 40 and 64 lower-case hex digits are accepted; anything else is refused.
/// Catches a length or alphabet check that is off by one.
#[test]
fn commit_ids_are_40_or_64_lower_case_hex_digits() {
    for good in [COMMIT.to_owned(), "9".repeat(64)] {
        assert_eq!(CommitId::new(&good).expect("a commit").as_str(), good);
    }
    for bad in [
        "".to_owned(),
        COMMIT[1..].to_owned(),
        format!("{COMMIT}0"),
        COMMIT.to_ascii_uppercase(),
        "g".repeat(40),
        "a".repeat(63),
        "a".repeat(65),
    ] {
        assert_eq!(CommitId::new(&bad), None, "{bad}");
    }
}

/// Plain refs under `refs/` are accepted, with the length limit at 256 bytes.
/// Catches a ref grammar that refuses a real tag or branch, or a limit that is off by one.
#[test]
fn plain_refs_are_accepted() {
    let longest = format!("refs/{}", "a".repeat(251));
    for good in [
        "refs/tags/v1.2.3",
        "refs/heads/release/1.2",
        "refs/tags/v1+build",
        "refs/heads/a_b-c",
        longest.as_str(),
    ] {
        assert_eq!(SourceRef::new(good).expect("a ref").as_str(), good);
    }
}

/// Text that could be a path trick, an option, or not a ref is refused.
/// Catches a ref passed to the verifier as an argument that could mean something else.
#[test]
fn refs_that_could_mean_something_else_are_refused() {
    let too_long = format!("refs/{}", "a".repeat(252));
    for bad in [
        "",
        "refs/",
        "refs//a",
        "refs/a/",
        "refs/./a",
        "refs/../a",
        "heads/main",
        "-refs/x",
        "refs/a b",
        "refs/a\nb",
        "refs",
        too_long.as_str(),
    ] {
        assert_eq!(SourceRef::new(bad), None, "{bad:?}");
    }
}
