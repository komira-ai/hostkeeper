use base64::Engine as _;
use serde_json::json;

use super::*;
use crate::release::fakes::scratch;

/// An unsigned bundle whose statement lists `digests` as its subjects' SHA-256.
fn bundle_listing(digests: &[&str]) -> Vec<u8> {
    let subjects: Vec<_> = digests
        .iter()
        .map(|d| json!({"name": "package", "digest": {"sha256": d}}))
        .collect();
    let statement = json!({"_type": "https://in-toto.io/Statement/v1", "subject": subjects});
    json!({"dsseEnvelope": {"payload": STANDARD.encode(statement.to_string())}})
        .to_string()
        .into_bytes()
}

#[test]
fn a_bundle_is_taken_at_the_daemons_cap_and_refused_over_it() {
    // Catches: a bundle of exactly the cap refused, or one a byte over it read at all.
    let digest = "a".repeat(64);
    let mut bundle = bundle_listing(&[&digest]);
    bundle.resize(usize::try_from(MAX_BUNDLE_BYTES).expect("fits"), b' ');
    assert_eq!(bundle_problem(&bundle, "p.deb", &digest), None);
    bundle.push(b' ');
    assert_eq!(
        bundle_problem(&bundle, "p.deb", &digest),
        Some("longer than 1048576 bytes, the most the daemon reads of a bundle".to_owned())
    );
}

#[test]
fn a_bundle_must_list_the_packages_digest_among_its_subjects() {
    // Catches: a bundle that attests other bytes taken for the package's, or the digest compared
    // in another case than the bundle spells it.
    let digest = "a".repeat(64);
    let other = "b".repeat(64);
    assert_eq!(
        bundle_problem(&bundle_listing(&[&other, &digest]), "p.deb", &digest),
        None
    );
    assert_eq!(
        bundle_problem(&bundle_listing(&[&digest.to_uppercase()]), "p.deb", &digest),
        None
    );
    assert_eq!(
        bundle_problem(&bundle_listing(&[&other]), "p.deb", &digest),
        Some(format!("does not list the SHA-256 {digest} of p.deb"))
    );
    assert_eq!(
        bundle_problem(&bundle_listing(&[]), "p.deb", &digest),
        Some(format!("does not list the SHA-256 {digest} of p.deb"))
    );
}

#[test]
fn a_file_that_is_not_a_bundle_is_named_for_what_it_lacks() {
    // Catches: a file with no DSSE payload, a payload that is not base64 JSON, or a statement
    // with no subject list taken as a bundle.
    let digest = "a".repeat(64);
    let cases = [
        (b"not json".to_vec(), "not JSON"),
        (b"{}".to_vec(), "no DSSE payload"),
        (
            br#"{"dsseEnvelope":{"payload":"not base64!"}}"#.to_vec(),
            "the DSSE payload is not a JSON statement in base64",
        ),
    ];
    for (bytes, why) in cases {
        assert_eq!(
            bundle_problem(&bytes, "p.deb", &digest),
            Some(format!("not an attestation bundle: {why}"))
        );
    }
    let statement = STANDARD.encode(json!({"_type": "x"}).to_string());
    let no_subjects = json!({"dsseEnvelope": {"payload": statement}}).to_string();
    assert_eq!(
        bundle_problem(no_subjects.as_bytes(), "p.deb", &digest),
        Some(format!("does not list the SHA-256 {digest} of p.deb"))
    );
}

#[test]
fn a_regular_file_passes_and_anything_else_is_named() {
    // Catches: a directory taken as a package, a missing file reported as anything but the
    // caller's reason, or a path that cannot be read taken as absent.
    let root = scratch("targets-files-regular");
    std::fs::write(root.join("file"), b"x").expect("write");
    std::fs::create_dir(root.join("dir")).expect("mkdir");
    let mut problems = Vec::new();
    assert_eq!(regular_file(&root, "file", "gone", &mut problems), Ok(true));
    assert_eq!(regular_file(&root, "dir", "gone", &mut problems), Ok(false));
    assert_eq!(
        regular_file(&root, "nothing", "gone", &mut problems),
        Ok(false)
    );
    assert_eq!(
        problems,
        [
            "dir: not a regular file".to_owned(),
            "nothing: gone".to_owned()
        ]
    );
    // Below a file, nothing can be looked up: not "missing", an error.
    let below = regular_file(&root, "file/below", "gone", &mut problems);
    assert!(matches!(below, Err(Error::Io { .. })), "{below:?}");
}

#[test]
fn a_file_is_read_up_to_one_byte_past_the_cap() {
    // Catches: the whole of an oversized file read into memory, or a file of exactly the cap
    // reported as over it.
    let root = scratch("targets-files-capped");
    std::fs::write(root.join("f"), b"0123456789").expect("write");
    assert_eq!(
        read_capped(&root.join("f"), 10).expect("read"),
        b"0123456789"
    );
    assert_eq!(read_capped(&root.join("f"), 4).expect("read"), b"01234");
    assert!(matches!(
        read_capped(&root.join("none"), 4),
        Err(Error::Io { .. })
    ));
}
