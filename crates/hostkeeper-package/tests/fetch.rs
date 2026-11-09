//! Fetch and proof through an order's entry (docs/design/packages.md, section 7.4), through the
//! public API only: the host's stores are a real `stores.toml` in a scratch directory, the network
//! is a fake fetcher keyed by URL (or [`NetFetcher`] over an in-memory transport for the rows
//! that are the fetcher's own: redirects, the credential, the token challenge), and the verifier
//! is a fake that records what it was asked. The clock moves only when a test moves it.

#[path = "fetch/network.rs"]
mod network;
#[path = "fetch/support.rs"]
mod support;

use hostkeeper_hook::{Class, Outcome};
use hostkeeper_package::ports::VerifyError;
use hostkeeper_package::{PackageFailure, Platform};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use support::*;

/// A mirror answers with the right bytes: it is asked first, the location is never asked, and the
/// staged archive is the verified archive, with the entry's size and SHA-256, whose verifier
/// saw both files; after success the bundle's staging file is gone and the archive is removed with
/// its value.
///
/// Catches mirrors tried after the locations, a location asked although a mirror gave the bytes,
/// and staging files left behind or removed before the verifier ran.
#[test]
fn a_mirror_with_the_right_bytes_wins_and_the_locations_are_not_asked() {
    for which in Which::BOTH {
        let host = Host::new("mirror-wins", true);
        let verifier = FakeVerifier::accepting();
        let fetcher = with(
            &host,
            which,
            &[
                (which.mirror_url("m1"), Serve::Bytes(which.bytes())),
                (which.url(), Serve::Bytes(wrong_for(which))),
            ],
        );
        let proven = host.run(&fetcher, &verifier, &entry()).expect("proven");
        assert!(proven.other_bytes.is_empty(), "{which:?}");
        assert_eq!(
            which.asked(&fetcher, &[which.mirror_url("m1"), which.url()]),
            [which.mirror_url("m1")],
            "{which:?}"
        );
        assert_eq!(proven.archive.sha256().as_str(), sha256(ARCHIVE));
        assert_eq!(
            proven.archive.size(),
            u64::try_from(ARCHIVE.len()).expect("a length")
        );
        assert_eq!(proven.archive.platform(), Platform::LinuxX86_64);
        assert_eq!(
            fs::read(proven.archive.path()).expect("the archive"),
            ARCHIVE
        );
        assert!(verifier.files_existed.get());
        let asked = verifier.asked.borrow();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, sha256(ARCHIVE));
        assert_eq!(
            asked[0].2.extension().and_then(|e| e.to_str()),
            Some("json")
        );
        assert_eq!(
            host.staged().len(),
            1,
            "only the archive is left: {:?}",
            host.staged()
        );
        let kept = proven.archive.path().to_owned();
        drop(proven);
        assert!(!kept.exists());
        assert!(host.staged().is_empty());
    }
}

/// A mirror answers with other bytes and then a location with the right ones: the location wins
/// and the mirror is reported as having given other bytes, once.
///
/// Catches a mirror's wrong bytes failing the order when a location has the right ones, and a
/// mismatch that is not reported.
#[test]
fn a_mirror_with_other_bytes_is_reported_and_the_location_is_used() {
    for which in Which::BOTH {
        let host = Host::new("mirror-wrong", true);
        let verifier = FakeVerifier::accepting();
        let fetcher = with(
            &host,
            which,
            &[
                (which.mirror_url("m1"), Serve::Bytes(wrong_for(which))),
                (which.url(), Serve::Bytes(which.bytes())),
            ],
        );
        let proven = host.run(&fetcher, &verifier, &entry()).expect("proven");
        assert_eq!(proven.other_bytes, ["m1"], "{which:?}");
        assert_eq!(
            which.asked(
                &fetcher,
                &[which.mirror_url("m1"), which.mirror_url("m2"), which.url()]
            ),
            [which.mirror_url("m1"), which.mirror_url("m2"), which.url()],
            "{which:?}: the second mirror is asked too, being down"
        );
    }
}

/// Only mirrors answer and every one gives other bytes (the entry's locations name a store the
/// host does not define): `store_digest_mismatch`, deferred. A location that also gave other
/// bytes makes it the release's fault; a mirror down and a location wrong too.
///
/// Catches a mirror's disagreement blamed on the release, and a location's blamed on the host.
#[test]
fn only_mirrors_with_other_bytes_defer_and_a_wrong_location_fails_the_release() {
    for which in Which::BOTH {
        let host = Host::new("only-mirrors", true);
        let verifier = FakeVerifier::accepting();
        let undefined = entry_of(
            ARCHIVE,
            &[("nowhere", ARTIFACT_AT)],
            &bundle(),
            &[("nowhere", BUNDLE_AT)],
        );
        let fetcher = with(
            &host,
            which,
            &[
                (which.mirror_url("m1"), Serve::Bytes(wrong_for(which))),
                (which.mirror_url("m2"), Serve::Bytes(wrong_for(which))),
            ],
        );
        // The other blob needs a store: serve it from the mirrors too.
        let other = match which {
            Which::Artifact => Which::Bundle,
            Which::Bundle => Which::Artifact,
        };
        let fetcher = fetcher.route(&other.mirror_url("m1"), Serve::Bytes(other.bytes()));
        let refusal = host
            .run(&fetcher, &verifier, &undefined)
            .expect_err("refused");
        assert_eq!(refusal, deferred("store_digest_mismatch"), "{which:?}");
        assert!(host.staged().is_empty(), "{which:?}");
        assert!(verifier.asked.borrow().is_empty());

        let fetcher = with(
            &host,
            which,
            &[
                (which.mirror_url("m1"), Serve::Bytes(wrong_for(which))),
                (which.mirror_url("m2"), Serve::Status(503)),
                (which.url(), Serve::Bytes(wrong_for(which))),
            ],
        );
        let refusal = host
            .run(&fetcher, &verifier, &entry())
            .expect_err("refused");
        assert_eq!(refusal, which.wrong(), "{which:?}");
        assert!(host.staged().is_empty(), "{which:?}");
    }
}

/// A mirror that hangs past the deadline is given up on and a location, with its own deadline,
/// gives the bytes; the hung mirror is not "other bytes".
///
/// Catches one deadline shared by all sources (the location would have none left) and a hang
/// counted as a mismatch.
#[test]
fn a_hung_mirror_does_not_use_up_the_next_sources_deadline() {
    for which in Which::BOTH {
        let host = Host::new("mirror-hangs", true);
        let verifier = FakeVerifier::accepting();
        let fetcher = with(
            &host,
            which,
            &[
                (which.mirror_url("m1"), Serve::Hang),
                (which.url(), Serve::Bytes(which.bytes())),
            ],
        );
        let proven = host.run(&fetcher, &verifier, &entry()).expect("proven");
        assert!(proven.other_bytes.is_empty(), "{which:?}");
    }
}

/// Every source is down (a status, nothing there, a hang): the artifact is `fetch_failed` and the
/// bundle `attestation_unavailable`, both deferred, and nothing is left staged.
///
/// Catches a down source treated as bad bytes, and the wrong token for the bundle.
#[test]
fn every_source_down_defers_with_the_blobs_own_reason() {
    for which in Which::BOTH {
        let host = Host::new("all-down", true);
        let verifier = FakeVerifier::accepting();
        let fetcher = with(
            &host,
            which,
            &[
                (which.mirror_url("m1"), Serve::Status(500)),
                (which.mirror_url("m2"), Serve::Hang),
                (which.url(), Serve::Status(404)),
            ],
        );
        let refusal = host
            .run(&fetcher, &verifier, &entry())
            .expect_err("refused");
        assert_eq!(refusal, which.down(), "{which:?}");
        assert!(host.staged().is_empty(), "{which:?}");
        assert!(verifier.asked.borrow().is_empty());
    }
}

/// With no mirror, every location of the entry giving other bytes is the release's fault.
///
/// Catches wrong bytes at a location treated as a fault of the host.
#[test]
fn every_location_with_other_bytes_fails_the_release() {
    for which in Which::BOTH {
        let host = Host::new("all-wrong", false);
        let verifier = FakeVerifier::accepting();
        let both = entry_of(
            ARCHIVE,
            &[("a", ARTIFACT_AT), ("b", ARTIFACT_AT)],
            &bundle(),
            &[("a", BUNDLE_AT), ("b", BUNDLE_AT)],
        );
        let at_b = format!("https://b.example/{}", location_of(which));
        let fetcher = with(
            &host,
            which,
            &[
                (which.url(), Serve::Bytes(wrong_for(which))),
                (at_b.clone(), Serve::Bytes(wrong_for(which))),
            ],
        );
        let refusal = host.run(&fetcher, &verifier, &both).expect_err("refused");
        assert_eq!(refusal, which.wrong(), "{which:?}");
        assert!(fetcher.urls().ends_with(&[which.url(), at_b]), "{which:?}");
        assert!(host.staged().is_empty(), "{which:?}");
    }
}

fn location_of(which: Which) -> &'static str {
    match which {
        Which::Artifact => ARTIFACT_AT,
        Which::Bundle => BUNDLE_AT,
    }
}

/// A host that defines no mirror and none of the entry's stores cannot ask anyone: `no_store`, a
/// failure of the environment, and not one request is made. A location of an undefined store is
/// skipped and the next is used; so is a location whose locator breaks the grammar.
///
/// Catches a missing store treated as a down source, and a skipped location that stops the walk.
#[test]
fn no_store_fails_the_environment_and_an_undefined_location_is_skipped() {
    for which in Which::BOTH {
        let host = Host::new("no-store", false);
        let verifier = FakeVerifier::accepting();
        let nobody = match which {
            Which::Artifact => entry_of(
                ARCHIVE,
                &[("nowhere", ARTIFACT_AT)],
                &bundle(),
                &[("a", BUNDLE_AT)],
            ),
            Which::Bundle => entry_of(
                ARCHIVE,
                &[("a", ARTIFACT_AT)],
                &bundle(),
                &[("nowhere", BUNDLE_AT)],
            ),
        };
        let fetcher = published(&host);
        let refusal = host.run(&fetcher, &verifier, &nobody).expect_err("refused");
        assert_eq!(
            refusal,
            failure(Outcome::Failure, Class::Environment, "no_store"),
            "{which:?}"
        );
        assert!(!fetcher.urls().contains(&which.url()), "{which:?}");

        let bad_locator = match which {
            Which::Artifact => entry_of(
                ARCHIVE,
                &[("a", "../etc/passwd")],
                &bundle(),
                &[("a", BUNDLE_AT)],
            ),
            Which::Bundle => entry_of(
                ARCHIVE,
                &[("a", ARTIFACT_AT)],
                &bundle(),
                &[("a", "x/../y")],
            ),
        };
        let refusal = host
            .run(&published(&host), &verifier, &bad_locator)
            .expect_err("refused");
        assert_eq!(
            refusal,
            failure(Outcome::Failure, Class::Environment, "no_store"),
            "{which:?}"
        );

        let skipping = match which {
            Which::Artifact => entry_of(
                ARCHIVE,
                &[("nowhere", ARTIFACT_AT), ("a", ARTIFACT_AT)],
                &bundle(),
                &[("a", BUNDLE_AT)],
            ),
            Which::Bundle => entry_of(
                ARCHIVE,
                &[("a", ARTIFACT_AT)],
                &bundle(),
                &[("nowhere", BUNDLE_AT), ("a", BUNDLE_AT)],
            ),
        };
        let fetcher = published(&host);
        host.run(&fetcher, &verifier, &skipping).expect("proven");
        assert!(
            !fetcher.urls().iter().any(|u| u.contains("nowhere")),
            "{which:?}"
        );
    }
}

/// A source that sends one byte more than the entry's size (announced, or not announced) is
/// stopped and counts as giving other bytes; one that sends one byte fewer does too.
///
/// Catches a size bound that is off by one, and bytes accepted past the size.
#[test]
fn one_byte_over_or_under_is_other_bytes() {
    for which in Which::BOTH {
        let mut over = which.bytes();
        over.push(b'!');
        let mut under = which.bytes();
        under.pop();
        for (case, serve) in [
            ("announced over", Serve::Bytes(over.clone())),
            ("unannounced over", Serve::Unannounced(over)),
            ("under", Serve::Bytes(under)),
        ] {
            let host = Host::new("off-by-one", false);
            let verifier = FakeVerifier::accepting();
            let fetcher = with(&host, which, &[(which.url(), serve)]);
            let refusal = host.run(&fetcher, &verifier, &entry()).expect_err(case);
            assert_eq!(refusal, which.wrong(), "{which:?} {case}");
            assert!(host.staged().is_empty(), "{which:?} {case}");
        }
    }
}

/// An entry that asks for more than the host allows, or that no fetch can be made for, is
/// refused before any request: bytes over `max_package_bytes` or 0, a digest that is not one, no
/// attestation, a bundle over 1 MiB or of 0 bytes, a bundle digest that is not one.
///
/// Catches the bound checked only after the transfer, and a malformed entry that reaches the
/// network.
#[test]
fn an_entry_that_cannot_be_fetched_is_refused_before_any_request() {
    let host = Host::new("refused-first", true);
    let verifier = FakeVerifier::accepting();
    let fetcher = published(&host);
    let mut cases: Vec<(&str, Target, PackageFailure)> = Vec::new();
    let mut over = entry();
    over.bytes = 65;
    cases.push((
        "bytes over max_package_bytes",
        over,
        release("package_digest_mismatch"),
    ));
    let mut none = entry();
    none.bytes = 0;
    cases.push(("no bytes", none, release("package_digest_mismatch")));
    let mut digest = entry();
    digest.digest = "xyz".to_owned();
    cases.push((
        "a digest that is not one",
        digest,
        release("package_digest_mismatch"),
    ));
    let mut absent = entry();
    absent.attestation = None;
    cases.push(("no attestation", absent, release("not_attested")));
    let mut big = entry();
    big.attestation.as_mut().expect("a bundle").bytes = (1 << 20) + 1;
    cases.push(("a bundle over 1 MiB", big, release("not_attested")));
    let mut empty = entry();
    empty.attestation.as_mut().expect("a bundle").bytes = 0;
    cases.push(("a bundle of no bytes", empty, release("not_attested")));
    let mut odd = entry();
    odd.attestation.as_mut().expect("a bundle").digest = "nope".to_owned();
    cases.push((
        "a bundle digest that is not one",
        odd,
        release("not_attested"),
    ));
    for (case, target, expected) in cases {
        let refusal = host.run(&fetcher, &verifier, &target).expect_err(case);
        assert_eq!(refusal, expected, "{case}");
    }
    assert!(fetcher.urls().is_empty(), "{:?}", fetcher.urls());
    assert!(host.staged().is_empty());
}

/// A bundle that does not list the entry's digest is `not_attested` and the verifier is not
/// asked; one bundle that lists the digests of two entries is accepted for each, and the verifier
/// is asked about each digest.
///
/// Catches a bundle for another artifact being accepted, and one bundle failing the second of
/// its entries.
#[test]
fn a_bundle_must_list_the_digest_and_one_bundle_may_list_several() {
    let host = Host::new("bundle-lists", false);
    let verifier = FakeVerifier::accepting();
    let other = bundle_listing(&[&sha256(WRONG)]);
    let unlisted = entry_of(ARCHIVE, &[("a", ARTIFACT_AT)], &other, &[("a", BUNDLE_AT)]);
    let fetcher = FakeFetcher::new(&host.clock)
        .route(&artifact_url(), Serve::Bytes(ARCHIVE.to_vec()))
        .route(&bundle_url(), Serve::Bytes(other));
    let refusal = host
        .run(&fetcher, &verifier, &unlisted)
        .expect_err("refused");
    assert_eq!(refusal, release("not_attested"));
    assert!(verifier.asked.borrow().is_empty());
    assert!(host.staged().is_empty());

    let shared = bundle_listing(&[&sha256(ARCHIVE), &sha256(WRONG)]);
    for archive in [ARCHIVE, WRONG] {
        let target = entry_of(archive, &[("a", ARTIFACT_AT)], &shared, &[("a", BUNDLE_AT)]);
        let fetcher = FakeFetcher::new(&host.clock)
            .route(&artifact_url(), Serve::Bytes(archive.to_vec()))
            .route(&bundle_url(), Serve::Bytes(shared.clone()));
        let proven = host.run(&fetcher, &verifier, &target).expect("proven");
        assert_eq!(proven.archive.sha256().as_str(), sha256(archive));
    }
    let asked: Vec<String> = verifier
        .asked
        .borrow()
        .iter()
        .map(|(d, _, _)| d.clone())
        .collect();
    assert_eq!(asked, [sha256(ARCHIVE), sha256(WRONG)]);

    let garbage = b"not a bundle at all".to_vec();
    let target = entry_of(
        ARCHIVE,
        &[("a", ARTIFACT_AT)],
        &garbage,
        &[("a", BUNDLE_AT)],
    );
    let fetcher = FakeFetcher::new(&host.clock)
        .route(&artifact_url(), Serve::Bytes(ARCHIVE.to_vec()))
        .route(&bundle_url(), Serve::Bytes(garbage));
    assert_eq!(
        host.run(&fetcher, &verifier, &target).expect_err("refused"),
        release("not_attested")
    );
}

/// The verifier's answer is the proof's: a rejection is `not_attested` (release) and an
/// unavailable or slow verifier is `attestation_unavailable` (deferred); in each the staging
/// files are gone.
///
/// Catches a rejected attestation let through, and a verifier fault blamed on the release.
#[test]
fn the_verifiers_answer_decides_and_leaves_no_file() {
    let cases = [
        (
            VerifyError::Rejected("no".to_owned()),
            release("not_attested"),
        ),
        (
            VerifyError::Unavailable,
            deferred("attestation_unavailable"),
        ),
        (VerifyError::TimedOut, deferred("attestation_unavailable")),
    ];
    for (error, expected) in cases {
        let host = Host::new("verifier", false);
        let verifier = FakeVerifier::answering(Err(error));
        let refusal = host
            .run(&published(&host), &verifier, &entry())
            .expect_err("refused");
        assert_eq!(refusal, expected);
        assert_eq!(verifier.asked.borrow().len(), 1);
        assert!(host.staged().is_empty());
    }
}

/// Orphan staging files of earlier fetches in the service's directory are removed before the
/// fetch, and other names are left.
///
/// Catches a fetch that leaves an earlier fetch's partial file, or removes a file that is not a
/// staging file.
#[test]
fn orphan_staging_files_are_removed_first() {
    let host = Host::new("orphans", false);
    let dir = host.root().join("svc");
    fs::create_dir(&dir).expect("the service directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    fs::write(dir.join(".fetch-1-1"), "partial").expect("an orphan");
    fs::write(dir.join("1.0.0"), "kept").expect("another file");
    let refusal = host
        .run(
            &FakeFetcher::new(&host.clock),
            &FakeVerifier::accepting(),
            &entry(),
        )
        .expect_err("nothing is served");
    assert_eq!(refusal, deferred("fetch_failed"));
    assert_eq!(host.staged(), ["1.0.0"]);
}

/// A service directory that group or others can write is refused as `fetch_failed`, without a
/// request.
///
/// Catches staging files made in a directory another account can write.
#[test]
fn a_service_directory_others_can_write_is_refused() {
    let host = Host::new("open-dir", false);
    let dir = host.root().join("svc");
    fs::create_dir(&dir).expect("the service directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("its mode");
    let fetcher = published(&host);
    let refusal = host
        .run(&fetcher, &FakeVerifier::accepting(), &entry())
        .expect_err("refused");
    assert_eq!(refusal, deferred("fetch_failed"));
    assert!(fetcher.urls().is_empty());
}

/// A store that gave other bytes at two sources (a mirror and a location of the same store, or
/// two locations in it) is reported once.
///
/// Catches a store listed once per bad source, which would log one store twice.
#[test]
fn a_store_that_gave_other_bytes_twice_is_reported_once() {
    for which in Which::BOTH {
        let host = Host::new("reported-once", false);
        let verifier = FakeVerifier::accepting();
        let target = match which {
            Which::Artifact => entry_of(
                ARCHIVE,
                &[("a", "x/one"), ("a", "x/two"), ("b", ARTIFACT_AT)],
                &bundle(),
                &[("a", BUNDLE_AT)],
            ),
            Which::Bundle => entry_of(
                ARCHIVE,
                &[("a", ARTIFACT_AT)],
                &bundle(),
                &[("a", "x/one"), ("a", "x/two"), ("b", BUNDLE_AT)],
            ),
        };
        let wrong = Serve::Bytes(wrong_for(which));
        let fetcher = with(
            &host,
            which,
            &[
                ("https://a.example/x/one".to_owned(), wrong.clone()),
                ("https://a.example/x/two".to_owned(), wrong),
                (
                    format!("https://b.example/{}", location_of(which)),
                    Serve::Bytes(which.bytes()),
                ),
            ],
        );
        let proven = host.run(&fetcher, &verifier, &target).expect("proven");
        assert_eq!(proven.other_bytes, ["a"], "{which:?}");
    }
}
