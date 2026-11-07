//! The package digest in the desired state and the registry of the versions CI has set, and
//! the refusals of CI's set (design sections 8.1, 8.10 and 10): a version below the floor, one
//! the version scheme cannot read, one marked bad, and a second digest for a version are each
//! refused before anything changes; the registry is written with the state, in one replacement
//! of the file.

use std::fs;
use std::path::Path;

use hostkeeper_wire::version::Version;
use serde_json::{Value, json};

use crate::desired::{Desired, DesiredError, DesiredStore, Recorded, StoreError};
use crate::http::{DESIRED_PATH, Surface, handle};
use crate::ops_tests::temp_beside;
use crate::registry::{Digest, DigestError, MAX_REGISTRY, Registry, RegistryError, Release};
use crate::rollback_tests::{
    AUTO, ONE, THREE, TWO, marks, persisted, restart, rig, round, seed, two_failed, wanted,
};
use crate::service::SetError;
use crate::services::VersionScheme;
use crate::testutil::{LEGACY, Rig, ci_token, desired, version};

const D1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const D2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// A desired-state body of `version`, with `digest` as its `release_digest` when there is one.
fn body(version: &str, digest: Option<&str>) -> String {
    let mut body = json!({"version": version, "min_up": {"storage": 0, "frontend": 0}});
    if let Some(digest) = digest {
        body["release_digest"] = json!(digest);
    }
    body.to_string()
}

/// CI's set of `body`, with a token no one has used.
async fn set(rig: &Rig, body: &str) -> Result<Desired, SetError> {
    rig.coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
}

fn digest(text: &str) -> Digest {
    Digest::parse(text).expect("a digest")
}

/// The release of one package digest.
fn package(text: &str) -> Release {
    Release::Package(digest(text))
}

/// The bytes of the rig's state file and ops file.
fn files(rig: &Rig) -> (Vec<u8>, Option<Vec<u8>>) {
    let dir = rig.dir.path();
    (
        fs::read(dir.join("desired.json")).expect("the state file"),
        fs::read(dir.join("desired.json.ops")).ok(),
    )
}

fn state_file(rig: &Rig) -> Value {
    serde_json::from_slice(&files(rig).0).expect("JSON")
}

fn reopened(path: &Path) -> DesiredStore {
    DesiredStore::open(path, Some(LEGACY)).expect("the state file")
}

/// Catches: the digest dropped, or the registry written apart from the state: after a set the
/// file and a store reopened over it hold both; a replacement of the file that fails before its
/// rename leaves neither, in the file or in memory, and one whose rename landed leaves both.
#[tokio::test]
async fn set_desired_records_the_digest_with_the_version() {
    let rig = rig("digest-records", "", ONE).await;
    let set = set(&rig, &body("2.0", Some(D1))).await.expect("set");
    assert_eq!(set.release_digest, Some(digest(D1)));
    let file = state_file(&rig);
    assert_eq!(file["version"], "2.0");
    assert_eq!(file["release_digest"], D1);
    assert_eq!(file["digests"], json!({"2.0": D1}));
    let path = rig.dir.path().join("desired.json");
    let store = reopened(&path);
    assert_eq!(
        store.get(LEGACY).and_then(|d| d.release_digest),
        Some(digest(D1))
    );
    assert_eq!(
        store.digests(LEGACY).get(&version("2.0")),
        Some(&package(D1))
    );

    let mut three = desired("3.0", 0, 0);
    three.release_digest = Some(digest(D2));
    let blocker = temp_beside(&path);
    fs::create_dir(&blocker).expect("a directory where the temporary file goes");
    let failed = store.record(LEGACY, three.clone(), |_| false);
    assert!(matches!(failed, Err(StoreError::Io { .. })), "{failed:?}");
    assert_eq!(store.get(LEGACY).map(|d| d.version), Some(version("2.0")));
    assert_eq!(store.digests(LEGACY).get(&version("3.0")), None);
    let after = reopened(&path);
    assert_eq!(after.get(LEGACY).map(|d| d.version), Some(version("2.0")));
    assert_eq!(after.digests(LEGACY).get(&version("3.0")), None);
    fs::remove_dir(&blocker).expect("unblocked");

    let unsynced = DesiredStore::open_syncing(&path, Some(LEGACY), |_| {
        Err(std::io::Error::other("no sync"))
    })
    .expect("the state file");
    let landed = unsynced.record(LEGACY, three, |_| false);
    assert!(
        matches!(landed, Err(StoreError::NotSynced { .. })),
        "{landed:?}"
    );
    assert_eq!(
        unsynced.digests(LEGACY).get(&version("3.0")),
        Some(&package(D2))
    );
    let after = reopened(&path);
    assert_eq!(after.get(LEGACY).map(|d| d.version), Some(version("3.0")));
    assert_eq!(
        after.digests(LEGACY).get(&version("2.0")),
        Some(&package(D1))
    );
    assert_eq!(
        after.digests(LEGACY).get(&version("3.0")),
        Some(&package(D2))
    );
}

/// Catches: any one of the three refusals writing before it refuses, or answered with another
/// reason: a version below `min_version`, one the version scheme cannot read and one marked bad
/// are each refused with their own token, and the state file and the ops file are byte for byte
/// as they were.
#[tokio::test]
async fn set_desired_refuses_below_floor_unparseable_and_bad() {
    let floored = "min_version = \"1.0\"";
    let mut rig = rig("digest-refusals", floored, ONE).await;
    seed(&rig, |m| m.bad = vec![version("2.0")]);
    restart(&mut rig, floored);
    let before = files(&rig);
    for (version, refusal) in [
        ("0.9", "below_min_version"),
        ("1..2", "unparseable_version"),
        ("2.0", "bad_version"),
    ] {
        let refused = set(&rig, &body(version, Some(D1))).await;
        let why = refused.expect_err("refused");
        assert_eq!(why.reason(), Some(refusal), "{version}: {why}");
        assert!(why.to_string().starts_with(refusal), "{why}");
        assert_eq!(files(&rig), before, "{version} changed a file");
    }
    assert_eq!(wanted(&rig), version("1.0"));
    assert!(
        reopened(&rig.dir.path().join("desired.json"))
            .digests(LEGACY)
            .is_empty()
    );
}

/// Catches: CI lifting the mark of a version the coordinator rolled back from: the set is
/// refused and the desired state stays the version rolled back to.
#[tokio::test]
async fn set_desired_of_a_bad_version_is_refused() {
    let rig = rig("digest-bad", AUTO, ONE).await;
    round(&rig, 2_000, &[]);
    set(&rig, TWO).await.expect("set");
    round(&rig, 2_030, &two_failed());
    assert_eq!(wanted(&rig), version("1.0"));
    let refused = set(&rig, TWO).await;
    assert!(matches!(refused, Err(SetError::BadVersion)), "{refused:?}");
    assert_eq!(wanted(&rig), version("1.0"));
}

/// Catches: a CI write that unmarks: after a refused set of the bad version and an accepted set
/// of another, the mark stands in memory, in the file and after a restart, and the audit has no
/// unmark.
#[tokio::test]
async fn ci_write_does_not_lift_a_bad_mark() {
    let mut rig = rig("digest-mark-stays", AUTO, ONE).await;
    round(&rig, 2_000, &[]);
    set(&rig, TWO).await.expect("set");
    round(&rig, 2_030, &two_failed());
    let _ = set(&rig, TWO).await;
    set(&rig, THREE).await.expect("another version");
    assert_eq!(marks(&rig).bad, vec![version("2.0")]);
    assert_eq!(persisted(&rig).bad, vec![version("2.0")]);
    restart(&mut rig, AUTO);
    assert_eq!(marks(&rig).bad, vec![version("2.0")]);
    let audit = crate::rollback_tests::status(&rig).ops.audit;
    assert!(audit.iter().all(|e| e.action != "unmark_bad"), "{audit:?}");
}

/// Catches: a changed artifact for a version accepted: CI's second digest for 2.0 is refused,
/// with nothing written, while 2.0 is desired and after another version is.
#[tokio::test]
async fn set_desired_refuses_a_second_digest_for_a_version() {
    let rig = rig("digest-conflict", "", ONE).await;
    set(&rig, &body("2.0", Some(D1))).await.expect("set");
    let before = files(&rig);
    let refused = set(&rig, &body("2.0", Some(D2))).await;
    assert!(
        matches!(refused, Err(SetError::DigestConflict)),
        "{refused:?}"
    );
    assert_eq!(files(&rig), before);
    set(&rig, &body("3.0", Some(D2)))
        .await
        .expect("another version");
    let refused = set(&rig, &body("2.0", Some(D2))).await;
    assert!(
        matches!(refused, Err(SetError::DigestConflict)),
        "{refused:?}"
    );
    assert_eq!(wanted(&rig), version("3.0"));
}

/// Catches: a resend of the same set rewriting the file or refused as a conflict; and a set
/// with no digest losing the registry's row: the same version and digest again is accepted and
/// leaves the file as it was, and a set without a digest stores none and keeps the row.
#[tokio::test]
async fn the_same_digest_again_is_accepted_and_changes_nothing() {
    let rig = rig("digest-same", "", ONE).await;
    set(&rig, &body("2.0", Some(D1))).await.expect("set");
    let path = rig.dir.path().join("desired.json");
    let before = files(&rig);
    let blocker = temp_beside(&path);
    fs::create_dir(&blocker).expect("no write could land");
    let again = set(&rig, &body("2.0", Some(D1))).await.expect("accepted");
    assert_eq!(again.release_digest, Some(digest(D1)));
    assert_eq!(files(&rig), before);
    let store = reopened(&path);
    let mut same = desired("2.0", 0, 0);
    same.release_digest = Some(digest(D1));
    assert_eq!(
        store.record(LEGACY, same, |_| false).ok(),
        Some(Recorded::Unchanged)
    );
    fs::remove_dir(&blocker).expect("unblocked");

    let plain = set(&rig, &body("2.0", None)).await.expect("accepted");
    assert_eq!(plain.release_digest, None);
    let file = state_file(&rig);
    assert_eq!(file.get("release_digest"), None);
    assert_eq!(file["digests"], json!({"2.0": D1}));
}

/// Catches: a digest that is not 64 lowercase hex characters stored: it is a malformed body.
#[tokio::test]
async fn a_digest_that_is_not_sixty_four_lowercase_hex_is_malformed() {
    let rig = rig("digest-malformed", "", ONE).await;
    let (upper, long) = ("AB".repeat(32), format!("{D1}0"));
    for bad in [&D1[1..], upper.as_str(), long.as_str(), "z"] {
        let refused = set(&rig, &body("2.0", Some(bad))).await;
        assert!(
            matches!(refused, Err(SetError::Invalid(DesiredError::Malformed(_)))),
            "{bad}: {refused:?}"
        );
    }
    assert_eq!(wanted(&rig), version("1.0"));
}

/// Catches: the refusals answered over HTTP as a bad request, or with text other than their
/// token: `unparseable_version` is 400, and `below_min_version`, `bad_version` and
/// `digest_conflict` are 409.
#[tokio::test]
async fn the_refusals_are_answered_with_their_token() {
    let floored = "min_version = \"1.0\"";
    let mut rig = rig("digest-http", floored, ONE).await;
    seed(&rig, |m| m.bad = vec![version("2.0")]);
    restart(&mut rig, floored);
    set(&rig, &body("3.0", Some(D1))).await.expect("set");
    for (version, digest, code, token) in [
        ("1..2", None, 400, "unparseable_version"),
        ("0.9", None, 409, "below_min_version"),
        ("2.0", None, 409, "bad_version"),
        ("3.0", Some(D2), 409, "digest_conflict"),
    ] {
        let request = hyper::Request::builder()
            .method("POST")
            .uri(DESIRED_PATH)
            .header("authorization", format!("Bearer {}", ci_token()))
            .body(http_body_util::Full::new(bytes::Bytes::from(body(
                version, digest,
            ))))
            .expect("a request");
        let response = handle(
            &rig.coordinator,
            Surface::Control,
            &None,
            request,
            std::time::Duration::from_secs(30),
        )
        .await;
        assert_eq!(response.status().as_u16(), code, "{version}");
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("a body")
            .to_bytes();
        let answer: Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(answer, json!({"error": token}), "{version}");
    }
}

/// Catches: a rollback that keeps the digest of the version it leaves: the desired state gone
/// back to names the digest CI registered for that version.
#[tokio::test]
async fn a_rollback_names_the_digest_of_the_version_it_goes_back_to() {
    let rig = rig("digest-rollback", AUTO, &body("1.0", Some(D1))).await;
    round(&rig, 2_000, &[]);
    set(&rig, &body("2.0", Some(D2))).await.expect("set");
    round(&rig, 2_030, &two_failed());
    let back = crate::rollback_tests::status(&rig).services[LEGACY].clone();
    assert_eq!(back.version, version("1.0"));
    assert_eq!(back.release_digest, Some(digest(D1)));
}

/// Catches: the cap not held, or a kept version dropped at it: past [`MAX_REGISTRY`] versions
/// the oldest one not kept is dropped (by number, not by text); when every other is kept, none
/// is, and the registry holds one more.
#[test]
fn the_registry_drops_the_oldest_version_not_kept_at_its_cap() {
    let mut registry = Registry::default();
    let v = |n: usize| Version::parse(&format!("1.{n}")).expect("a version");
    for n in 1..=MAX_REGISTRY {
        registry.record(&v(n), package(D1), |_| false);
    }
    assert_eq!(registry.len(), MAX_REGISTRY);
    let kept = v(1);
    registry.record(&v(MAX_REGISTRY + 1), package(D2), |x| *x == kept);
    assert_eq!(registry.len(), MAX_REGISTRY);
    assert!(registry.get(&v(1)).is_some(), "kept");
    assert!(
        registry.get(&v(2)).is_none(),
        "the oldest not kept, by number"
    );
    assert!(registry.get(&v(10)).is_some(), "1.10 is newer than 1.2");
    assert_eq!(registry.get(&v(MAX_REGISTRY + 1)), Some(&package(D2)));

    registry.record(&v(MAX_REGISTRY + 2), package(D2), |_| true);
    assert_eq!(registry.len(), MAX_REGISTRY + 1, "every one is kept");
}

/// Catches: a digest's rule loosened, or a registry read that takes what is not one.
#[test]
fn a_digest_and_a_registry_are_read_strictly() {
    assert_eq!(digest(D1).as_str(), D1);
    assert_eq!(digest(D1).to_string(), D1);
    assert_eq!(String::from(digest(D1)), D1);
    assert_eq!(Digest::try_from("AB".repeat(32)), Err(DigestError));
    assert_eq!(digest(&"ab".repeat(32)).as_str(), "ab".repeat(32));
    assert_eq!(
        DigestError.to_string(),
        "a digest is 64 lowercase hex characters"
    );
    assert_eq!(Registry::read(json!({"2.0": D1})).map(|r| r.len()), Ok(1));
    assert!(matches!(
        Registry::read(json!(["2.0"])),
        Err(RegistryError::Malformed(_))
    ));
    assert!(matches!(
        Registry::read(json!({"2.0": "x"})),
        Err(RegistryError::Malformed(_))
    ));
    assert_eq!(
        Registry::read(json!({"v2": D1})),
        Err(RegistryError::Version("v2".to_owned()))
    );
}

/// Catches: a state file with a bad registry, or a malformed map form, read as if it were fine:
/// each refuses the open, as any state the store cannot read does.
#[test]
fn a_state_file_with_a_bad_registry_or_shape_is_refused() {
    let dir = crate::testutil::scratch("digest-bad-file");
    let path = dir.path().join("desired.json");
    let state = r#""version":"2.0","min_up":{"storage":0,"frontend":0}"#;
    for text in [
        format!(r#"{{{state},"digests":{{"x":"{D1}"}}}}"#),
        format!(r#"{{"services":{{"kbf":{{{state},"digests":[]}}}}}}"#),
        format!(r#"{{"services":{{"kbf":{{{state}}}}},"other":1}}"#),
        r#"{"services":7}"#.to_owned(),
        r#"{"services":{"kbf":7}}"#.to_owned(),
        "not json".to_owned(),
    ] {
        fs::write(&path, &text).expect("written");
        let refused = DesiredStore::open(&path, Some(LEGACY));
        assert!(
            matches!(refused, Err(StoreError::Invalid { .. })),
            "{text}: {refused:?}"
        );
    }
}

/// Catches: the file of the release before refused, or rewritten with keys it would refuse:
/// the recorded single-state and map files (what the coordinator before this one read and
/// wrote) are read with no digest and no registry, and a set without a digest writes neither
/// key.
#[tokio::test]
async fn an_old_state_file_still_reads() {
    for (name, text) in [
        (
            "desired-single.json",
            include_str!("../tests/fixtures/recorded/desired-single.json"),
        ),
        (
            "desired-map.json",
            include_str!("../tests/fixtures/recorded/desired-map.json"),
        ),
    ] {
        let dir = crate::testutil::scratch(&format!("digest-old-{name}"));
        let path = dir.path().join("desired.json");
        fs::write(&path, text).expect("written");
        let store = DesiredStore::open(&path, Some(LEGACY)).expect(name);
        let all = store.all();
        assert!(!all.is_empty(), "{name}");
        for (service, state) in &all {
            assert_eq!(state.release_digest, None, "{name}: {service}");
            assert!(store.digests(service).is_empty(), "{name}: {service}");
            store.set(service, state.clone()).expect("written back");
        }
        let written = fs::read_to_string(&path).expect("read");
        assert!(!written.contains("release_digest"), "{name}");
        assert!(!written.contains("digests"), "{name}");
    }
}

/// Catches: the version scheme reading a version it would order by a guess.
#[test]
fn the_semver_scheme_reads_release_numbers_and_a_suffix_only() {
    for good in [
        "1",
        "0",
        "1.0",
        "0.1.10+g0123456789ab-8",
        "1.2.3~rc1-1",
        "7.0.0-1~deb12u1",
        "20261021.1+git9ba38c2-2",
    ] {
        assert!(VersionScheme::Semver.parses(&version(good)), "{good}");
    }
    for bad in [
        "1..2",
        "1.",
        "1.0a",
        "01.2",
        "1.02",
        "00",
        "99999999999999999999.0",
    ] {
        assert!(!VersionScheme::Semver.parses(&version(bad)), "{bad}");
    }
}

/// Catches: the tie between two versions of the same release numbers broken by chance: at the
/// cap, of `1.0` and `1.0.0` (the same release), the one first by text is dropped.
#[test]
fn the_registry_breaks_a_tie_of_release_numbers_by_text() {
    let mut registry = Registry::default();
    for n in 0..MAX_REGISTRY - 2 {
        registry.record(&version(&format!("2.{n}")), package(D1), |_| false);
    }
    registry.record(&version("1.0.0"), package(D1), |_| false);
    registry.record(&version("1.0"), package(D1), |_| false);
    registry.record(&version("3.0"), package(D2), |_| false);
    assert_eq!(registry.len(), MAX_REGISTRY);
    assert!(registry.get(&version("1.0")).is_none());
    assert!(registry.get(&version("1.0.0")).is_some());
}

/// Catches: the registry dropping the last good version or a bad one at its cap, or keeping a
/// version for being anything else; and a refusal token given to an error that is no refusal of
/// the version.
#[test]
fn the_cap_keeps_the_last_good_and_bad_versions_and_only_refusals_have_a_token() {
    let marks = crate::ops::ServiceOps {
        last_good: Some(version("1.0")),
        bad: vec![version("2.0")],
        ..crate::ops::ServiceOps::default()
    };
    assert!(crate::service::kept(&marks, &version("1.0")));
    assert!(crate::service::kept(&marks, &version("2.0")));
    assert!(!crate::service::kept(&marks, &version("3.0")));
    for error in [
        SetError::Forbidden,
        SetError::Unavailable,
        SetError::Unauthorized("missing"),
    ] {
        assert_eq!(error.reason(), None, "{error}");
    }
}
