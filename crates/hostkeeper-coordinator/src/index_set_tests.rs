//! A set of the desired state over a release index: what the coordinator refuses and leaves
//! unchanged, what it keeps of an index that passed (its digest alone: the text is not read),
//! how the state file holds it and reads the files of the release before, and the conflict of a
//! second release for a version.

use std::fs;

use hostkeeper_wire::Platform;
use serde_json::{Value, json};

use crate::desired::{Desired, DesiredError, DesiredStore, Recorded, StoreError};
use crate::http::{DESIRED_PATH, Surface, handle};
use crate::index_set_support::{ALL, index_body, index_text, set_index, sha, try_set};
use crate::marks_support::{WEB, digest, ops_bytes, rig};
use crate::registry::{IndexKey, IndexRecord, Registry, RegistryError, Release, index_platform};
use crate::service::SetError;
use crate::testutil::{LEGACY, Rig, ci_token, version};

/// The bytes of the rig's state file and ops file.
fn files(rig: &Rig) -> (Vec<u8>, Option<Vec<u8>>) {
    (
        fs::read(rig.dir.path().join("desired.json")).expect("the state file"),
        ops_bytes(rig),
    )
}

/// The state file of the rig, read as JSON.
fn state_file(rig: &Rig) -> Value {
    serde_json::from_slice(&files(rig).0).expect("JSON")
}

/// `POST /v1/desired` of `body` with CI's token: the status and the JSON answer.
async fn post(rig: &Rig, body: &Value) -> (u16, Value) {
    let request = hyper::Request::builder()
        .method("POST")
        .uri(DESIRED_PATH)
        .header("authorization", format!("Bearer {}", ci_token()))
        .body(http_body_util::Full::new(bytes::Bytes::from(
            body.to_string(),
        )))
        .expect("a request");
    let response = handle(
        &rig.coordinator,
        Surface::Control,
        &None,
        request,
        std::time::Duration::from_secs(30),
    )
    .await;
    let status = response.status().as_u16();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("a body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).expect("JSON"))
}

/// The bodies of sets that name a release index and must be refused, each with the token it is
/// refused with. The index is that of `web` 2.0 over all three platforms.
fn refused_bodies() -> Vec<(&'static str, Value, &'static str)> {
    let text = index_text(WEB, "2.0", &ALL);
    let good = sha(&text);
    let with = |change: &dyn Fn(&mut Value)| {
        let mut body = index_body(WEB, "2.0", &text, &good);
        change(&mut body);
        body
    };
    let tampered = text.replace(&digest(1), &digest(9));
    vec![
        (
            "the index without its digest",
            with(&|b| {
                drop(
                    b.as_object_mut()
                        .and_then(|o| o.remove("release_index_digest")),
                )
            }),
            "index_invalid",
        ),
        (
            "the digest without its index",
            with(&|b| drop(b.as_object_mut().and_then(|o| o.remove("release_index")))),
            "index_invalid",
        ),
        (
            "the index beside a package digest",
            with(&|b| b["release_digest"] = json!(digest(1))),
            "index_invalid",
        ),
        (
            "an upper-case digest",
            with(&|b| b["release_index_digest"] = json!(good.to_uppercase())),
            "index_invalid",
        ),
        (
            "a short digest",
            with(&|b| b["release_index_digest"] = json!(&good[..63])),
            "index_invalid",
        ),
        (
            "an index that is not text",
            with(&|b| b["release_index"] = json!({"digest": good, "packages": {}})),
            "index_invalid",
        ),
        (
            "a digest that is not text",
            with(&|b| b["release_index_digest"] = json!(5)),
            "index_invalid",
        ),
        (
            "an index changed after its digest was taken",
            with(&|b| b["release_index"] = json!(tampered)),
            "index_digest_mismatch",
        ),
        (
            "an index cut short",
            with(&|b| b["release_index"] = json!(&text[..text.len() - 1])),
            "index_digest_mismatch",
        ),
    ]
}

/// Every set that names a release index it cannot take is refused with its token, over CI's JSON
/// route (400) as well as in process, and changes nothing: the state file and the ops file are
/// byte for byte what they were.
///
/// Catches: a tampered index stored, a refusal answered with another token or status, a pair of
/// keys taken half, and a refused set that writes.
#[tokio::test]
async fn a_set_over_an_index_it_cannot_take_is_refused_and_changes_nothing() {
    let rig = rig("index-refusals", "");
    set_digest(&rig, WEB, "1.0", 1).await;
    let before = files(&rig);
    for (what, body, token) in refused_bodies() {
        let refused = try_set(&rig, &body).await.expect_err(what);
        assert_eq!(refused.reason(), Some(token), "{what}: {refused}");
        assert_eq!(files(&rig), before, "{what} changed a file");
        let (status, answer) = post(&rig, &body).await;
        assert_eq!(
            (status, answer),
            (400, json!({ "error": token })),
            "{what} over the route"
        );
        assert_eq!(files(&rig), before, "{what} over the route changed a file");
    }
}

/// An index that passed leaves the coordinator the digest of the index and no package, in the
/// desired state and in the registry, in the one state file, and not the index's text; a store
/// opened over the file reads the same, and a version registered before keeps its row as it was.
///
/// Catches: the index's bytes kept, a package read out of text the coordinator does not parse, a
/// record that does not survive a restart, and a row of the registry rewritten.
#[tokio::test]
async fn an_index_is_kept_as_its_digest_alone() {
    let rig = rig("index-kept", "");
    set_digest(&rig, WEB, "1.0", 1).await;
    let index = set_index(&rig, WEB, "2.0", &ALL).await;
    let record = json!({"digest": index, "packages": {}});
    let file = state_file(&rig);
    let entry = &file["services"][WEB];
    assert_eq!(entry["version"], "2.0");
    assert_eq!(entry["release_index"], record);
    assert_eq!(entry.get("release_digest"), None);
    assert_eq!(entry["digests"]["2.0"], record);
    assert_eq!(entry["digests"]["1.0"], json!(digest(1)));
    let text = String::from_utf8(files(&rig).0).expect("UTF-8");
    assert!(!text.contains("schema"), "{text}");

    let store = DesiredStore::open(&rig.dir.path().join("desired.json"), Some(LEGACY))
        .expect("the state file");
    let kept = store.get(WEB).expect("web's state");
    assert_eq!(kept.release_digest, None);
    let record: IndexRecord = serde_json::from_value(record).expect("a record");
    assert_eq!(kept.release_index.as_ref(), Some(&record));
    assert_eq!(
        store.digests(WEB).get(&version("2.0")),
        Some(&Release::Index(record))
    );
}

/// A state file of the release before (a package digest and its registry) is read, and a set of
/// an index beside it rewrites the file with the old rows untouched; the same set again changes
/// nothing; a file that holds both a package digest and an index, or an index record this
/// release does not write, is refused.
///
/// Catches: an old file refused, an old row rewritten, a resend that rewrites the file, and a
/// hand-edited file that names two releases for one version taken.
#[test]
fn the_state_file_of_the_release_before_is_read_and_a_record_beside_it_is_kept() {
    let dir = crate::testutil::scratch("index-old-file");
    let path = dir.path().join("desired.json");
    let old = json!({
        "version": "1.0",
        "max_updating": {"frontend": 1, "storage": 1, "daemon": 2, "mac-daemon": 2},
        "min_up": {"storage": 0, "frontend": 0},
        "release_digest": digest(1),
        "digests": {"1.0": digest(1)},
    });
    fs::write(&path, serde_json::to_vec_pretty(&old).expect("JSON")).expect("written");
    let store = DesiredStore::open(&path, Some(LEGACY)).expect("the old file is read");
    let mut next: Desired = store.get(LEGACY).expect("a state");
    next.version = version("2.0");
    let record = record_of(&index_text(LEGACY, "2.0", &ALL));
    next.set_release(Some(Release::Index(record.clone())));
    assert_eq!(
        store.record(LEGACY, next.clone(), |_| false).ok(),
        Some(Recorded::Written)
    );
    let written: Value = serde_json::from_slice(&fs::read(&path).expect("file")).expect("JSON");
    assert_eq!(written["digests"]["1.0"], json!(digest(1)));
    assert_eq!(written["digests"]["2.0"]["digest"], json!(record.digest));
    assert_eq!(written["release_index"]["digest"], json!(record.digest));
    assert_eq!(written.get("release_digest"), None);
    assert_eq!(
        store.record(LEGACY, next, |_| false).ok(),
        Some(Recorded::Unchanged)
    );

    let both = |change: &dyn Fn(&mut Value)| {
        let mut file = written.clone();
        change(&mut file);
        fs::write(&path, serde_json::to_vec_pretty(&file).expect("JSON")).expect("written");
        DesiredStore::open(&path, Some(LEGACY)).expect_err("refused")
    };
    let refused = both(&|f| f["release_digest"] = json!(digest(1)));
    assert!(
        matches!(
            refused,
            StoreError::Invalid { source: DesiredError::Malformed(ref why), .. }
                if why.contains("exclusive")
        ),
        "{refused}"
    );
    for change in [
        |f: &mut Value| f["release_index"]["packages"]["windows-x86"] = json!(digest(4)),
        |f: &mut Value| f["release_index"]["extra"] = json!(1),
        |f: &mut Value| f["release_index"]["packages"]["linux-arm64"] = json!("short"),
        |f: &mut Value| f["digests"]["2.0"] = json!({"digest": digest(1)}),
    ] {
        assert!(matches!(both(&change), StoreError::Invalid { .. }));
    }
}

/// The record of the index `text`, made the way a set makes it, without a coordinator: its digest
/// alone.
fn record_of(text: &str) -> IndexRecord {
    IndexRecord {
        digest: crate::registry::Digest::parse(&sha(text)).expect("a digest"),
        packages: std::collections::BTreeMap::new(),
    }
}

/// A version already set with a release is not set with another: a second index for it, a
/// package digest for an index and an index for a package digest are each `digest_conflict`
/// (409) and change nothing, while the same index again is accepted and changes nothing.
///
/// Catches: a version whose packages changed under the same name taken because the digest of the
/// release was compared only for a package digest, and a resend refused or rewritten.
#[tokio::test]
async fn a_second_release_for_a_version_is_a_conflict() {
    let rig = rig("index-conflict", "");
    set_index(&rig, WEB, "2.0", &ALL).await;
    let set_once = files(&rig);
    set_index(&rig, WEB, "2.0", &ALL).await;
    assert_eq!(files(&rig), set_once, "the same index again writes nothing");

    set_digest(&rig, WEB, "3.0", 5).await;
    let before = files(&rig);
    let other = index_text(WEB, "2.0", &ALL[..2]);
    let three = index_text(WEB, "3.0", &ALL);
    for (what, body) in [
        (
            "another index",
            index_body(WEB, "2.0", &other, &sha(&other)),
        ),
        (
            "a package digest for an index",
            json!({"service": WEB, "version": "2.0", "min_up": {}, "release_digest": digest(1)}),
        ),
        (
            "an index for a package digest",
            index_body(WEB, "3.0", &three, &sha(&three)),
        ),
    ] {
        let refused = try_set(&rig, &body).await.expect_err(what);
        assert!(
            matches!(refused, SetError::DigestConflict),
            "{what}: {refused}"
        );
        assert_eq!(files(&rig), before, "{what} changed a file");
        let (status, answer) = post(&rig, &body).await;
        assert_eq!(
            (status, answer),
            (409, json!({"error": "digest_conflict"})),
            "{what}"
        );
    }
}

/// Only the two refusals of an index have a token of their own; every other fault of a body is
/// told as a bad request.
///
/// Catches: a token given to a malformed body, which would tell CI its index was at fault.
#[test]
fn only_the_refusals_of_an_index_have_a_token() {
    assert_eq!(
        DesiredError::IndexInvalid("why").reason(),
        Some("index_invalid")
    );
    assert_eq!(
        DesiredError::IndexDigestMismatch.reason(),
        Some("index_digest_mismatch")
    );
    for error in [
        DesiredError::Malformed("x".to_owned()),
        DesiredError::Count,
        DesiredError::UnknownService,
        DesiredError::NoLegacyService,
        DesiredError::Registry(RegistryError::Version("v".to_owned())),
    ] {
        assert_eq!(error.reason(), None, "{error}");
        assert_eq!(SetError::Invalid(error).reason(), None);
    }
    assert_eq!(
        SetError::Invalid(DesiredError::IndexDigestMismatch).reason(),
        Some("index_digest_mismatch")
    );
}

/// The mapping of a host's platform to the index's spelling is one function: the Mac is
/// `macos-arm64` in a release's list and `darwin-arm64` on the wire. A record's keys read the
/// index's spelling alone.
///
/// Catches: the Mac looked up by the wire's spelling, or two platforms mapped to one.
#[test]
fn the_wire_platforms_map_to_the_indexs_spelling() {
    let spelled: Vec<(&str, &str)> = Platform::ALL
        .iter()
        .map(|platform| (platform.as_str(), index_platform(*platform).as_str()))
        .collect();
    assert_eq!(
        spelled,
        [
            ("linux-x86_64", "linux-x86_64"),
            ("linux-arm64", "linux-arm64"),
            ("darwin-arm64", "macos-arm64"),
        ]
    );
    assert!(serde_json::from_value::<IndexKey>(json!("macos-arm64")).is_ok());
    assert!(serde_json::from_value::<IndexKey>(json!("darwin-arm64")).is_err());
    assert!(matches!(
        Registry::read(
            json!({"2.0": {"digest": digest(1), "packages": {"darwin-arm64": digest(2)}}})
        ),
        Err(RegistryError::Malformed(_))
    ));
}

/// Sets `service` at `version` over the package digest [`digest`] `n`, as the JSON route carries
/// it.
async fn set_digest(rig: &Rig, service: &str, version: &str, n: u8) {
    try_set(
        rig,
        &json!({"service": service, "version": version, "release_digest": digest(n)}),
    )
    .await
    .expect("the set is accepted");
}
