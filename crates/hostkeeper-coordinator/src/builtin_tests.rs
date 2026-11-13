//! The built-in repository `komira` ([`crate::builtin`]): a valid entry whose keyring holds the
//! pinned fingerprints, answered by a fresh registry, replaced by an entry put over it, and
//! removable only as that entry.

use hostkeeper_release::descriptor;
use hostkeeper_release::registry::{self, keys};

use crate::builtin::{self, BASE_URI, NAME, PINNED};
use crate::marks_support::{operator, rig_on};
use crate::repositories::Registry;
use crate::repositories_support::komira;
use crate::store::Fake;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::RepositoriesRefusal as Refusal;

/// Proves: the built-in entry passes every rule of the registry, its keyring holds the pinned
/// fingerprints (the primary key and its signing subkey, and no other key), its descriptor reads
/// and names its own apt root, channels and components, and its macOS format maps hello to
/// hostkeeper's directory.
///
/// Catches: a keyring that is not the one pinned (the root of trust of every host's apt for this
/// repository), or a descriptor and entry that disagree.
#[test]
fn builtin_komira_is_a_valid_entry() {
    let kept = builtin::komira();
    let repository = &kept.repository;
    assert_eq!(kept.generation, 0);
    assert_eq!(registry::check(repository), Ok(()));
    let found = keys(&repository.keyring).expect("a keyring");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].fingerprint, PINNED[0]);
    assert_eq!(found[0].subkeys, [PINNED[1]]);
    assert_eq!(repository.fingerprints, PINNED);
    let parsed = descriptor::check(&repository.descriptor).expect("a descriptor");
    assert_eq!(parsed.apt_uri(BASE_URI), repository.uri);
    assert_eq!(parsed.channels, repository.suites);
    assert_eq!(parsed.fingerprints, repository.fingerprints);
    assert_eq!(parsed.keyring, "apt/komira-archive-keyring.gpg");
    let mac = parsed.macos_pkg.expect("the macOS format");
    assert_eq!(mac.platforms, ["macos-arm64"]);
    assert_eq!(mac.dir("hello"), "hostkeeper");
    assert_eq!(mac.dir("hostkeeper"), "hostkeeper");
    assert_eq!(repository.name, NAME);
}

/// Proves: the pin is exactly the two fingerprints, in any order, and no other set (fewer, more,
/// others, a repeat).
///
/// Catches: a key pinned by a prefix, by one of its fingerprints, or with an extra one.
#[test]
fn the_pin_is_exactly_the_two_fingerprints() {
    let own = |prints: &[&str]| -> Vec<String> { prints.iter().map(|p| (*p).to_owned()).collect() };
    assert!(builtin::is_pinned(&own(&[PINNED[0], PINNED[1]])));
    assert!(builtin::is_pinned(&own(&[PINNED[1], PINNED[0]])));
    assert!(!builtin::is_pinned(&own(&[PINNED[0]])));
    assert!(!builtin::is_pinned(&own(&[
        PINNED[0],
        PINNED[1],
        &"A".repeat(40)
    ])));
    assert!(!builtin::is_pinned(&own(&[PINNED[0], &"A".repeat(40)])));
    assert!(!builtin::is_pinned(&own(&[PINNED[0], PINNED[0]])));
}

/// Proves: a registry that holds nothing answers the built-in entry for `get`, `listed` and
/// `entries_of`, is not "unregistered" for it, and counts no entry against the cap.
///
/// Catches: a fresh coordinator that needs a repository put before any package can be added.
#[test]
fn a_fresh_registry_answers_komira() {
    let fresh = Registry::default();
    assert_eq!(fresh.get(NAME), Some(builtin::komira()));
    assert!(fresh.get("other").is_none());
    assert!(fresh.is_builtin(NAME));
    assert!(!fresh.is_builtin("other"));
    let listed = fresh.listed();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].repository.name, NAME);
    let sent = fresh.entries_of([NAME]).expect("komira is held");
    assert_eq!(sent, [builtin::komira().repository.clone()]);
    assert_eq!(fresh.entries_of([NAME, "other"]), None);
    assert!(fresh.entries.is_empty());
}

/// Proves: an entry named `komira` that was put replaces the built-in one in `get`, `listed` and
/// `entries_of` (one `komira` listed, at the put's generation), and `is_builtin` is false.
///
/// Catches: a key rotation by `Put` that leaves the built-in key in force, or two `komira`
/// entries listed.
#[test]
fn a_stored_komira_overrides_the_builtin() {
    let put = Registry::default().put(komira(1)).expect("put");
    assert!(!put.is_builtin(NAME));
    assert_eq!(put.get(NAME).expect("held").repository, komira(1));
    assert_eq!(put.get(NAME).expect("held").generation, 1);
    let listed = put.listed();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].repository, komira(1));
    assert_eq!(put.entries_of([NAME]), Some(vec![komira(1)]));
}

/// Proves: `Remove` of the built-in repository is refused `builtin` with the registry as it was,
/// and `Remove` of an entry put over it is taken, restores the built-in entry (`builtin` true,
/// generation 0), and is taken though the services of the rollout file name it.
///
/// Catches: a built-in repository that an operator removes from under every package, or an
/// override that can never be undone.
#[test]
fn remove_of_the_builtin_is_refused_and_of_an_override_restores_it() {
    let rig = rig_on("builtin-remove", &Fake::new(), "");
    let remove = || {
        rig.coordinator.repositories_remove(
            &operator(),
            proto::RemoveRepositoryRequest {
                name: NAME.to_owned(),
                reason: "retired".to_owned(),
            },
        )
    };
    assert_eq!(remove().map(drop), Err(Refusal::Builtin));
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(komira(1)),
                reason: "rotate".to_owned(),
            },
        )
        .expect("put over the built-in");
    let removed = remove().expect("the override is removed");
    assert_eq!(removed.generation, 2);
    let got = rig
        .coordinator
        .repositories_get(
            &operator(),
            &proto::GetRepositoryRequest {
                name: NAME.to_owned(),
            },
        )
        .expect("got")
        .repository
        .expect("held");
    assert!(got.builtin);
    assert_eq!(got.generation, 0);
    assert_eq!(got.repository.expect("entry").fingerprints, PINNED);
    assert_eq!(remove().map(drop), Err(Refusal::Builtin));
}

/// Proves: the stored entry does not depend on the built-in one: the registry's file and rows
/// hold what was put only, so a fresh registry encodes as no repository.
///
/// Catches: the built-in entry written to the state, which an older coordinator would then hold
/// as the operator's.
#[test]
fn the_builtin_entry_is_in_no_file() {
    let text = crate::repositories::encode_file(&Registry::default()).expect("encoded");
    let value: serde_json::Value = serde_json::from_slice(&text).expect("JSON");
    assert_eq!(value["repositories"], serde_json::json!({}));
}
