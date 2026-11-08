//! A set of the desired state with targets (`Coordinator::set_desired_with`, packages.md sections
//! 3.4 to 3.6) as the coordinator decides it, without a transport: what is stored (the third form
//! of the state and the release beside it), what is refused and in what order, what is audited,
//! how `--release-channel` is asked, and what `release_verified` reads back. The channel is a fake
//! fetcher: no network.

use std::sync::Arc;

use hostkeeper_config::platform::Platform;
use hostkeeper_oidc::testkit::WORKFLOW;
use serde_json::{Value, json};

use crate::channel::{ChannelRefusal, Fetched, ReleaseChannel, index_path};
use crate::channel_tests::FakeChannel;
use crate::desired::{self, DesiredStore};
use crate::ops::AuditEntry;
use crate::registry::{MAX_REGISTRY, Release as Registered};
use crate::release_index_support::{index_body, sha};
use crate::releases::{Release, Targets, audit_reason};
use crate::releases_tests::{
    agreeing_index, archive_targets, digest, entry, hex, index_of, list, targets as web_targets,
};
use crate::service::ci_calls::{ReleaseError, Setter};
use crate::service::{Set, SetError};
use crate::store::Fake;
use crate::testutil::{LEGACY, Rig, ci_token, version};
use crate::write_record::{WriteAsk, WriteRecord};

const OPERATOR_NAME: &str = "alice";

/// The state-file-side bytes a set could change: the state, the releases and the audit file.
fn files(rig: &Rig) -> Vec<Option<Vec<u8>>> {
    [
        "desired.json",
        "desired.json.releases",
        "desired.json.ops",
        "desired.json.ops.audit",
    ]
    .iter()
    .map(|name| std::fs::read(rig.dir.path().join(name)).ok())
    .collect()
}

fn body(at: &str) -> Value {
    json!({"version": at, "min_up": {"storage": 0, "frontend": 0}})
}

fn with(mut body: Value, key: &str, value: Value) -> Value {
    body[key] = value;
    body
}

fn targets(service: &str) -> Targets {
    web_targets(service)
}

type Answer = Result<(Set, Option<WriteRecord>), SetError>;

/// CI's set of `body` with `targets`, with a token no one has used.
async fn ci_sets(rig: &Rig, body: &Value, targets: Option<Targets>) -> Answer {
    let verified = rig
        .coordinator
        .ci(Some(&ci_token()))
        .await
        .expect("a token");
    rig.coordinator
        .set_desired_with(
            &Setter::Ci(&verified),
            body.to_string().as_bytes(),
            &WriteAsk::default(),
            targets,
        )
        .await
}

/// The operator `alice`'s set of `body` with `targets`.
async fn operator_sets(rig: &Rig, body: &Value, targets: Option<Targets>) -> Answer {
    rig.coordinator
        .set_desired_with(
            &Setter::Operator(OPERATOR_NAME),
            body.to_string().as_bytes(),
            &WriteAsk::default(),
            targets,
        )
        .await
}

fn audit(rig: &Rig) -> Vec<AuditEntry> {
    rig.coordinator.ops.audit_after(0).expect("the audit")
}

/// The `set_desired` entries of the audit as (by, service, reason).
fn set_entries(rig: &Rig) -> Vec<(String, Option<String>, Option<String>)> {
    audit(rig)
        .into_iter()
        .filter(|entry| entry.action == "set_desired")
        .map(|entry| (entry.by, entry.service, entry.reason))
        .collect()
}

fn kept(rig: &Rig, at: &str) -> Option<Arc<Release>> {
    rig.coordinator.desired.release(LEGACY, at)
}

/// Proves: CI's set with targets stores the third form of the state (the version, no package
/// digest and no index), keeps the release beside it (in memory and in the releases file), leaves
/// the registry of digests alone, and reads back through `release_verified` with the targets in the
/// order set; the state file holds neither the targets nor a digest key.
#[tokio::test]
async fn a_set_with_targets_stores_the_third_form_and_the_release_beside_it() {
    let rig = Rig::new("release-set-third-form");
    let (set, _) = ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("set");
    assert_eq!(set.desired.version, version("2.0"));
    assert_eq!(set.desired.release_digest, None);
    assert_eq!(set.desired.release_index, None);
    assert_eq!(rig.coordinator.desired.digests(LEGACY).len(), 0);
    let release = kept(&rig, "2.0").expect("a release");
    assert_eq!(*release, Release::new(targets(LEGACY), None));
    let read = rig
        .coordinator
        .release_verified(LEGACY, "2.0")
        .expect("read back");
    assert!(Arc::ptr_eq(&read, &release));
    assert_eq!(
        read.targets()["linux-x86_64"].entries[0].installer,
        "deb",
        "the order set"
    );

    let state = std::fs::read_to_string(rig.dir.path().join("desired.json")).expect("the state");
    assert!(
        !state.contains("release_digest")
            && !state.contains("release_index")
            && !state.contains("tarball"),
        "{state}"
    );
    let on_disk =
        std::fs::read(rig.dir.path().join("desired.json.releases")).expect("the releases file");
    let books = crate::releases::parse_file(&on_disk).expect("parses");
    assert_eq!(*books[LEGACY]["2.0"], *release);
    // What an older build reads: the strict form of the state alone, with no release.
    let old = desired::parse_file(state.as_bytes(), Some(LEGACY)).expect("strict parse");
    assert_eq!(old.desired[LEGACY].version, version("2.0"));
    assert!(old.digests.is_empty());
}

/// Proves: a restarted coordinator reads the release back from the releases file, and the version
/// stays the desired one.
#[tokio::test]
async fn a_restart_reads_the_release_from_the_releases_file() {
    let mut rig = Rig::new("release-set-restart");
    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("set");
    rig.restart();
    let read = rig
        .coordinator
        .release_verified(LEGACY, "2.0")
        .expect("read back");
    assert_eq!(*read, Release::new(targets(LEGACY), None));
}

/// Proves: every set with targets is audited as `set_desired` before it is stored, CI's by `ci`
/// with the workflow in the reason and an operator's by the token's name without it, the reason
/// being `<version> with targets[ by workflow ..][ and release index ..]: ` and then the platforms
/// and entries; an identical repeat is accepted and audited again.
#[tokio::test]
async fn a_set_with_targets_is_audited_by_ci_and_by_the_operator() {
    let rig = Rig::new("release-set-audit");
    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("CI's set");
    operator_sets(&rig, &body("2.1"), Some(targets(LEGACY)))
        .await
        .expect("the operator's set");
    let release = Release::new(targets(LEGACY), None);
    let version_2_0 = version("2.0");
    assert_eq!(
        set_entries(&rig),
        [
            (
                "ci".to_owned(),
                Some(LEGACY.to_owned()),
                Some(audit_reason(&version_2_0, &release, Some(WORKFLOW)))
            ),
            (
                OPERATOR_NAME.to_owned(),
                Some(LEGACY.to_owned()),
                Some(audit_reason(&version("2.1"), &release, None))
            ),
        ]
    );
    let reason = set_entries(&rig)[0].2.clone().expect("a reason");
    assert!(
        reason.starts_with(&format!(
            "2.0 with targets by workflow {WORKFLOW}: linux-x86_64 deb {} manifest ",
            hex('a')
        )),
        "{reason}"
    );
    assert!(
        reason.contains(&format!("; macos-arm64 tarball {} manifest ", hex('d'))),
        "{reason}"
    );

    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("the identical repeat");
    assert_eq!(set_entries(&rig).len(), 3, "audited again");
}

/// Proves: a set without targets is audited as it was: an operator's with its version, its package
/// digest or its index; CI's not at all.
#[tokio::test]
async fn a_set_without_targets_is_audited_as_before() {
    let rig = Rig::new("release-set-audit-plain");
    let plain = body("1.0");
    let package = with(body("1.1"), "release_digest", json!(hex('1')));
    let text = index_of(LEGACY, "1.2", &[(Platform::LinuxX86_64, '3', 9)]);
    let index_digest = sha(&text);
    let indexed = index_body(LEGACY, "1.2", &text, &index_digest);
    ci_sets(&rig, &plain, None).await.expect("CI's set");
    assert_eq!(set_entries(&rig), []);
    for body in [&plain, &package, &indexed] {
        operator_sets(&rig, body, None).await.expect("set");
    }
    let reasons: Vec<_> = set_entries(&rig)
        .into_iter()
        .map(|(by, _, reason)| (by, reason.expect("a reason")))
        .collect();
    assert_eq!(
        reasons,
        [
            (OPERATOR_NAME.to_owned(), "1.0".to_owned()),
            (
                OPERATOR_NAME.to_owned(),
                format!("1.1 with package digest {}", hex('1'))
            ),
            (
                OPERATOR_NAME.to_owned(),
                format!("1.2 with release index {index_digest}")
            ),
        ]
    );
}

/// Proves: targets set beside a package digest are refused `index_invalid`, targets that break a
/// rule of the release `target_invalid` (no platform; an entry of no bytes, by exact message),
/// each with nothing written, nothing audited and no release kept.
#[tokio::test]
async fn refused_targets_change_nothing() {
    let rig = Rig::new("release-set-target-refusals");
    ci_sets(&rig, &body("1.0"), None)
        .await
        .expect("a first set");
    let before = files(&rig);

    let beside_digest = with(body("2.0"), "release_digest", json!(hex('1')));
    let refused = ci_sets(&rig, &beside_digest, Some(targets(LEGACY)))
        .await
        .expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "index_invalid: targets are not set with a package digest"
    );
    assert_eq!(refused.reason(), Some("index_invalid"));

    let refused = ci_sets(&rig, &body("2.0"), Some(Targets::new()))
        .await
        .expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "target_invalid: the targets name no platform"
    );
    assert_eq!(refused.reason(), Some("target_invalid"));

    let mut empty = targets(LEGACY);
    empty
        .get_mut("macos-arm64")
        .expect("a list")
        .entries
        .push(entry(LEGACY, "pkg", 'e', 0));
    let refused = operator_sets(&rig, &body("2.0"), Some(empty))
        .await
        .expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "target_invalid: entry 1 of macos-arm64: the artifact's bytes is 0"
    );
    assert_eq!(files(&rig), before);
    assert!(kept(&rig, "2.0").is_none());
}

/// Proves: with a release index the targets must agree with it: an index that lists exactly the
/// archive entries is kept as the release's index digest (the state keeps no index, the registry
/// no digest) and named in the audit reason; one that disagrees is `index_invalid`, a digest that
/// is not the index's `index_digest_mismatch`, with nothing changed.
#[tokio::test]
async fn a_set_with_an_index_keeps_its_digest_beside_the_targets() {
    let rig = Rig::new("release-set-index");
    let text = agreeing_index(LEGACY, "2.0");
    let index_digest = sha(&text);
    let (set, _) = ci_sets(
        &rig,
        &index_body(LEGACY, "2.0", &text, &index_digest),
        Some(archive_targets(LEGACY)),
    )
    .await
    .expect("set");
    assert_eq!(set.desired.release_index, None);
    assert_eq!(set.desired.release_digest, None);
    assert_eq!(rig.coordinator.desired.digests(LEGACY).len(), 0);
    let release = kept(&rig, "2.0").expect("a release");
    assert_eq!(
        release.index_digest().map(ToString::to_string),
        Some(index_digest.clone())
    );
    let reason = set_entries(&rig)[0].2.clone().expect("a reason");
    assert!(
        reason.starts_with(&format!(
            "2.0 with targets by workflow {WORKFLOW} and release index {index_digest}: "
        )),
        "{reason}"
    );

    let before = files(&rig);
    let other = index_of(LEGACY, "2.1", &[(Platform::LinuxX86_64, '1', 1_111)]);
    let refused = ci_sets(
        &rig,
        &index_body(LEGACY, "2.1", &other, &sha(&other)),
        Some(archive_targets(LEGACY)),
    )
    .await
    .expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "index_invalid: release_index does not list exactly the archive entries of the targets"
    );
    let refused = ci_sets(
        &rig,
        &index_body(LEGACY, "2.1", &other, &hex('0')),
        Some(archive_targets(LEGACY)),
    )
    .await
    .expect_err("refused");
    assert_eq!(refused.reason(), Some("index_digest_mismatch"));
    assert_eq!(files(&rig), before);
}

/// Proves: a version set before with other targets (or the same targets and another index digest)
/// is refused `release_exists`, with nothing written or audited and the release kept; the identical
/// set is accepted again.
#[tokio::test]
async fn a_version_set_as_another_release_is_refused() {
    let rig = Rig::new("release-set-exists");
    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("set");
    let before = files(&rig);

    let mut other = targets(LEGACY);
    other.insert(
        "linux-arm64".to_owned(),
        list(vec![entry(LEGACY, "tarball", 'f', 5)]),
    );
    for attempt in [other, archive_targets(LEGACY)] {
        let refused = ci_sets(&rig, &body("2.0"), Some(attempt))
            .await
            .expect_err("refused");
        assert!(matches!(refused, SetError::ReleaseExists), "{refused:?}");
        assert_eq!(refused.reason(), Some("release_exists"));
        assert_eq!(
            refused.to_string(),
            "release_exists: the version was set before as another release"
        );
    }
    assert_eq!(files(&rig), before);
    assert_eq!(
        *kept(&rig, "2.0").expect("kept"),
        Release::new(targets(LEGACY), None)
    );

    let text = agreeing_index(LEGACY, "2.1");
    let indexed = index_body(LEGACY, "2.1", &text, &sha(&text));
    ci_sets(&rig, &indexed, Some(archive_targets(LEGACY)))
        .await
        .expect("set with an index");
    let mut no_index = archive_targets(LEGACY);
    no_index.get_mut("macos-arm64").expect("a list").entries[0].bytes = 2_222;
    let refused = ci_sets(&rig, &body("2.1"), Some(no_index))
        .await
        .expect_err("the same targets without the index are another release");
    assert!(matches!(refused, SetError::ReleaseExists), "{refused:?}");

    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("the identical repeat");
}

/// Proves: a version the registry already names (set before with a package digest, or with an
/// index) is refused `release_exists` when set with targets; a version that no registry names is
/// accepted.
#[tokio::test]
async fn a_version_the_registry_names_cannot_be_set_with_targets() {
    let rig = Rig::new("release-set-registry");
    let package = with(body("1.0"), "release_digest", json!(hex('1')));
    ci_sets(&rig, &package, None).await.expect("set");
    let text = index_of(LEGACY, "1.1", &[(Platform::LinuxX86_64, '3', 9)]);
    let indexed = index_body(LEGACY, "1.1", &text, &sha(&text));
    ci_sets(&rig, &indexed, None).await.expect("set");
    let before = files(&rig);
    for at in ["1.0", "1.1"] {
        let refused = ci_sets(&rig, &body(at), Some(targets(LEGACY)))
            .await
            .expect_err("refused");
        assert!(
            matches!(refused, SetError::ReleaseExists),
            "{at}: {refused:?}"
        );
    }
    assert_eq!(files(&rig), before);
    assert_eq!(
        rig.coordinator.desired.digests(LEGACY).get(&version("1.0")),
        Some(&Registered::Package(digest('1')))
    );
    ci_sets(&rig, &body("1.2"), Some(targets(LEGACY)))
        .await
        .expect("a version no registry names");
}

/// Proves: a set without targets that names a package digest or an index of a version that has a
/// release is refused `release_exists`; a plain set of that version (no digest, no index, no
/// targets) is accepted, changes the limits and leaves the release.
#[tokio::test]
async fn a_set_without_targets_cannot_name_a_digest_for_a_version_with_a_release() {
    let rig = Rig::new("release-set-plain-over-release");
    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("set");
    let before = files(&rig);
    let package = with(body("2.0"), "release_digest", json!(hex('1')));
    let text = index_of(LEGACY, "2.0", &[(Platform::LinuxX86_64, '3', 9)]);
    let indexed = index_body(LEGACY, "2.0", &text, &sha(&text));
    for body in [&package, &indexed] {
        let refused = ci_sets(&rig, body, None).await.expect_err("refused");
        assert!(matches!(refused, SetError::ReleaseExists), "{refused:?}");
    }
    assert_eq!(files(&rig), before);

    let limits = json!({"version": "2.0", "min_up": {"storage": 1, "frontend": 1}});
    let (set, _) = ci_sets(&rig, &limits, None).await.expect("a plain set");
    assert_eq!(set.desired.min_up.get("storage"), Some(1));
    assert_eq!(
        *kept(&rig, "2.0").expect("the release stays"),
        Release::new(targets(LEGACY), None)
    );
    let on_disk = crate::releases::parse_file(
        &std::fs::read(rig.dir.path().join("desired.json.releases")).expect("the file"),
    )
    .expect("parses");
    assert!(on_disk[LEGACY].contains_key("2.0"));
}

/// The rig whose coordinator asks the channel `fake` of every set it must.
fn channel_rig(test: &str, fake: &Arc<FakeChannel>) -> Rig {
    let mut rig = Rig::new(test);
    rig.coordinator =
        Rig::start(&rig.dir, &rig.clock).with_release_channel(ReleaseChannel::new(fake.clone()));
    rig
}

/// Proves: with `--release-channel` a set with targets and no index asks no channel (so the
/// channel's silence does not refuse it), a set with targets and an index asks it for the version
/// (refusing `not_in_channel` while it holds nothing, and accepting once it holds the very index),
/// and a set without targets is asked as before.
#[tokio::test]
async fn the_channel_is_asked_only_for_a_set_without_targets_or_with_an_index() {
    let fake = Arc::new(FakeChannel::default());
    let rig = channel_rig("release-set-channel", &fake);
    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("targets without an index");
    assert_eq!(fake.asked(), Vec::<String>::new());

    let text = agreeing_index(LEGACY, "2.1");
    let indexed = index_body(LEGACY, "2.1", &text, &sha(&text));
    let refused = ci_sets(&rig, &indexed, Some(archive_targets(LEGACY)))
        .await
        .expect_err("the channel holds nothing");
    assert!(
        matches!(refused, SetError::Channel(ChannelRefusal::NotInChannel)),
        "{refused:?}"
    );
    assert_eq!(fake.asked(), ["kbf/2.1/index.json"]);
    assert!(kept(&rig, "2.1").is_none());

    fake.answer(
        &index_path(LEGACY, &version("2.1")),
        Ok(Fetched::Found(text.clone().into_bytes())),
    );
    ci_sets(&rig, &indexed, Some(archive_targets(LEGACY)))
        .await
        .expect("the channel holds the index");
    assert!(kept(&rig, "2.1").is_some());

    let refused = ci_sets(&rig, &body("3.0"), None)
        .await
        .expect_err("a set without targets is asked");
    assert_eq!(refused.reason(), Some("not_in_channel"));
    assert_eq!(
        fake.asked(),
        [
            "kbf/2.1/index.json",
            "kbf/2.1/index.json",
            "kbf/3.0/index.json"
        ]
    );
}

/// Proves: a set with targets of a service already holding the most releases drops its oldest
/// release no mark keeps in the same write and keeps the one just set, the decision being made
/// under the marks of the service.
#[tokio::test]
async fn a_set_past_the_cap_drops_the_oldest_release() {
    let mut held = desired::Held::default();
    let old = Arc::new(Release::new(Targets::new(), None));
    held.releases.insert(
        LEGACY.to_owned(),
        (0..MAX_REGISTRY)
            .map(|n| (format!("1.{n}"), Arc::clone(&old)))
            .collect(),
    );
    let mut rig = Rig::new("release-set-past-cap");
    let text = hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER);
    rig.coordinator = Rig::start_over(
        DesiredStore::over(Arc::new(Fake::new()), held),
        &rig.clock,
        &text,
    );
    operator_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("set");
    assert!(kept(&rig, "1.0").is_none());
    assert!(kept(&rig, "1.1").is_some());
    assert!(kept(&rig, "2.0").is_some());
}

/// Proves: `release_verified` answers by cause: a service the coordinator does not manage is
/// `UnknownService`, a version never set with targets (set with a digest, or never set) is
/// `Unknown`.
#[tokio::test]
async fn a_release_is_read_by_service_and_version() {
    let rig = Rig::new("release-set-read");
    ci_sets(&rig, &body("1.0"), None).await.expect("set");
    ci_sets(&rig, &body("2.0"), Some(targets(LEGACY)))
        .await
        .expect("set");
    assert_eq!(
        rig.coordinator.release_verified("nobody", "2.0").err(),
        Some(ReleaseError::UnknownService)
    );
    for at in ["1.0", "9.9"] {
        assert_eq!(
            rig.coordinator.release_verified(LEGACY, at).err(),
            Some(ReleaseError::Unknown)
        );
    }
    assert!(rig.coordinator.release_verified(LEGACY, "2.0").is_ok());
}

/// Proves: past the cap of releases a service keeps, a write with a release drops the oldest
/// release no mark keeps and keeps the one just set, in the same write as the state: the first
/// version set falls off at the 4097th, and one a mark keeps stays.
#[test]
fn the_releases_of_a_service_are_capped_at_the_registrys_cap() {
    let store = DesiredStore::over(Arc::new(Fake::new()), desired::Held::default());
    let release = Arc::new(Release::new(
        Targets::from([(
            "macos-arm64".to_owned(),
            list(vec![entry(LEGACY, "tarball", 'd', 2_000)]),
        )]),
        None,
    ));
    for n in 0..=MAX_REGISTRY {
        store
            .record_as(
                LEGACY,
                crate::testutil::desired(&format!("1.{n}"), 0, 0),
                |at| at.as_str() == "1.1",
                &WriteAsk::default(),
                Some(release.clone()),
            )
            .expect("written");
    }
    assert!(store.release(LEGACY, "1.0").is_none());
    assert!(store.release(LEGACY, "1.1").is_some(), "kept by a mark");
    assert!(store.release(LEGACY, "1.2").is_some());
    assert!(store.release(LEGACY, "1.3").is_some());
    assert!(
        store
            .release(LEGACY, &format!("1.{MAX_REGISTRY}"))
            .is_some()
    );
    assert_eq!(store.release("web", "1.3"), None);
}
