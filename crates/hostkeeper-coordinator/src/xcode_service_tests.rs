//! `Xcode.List` and `Xcode.Remove` as handlers over the coordinator, the pruning of the Xcode
//! store to what its groups keep, and the helpers that read the groups' `xcode_keep`.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::XcodeRefusal as Refusal;

use crate::audit_actions::XCODE_REMOVE;
use crate::host_groups_support::{SELF, World};
use crate::marks_support::{operator, start};
use crate::operator::Caller;
use crate::ops::AuditEntry;
use crate::service::Coordinator;
use crate::store::{audit_failure, store_failure};
use crate::testutil::{Rig, id, scratch};
use crate::xcode_feed::{Channel, XcodeId, XcodeRelease, XcodeVersion};
use crate::xcode_store::{Archive, Digesting, Taken, XcodeStore, kept};

/// The time the archives of the tests were stored, in unix seconds.
const AT: u64 = 1_800_000_000;

/// The reason of the operator's removals.
const WHY: &str = "superseded by a newer build";

/// The Xcode `version` of build `build`.
fn xcode_id(version: &str, build: &str) -> XcodeId {
    XcodeId {
        version: XcodeVersion::parse(version).expect("a version number"),
        build: build.to_owned(),
    }
}

/// A release of the list: `version`, `build`, in the channel `channel`, which requires macOS 26.6.
fn release(version: &str, build: &str, channel: Channel) -> XcodeRelease {
    XcodeRelease {
        id: xcode_id(version, build),
        channel,
        requires_macos: "26.6".to_owned(),
    }
}

/// Stores `bytes` in `store` as the archive of `version` and `build`, uploaded by `uploader`: its
/// SHA-256 is the bytes' own.
fn stored(store: &XcodeStore, version: &str, build: &str, bytes: &[u8]) -> Archive {
    let mut digest = Digesting::default();
    digest.feed(bytes);
    let sha256 = digest.finish();
    let (path, mut file) = store.upload_file(&sha256).expect("an upload file");
    file.write_all(bytes).expect("written");
    drop(file);
    let archive = Archive {
        sha256,
        id: xcode_id(version, build),
        size: u64::try_from(bytes.len()).expect("a size"),
        by: "uploader".to_owned(),
        at_unix: AT,
    };
    let _changing = store.changing();
    store.commit(&path, &archive).expect("committed");
    archive
}

/// A coordinator over a world of groups (an enrollment, an in-memory store) that keeps Xcode
/// archives in a directory of its own.
struct Farm {
    world: World,
    store: Arc<XcodeStore>,
    dir: PathBuf,
}

impl Farm {
    /// A farm named `name`, whose store takes archives of at most 1 MiB.
    fn new(name: &str) -> Self {
        let mut world = World::new(name);
        let dir = world.rig.dir.path().join("xcode");
        fs::create_dir(&dir).expect("the store's directory");
        let store =
            Arc::new(XcodeStore::open(&dir, 1 << 20, std::time::UNIX_EPOCH).expect("a store"));
        let spare = scratch(&format!("{name}-spare"));
        let placeholder = start(&spare, &world.rig.clock, "");
        let built = std::mem::replace(&mut world.rig.coordinator, placeholder);
        world.rig.coordinator = built.with_xcode_store(Arc::clone(&store));
        Self { world, store, dir }
    }

    fn coordinator(&self) -> &Coordinator {
        self.world.coordinator()
    }

    /// Stores an archive ([`stored`]) whose bytes name the build.
    fn add(&self, version: &str, build: &str) -> Archive {
        stored(
            &self.store,
            version,
            build,
            format!("xip of {build}").as_bytes(),
        )
    }

    /// Puts the group `name` keeping `keep` Xcode releases.
    fn group_keeping(&self, name: &str, keep: u32) {
        let policy = proto::RolloutPolicy {
            maintenance: Some(proto::MaintenancePolicy {
                xcode_keep: keep,
                ..proto::MaintenancePolicy::default()
            }),
            ..proto::RolloutPolicy::default()
        };
        self.world
            .put_with(name, &[SELF], Some(policy), None)
            .expect("a group put");
    }

    /// The `xcode_remove` entries of the audit ring, oldest first.
    fn removals(&self) -> Vec<AuditEntry> {
        self.world
            .audit()
            .into_iter()
            .filter(|entry| entry.action == XCODE_REMOVE)
            .collect()
    }

    /// The sha256 of each archive the store holds, newest release first.
    fn held(&self) -> Vec<String> {
        self.store
            .archives()
            .expect("listed")
            .into_iter()
            .map(|archive| archive.sha256)
            .collect()
    }

    /// Whether neither file of `archive` is left.
    fn files_gone(&self, archive: &Archive) -> bool {
        !self.store.archive_path(&archive.sha256).exists()
            && !self.dir.join(format!("{}.json", archive.sha256)).exists()
    }
}

/// `Xcode.Remove` of `sha256` for `reason`.
fn remove_of(sha256: &str, reason: &str) -> proto::RemoveXcodeRequest {
    proto::RemoveXcodeRequest {
        sha256: sha256.to_owned(),
        reason: reason.to_owned(),
    }
}

/// The entry `archive`'s removal by `by` for `reason` is audited as, by its number `seq`.
fn removal(seq: u64, by: &str, archive: &Archive, reason: &str) -> AuditEntry {
    AuditEntry {
        seq,
        by: by.to_owned(),
        action: XCODE_REMOVE.to_owned(),
        service: None,
        node_id: None,
        host_group: None,
        reason: Some(reason.to_owned()),
        subject: Some(format!(
            "Xcode {} ({}) {}",
            archive.id.version, archive.id.build, archive.sha256
        )),
        source: None,
        api_token: None,
    }
}

/// Proves: a coordinator with no Xcode store answers `List` with `configured` false, no
/// archive, no keep and no size limit, and still names the release the list holds that the
/// (empty) store lacks, the newest release and not a beta.
///
/// Catches: a `List` that fails without a store, or one that loses the waiting release.
#[test]
fn list_without_a_store_says_it_is_not_configured_and_still_names_the_waiting_release() {
    let rig = Rig::new("xcode-list-no-store");
    rig.coordinator
        .xcode
        .known
        .keep(&[
            release("27.3", "27C30", Channel::Release),
            release("27.4", "27D40", Channel::Beta),
        ])
        .expect("kept");
    assert_eq!(
        rig.coordinator
            .xcode_list(&operator(), &proto::ListXcodeRequest {}),
        Ok(proto::ListXcodeResponse {
            configured: false,
            archives: vec![],
            waiting: Some(proto::XcodeReleaseWaiting {
                version: "27.3".to_owned(),
                build: "27C30".to_owned(),
                requires_macos: "26.6".to_owned(),
            }),
            keep: 0,
            max_bytes: 0,
            uploads: Vec::new(),
        })
    );
}

/// Proves: with a store, `List` answers the archives newest release first with their build, size,
/// uploader and time, `configured` true, the store's size limit, the release the list holds
/// that the store lacks, and a `keep` of 1 while no group keeps Xcode and the largest
/// `xcode_keep` once one does.
///
/// Catches: archives in the order they were stored, a size or uploader left out, a keep of 0
/// (the store keeps at least the release just pushed), or a keep that is not the largest of the
/// groups.
#[test]
fn list_answers_the_archives_newest_first_with_the_waiting_release_and_the_keep() {
    let farm = Farm::new("xcode-list-archives");
    let older = farm.add("27.1", "27A10");
    let newer = farm.add("27.2", "27B20");
    farm.coordinator()
        .xcode
        .known
        .keep(&[release("27.3", "27C30", Channel::Release)])
        .expect("kept");
    let archive = |stored: &Archive, version: &str, build: &str| proto::XcodeArchive {
        sha256: stored.sha256.clone(),
        version: version.to_owned(),
        build: build.to_owned(),
        size_bytes: stored.size,
        uploaded_by: "uploader".to_owned(),
        uploaded_at_unix: AT,
    };
    let expected = |keep: u32| proto::ListXcodeResponse {
        configured: true,
        archives: vec![
            archive(&newer, "27.2", "27B20"),
            archive(&older, "27.1", "27A10"),
        ],
        waiting: Some(proto::XcodeReleaseWaiting {
            version: "27.3".to_owned(),
            build: "27C30".to_owned(),
            requires_macos: "26.6".to_owned(),
        }),
        keep,
        max_bytes: 1 << 20,
        uploads: Vec::new(),
    };
    assert_eq!(
        farm.coordinator()
            .xcode_list(&operator(), &proto::ListXcodeRequest {}),
        Ok(expected(1))
    );
    farm.group_keeping("alpha", 3);
    farm.group_keeping("beta", 2);
    assert_eq!(
        farm.coordinator()
            .xcode_list(&operator(), &proto::ListXcodeRequest {}),
        Ok(expected(3))
    );
}

/// Proves: a store whose directory can no longer be listed makes `List` `store_failed`.
///
/// Catches: a failed listing told as an empty store.
#[test]
fn list_of_a_store_that_cannot_be_listed_is_store_failed() {
    let farm = Farm::new("xcode-list-unreadable");
    fs::remove_dir_all(&farm.dir).expect("the directory removed");
    assert_eq!(
        farm.coordinator()
            .xcode_list(&operator(), &proto::ListXcodeRequest {}),
        Err(Refusal::StoreFailed)
    );
}

/// Proves: a CI token is `forbidden` and no token `unauthorized` at both calls, before the
/// request is read: nothing is removed and no entry is written.
///
/// Catches: a call that reaches the store for a caller that is no operator.
#[test]
fn only_an_operator_may_list_or_remove() {
    let farm = Farm::new("xcode-callers");
    let archive = farm.add("27.1", "27A10");
    let audited = farm.world.audit().len();
    for (caller, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ] {
        assert_eq!(
            farm.coordinator()
                .xcode_list(&caller, &proto::ListXcodeRequest {}),
            Err(refusal)
        );
        assert_eq!(
            farm.coordinator()
                .xcode_remove(&caller, remove_of(&archive.sha256, WHY)),
            Err(refusal)
        );
    }
    assert_eq!(farm.held(), vec![archive.sha256]);
    assert_eq!(farm.world.audit().len(), audited);
}

/// Proves: `Remove` deletes the archive's file and record, leaves the other archive alone,
/// appends exactly one `xcode_remove` entry by the operator (the operator's reason, and the
/// release and archive as its subject), and answers that entry.
///
/// Catches: a removal that leaves the record, removes the wrong archive, audits twice or under
/// another name, or answers an entry that is not the one written.
#[test]
fn remove_deletes_the_archive_and_audits_it_once_under_the_operator() {
    let farm = Farm::new("xcode-remove");
    let doomed = farm.add("27.1", "27A10");
    let other = farm.add("27.2", "27B20");
    let answer = farm
        .coordinator()
        .xcode_remove(&operator(), remove_of(&doomed.sha256, WHY))
        .expect("removed");
    assert!(farm.files_gone(&doomed));
    assert_eq!(farm.held(), vec![other.sha256.clone()]);
    assert!(farm.store.archive_path(&other.sha256).exists());
    let entries = farm.removals();
    assert_eq!(entries.len(), 1);
    let seq = entries[0].seq;
    assert_eq!(entries, vec![removal(seq, "operator", &doomed, WHY)]);
    assert_eq!(
        answer,
        proto::RemoveXcodeResponse {
            entry: Some(proto::AuditEntry {
                seq,
                by: proto::Actor::Operator.into(),
                action: "xcode_remove".to_owned(),
                reason: Some(WHY.to_owned()),
                by_name: Some("operator".to_owned()),
                ..proto::AuditEntry::default()
            }),
        }
    );
}

/// Proves: removing the archive of a version whose second build was refused clears the standing
/// refusal: the alert `xcode_version_taken` for that version is raised until the removal and
/// gone after it, while the refusal of another version stands.
///
/// Catches: an alert that outlives the archive it names.
#[test]
fn remove_clears_the_refusal_of_a_second_build_of_that_version() {
    let farm = Farm::new("xcode-remove-taken");
    let held = farm.add("27.1", "27A10");
    farm.add("27.2", "27B20");
    farm.store.note_taken("27.1", "27A10", "27A11");
    farm.store.note_taken("27.2", "27B20", "27B21");
    let before = farm.coordinator().xcode_taken();
    assert_eq!(
        before,
        vec![
            Taken {
                version: "27.1".to_owned(),
                held_build: "27A10".to_owned(),
                refused_build: "27A11".to_owned(),
            },
            Taken {
                version: "27.2".to_owned(),
                held_build: "27B20".to_owned(),
                refused_build: "27B21".to_owned(),
            },
        ]
    );
    assert_eq!(super::version_taken(&before).len(), 2);
    farm.coordinator()
        .xcode_remove(&operator(), remove_of(&held.sha256, WHY))
        .expect("removed");
    let after = farm.coordinator().xcode_taken();
    assert_eq!(after, before[1..].to_vec());
    let alerts = super::version_taken(&after);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].subject.version, "27.2");
}

/// Proves: a sha256 the store does not hold is `unknown_archive`, and removes nothing and writes
/// no entry.
///
/// Catches: a removal of an unknown archive that is audited as made.
#[test]
fn remove_of_an_unknown_archive_is_refused_and_writes_no_entry() {
    let farm = Farm::new("xcode-remove-unknown");
    let held = farm.add("27.1", "27A10");
    let unknown = "ab".repeat(32);
    assert_eq!(
        farm.coordinator()
            .xcode_remove(&operator(), remove_of(&unknown, WHY)),
        Err(Refusal::UnknownArchive)
    );
    assert_eq!(farm.held(), vec![held.sha256]);
    assert_eq!(farm.removals(), vec![]);
}

/// Proves: a sha256 that is not 64 lowercase hex characters (short, upper case, a path, empty), and
/// a reason that is empty, blank, over 256 bytes or holds a control character, are
/// `invalid_request`, and nothing is removed and no entry written.
///
/// Catches: a name that reaches the store's paths unchecked (`../`), or a removal with no
/// reason.
#[test]
fn remove_with_a_bad_sha256_or_reason_is_invalid_and_changes_nothing() {
    let farm = Farm::new("xcode-remove-invalid");
    let held = farm.add("27.1", "27A10");
    let sha = held.sha256.clone();
    let long = "x".repeat(257);
    for (sha256, reason) in [
        (sha[..63].to_owned(), WHY),
        (sha.to_ascii_uppercase(), WHY),
        (format!("../{sha}"), WHY),
        (String::new(), WHY),
        (sha.clone(), ""),
        (sha.clone(), "   "),
        (sha.clone(), long.as_str()),
        (sha.clone(), "line\nbreak"),
    ] {
        assert_eq!(
            farm.coordinator()
                .xcode_remove(&operator(), remove_of(&sha256, reason)),
            Err(Refusal::InvalidRequest),
            "{sha256:?} {reason:?}"
        );
    }
    assert_eq!(farm.held(), vec![sha]);
    assert_eq!(farm.removals(), vec![]);
}

/// Proves: without a store, `Remove` of a well-formed request is `no_store`.
///
/// Catches: a removal that finds no archive and says `unknown_archive` for a coordinator that has
/// no store at all.
#[test]
fn remove_without_a_store_is_no_store() {
    let rig = Rig::new("xcode-remove-no-store");
    assert_eq!(
        rig.coordinator
            .xcode_remove(&operator(), remove_of(&"ab".repeat(32), WHY)),
        Err(Refusal::NoStore)
    );
}

/// Proves: a removal whose audit line cannot be kept is `audit_unavailable`, and one whose
/// commit fails `store_failed`, and either removes no file; once the store takes commits again
/// the same removal is made.
///
/// Catches: files removed before their audit line is written.
#[test]
fn a_removal_that_cannot_be_audited_removes_nothing() {
    let farm = Farm::new("xcode-remove-audit");
    let archive = farm.add("27.1", "27A10");
    for (audit, refusal) in [
        (true, Refusal::AuditUnavailable),
        (false, Refusal::StoreFailed),
    ] {
        farm.world.fake.refuse(move |_| {
            Some(if audit {
                audit_failure()
            } else {
                store_failure()
            })
        });
        assert_eq!(
            farm.coordinator()
                .xcode_remove(&operator(), remove_of(&archive.sha256, WHY)),
            Err(refusal)
        );
        assert_eq!(farm.held(), vec![archive.sha256.clone()]);
        assert_eq!(farm.removals(), vec![]);
    }
    farm.world.fake.heal();
    farm.coordinator()
        .xcode_remove(&operator(), remove_of(&archive.sha256, WHY))
        .expect("removed");
    assert!(farm.files_gone(&archive));
}

/// Proves: a record the store cannot read is `store_failed` and writes no entry; a file that
/// cannot be removed is `store_failed` after the entry was written (the audit comes first), and
/// the record stays.
///
/// Catches: an unreadable record told as an unknown archive, or a failed removal told as done.
#[test]
fn a_store_that_cannot_be_read_or_removed_from_is_store_failed() {
    let farm = Farm::new("xcode-remove-io");
    let unreadable = farm.add("27.1", "27A10");
    let stuck = farm.add("27.2", "27B20");
    fs::write(
        farm.dir.join(format!("{}.json", unreadable.sha256)),
        "not a record",
    )
    .expect("the record spoilt");
    assert_eq!(
        farm.coordinator()
            .xcode_remove(&operator(), remove_of(&unreadable.sha256, WHY)),
        Err(Refusal::StoreFailed)
    );
    assert_eq!(farm.removals(), vec![]);

    let file = farm.store.archive_path(&stuck.sha256);
    fs::remove_file(&file).expect("the archive removed");
    fs::create_dir(&file).expect("a directory in its place");
    assert_eq!(
        farm.coordinator()
            .xcode_remove(&operator(), remove_of(&stuck.sha256, WHY)),
        Err(Refusal::StoreFailed)
    );
    assert_eq!(farm.removals().len(), 1);
    assert!(farm.dir.join(format!("{}.json", stuck.sha256)).exists());
}

/// Proves: with groups that keep 2 and 1 Xcode releases and four archives, pruning keeps exactly
/// the newest two, removes the other two (files and records), and appends one `xcode_remove`
/// entry by `coordinator` for each, naming the release and archive and saying it was pruned;
/// a second prune changes nothing and writes nothing.
///
/// Catches: a prune that keeps the wrong releases (the oldest, or one per group), a removal
/// with no audit line or one under an operator's name, or a prune that is not idempotent.
#[test]
fn prune_keeps_the_newest_releases_the_largest_group_keeps_and_audits_each_removal() {
    let farm = Farm::new("xcode-prune");
    farm.group_keeping("alpha", 2);
    farm.group_keeping("beta", 1);
    let oldest = farm.add("27.1", "27A10");
    let second = farm.add("27.2", "27B20");
    let third = farm.add("27.3", "27C30");
    let newest = farm.add("27.4", "27D40");
    farm.coordinator().prune_xcode_store();
    assert_eq!(
        farm.held(),
        vec![newest.sha256.clone(), third.sha256.clone()]
    );
    assert!(farm.files_gone(&second) && farm.files_gone(&oldest));
    let entries = farm.removals();
    let first_seq = entries[0].seq;
    let reason = |archive: &Archive| {
        format!(
            "pruned: Xcode {} ({}), sha256 {}",
            archive.id.version, archive.id.build, archive.sha256
        )
    };
    assert_eq!(
        entries,
        vec![
            removal(first_seq, "coordinator", &second, &reason(&second)),
            removal(entries[1].seq, "coordinator", &oldest, &reason(&oldest)),
        ]
    );
    let audited = farm.world.audit();
    farm.coordinator().prune_xcode_store();
    assert_eq!(farm.world.audit(), audited);
    assert_eq!(farm.held(), vec![newest.sha256, third.sha256]);
}

/// Proves: when no group keeps Xcode, pruning still keeps the newest release (the one just
/// pushed) and removes the others.
///
/// Catches: a prune that empties the store when `xcode_keep` is 0 everywhere.
#[test]
fn prune_keeps_the_newest_release_when_no_group_keeps_xcode() {
    let farm = Farm::new("xcode-prune-none");
    farm.group_keeping("alpha", 0);
    let older = farm.add("27.1", "27A10");
    let newest = farm.add("27.2", "27B20");
    assert_eq!(farm.coordinator().xcode_keep_largest(), 0);
    farm.coordinator().prune_xcode_store();
    assert_eq!(farm.held(), vec![newest.sha256]);
    assert!(farm.files_gone(&older));
    assert_eq!(farm.removals().len(), 1);
}

/// Proves: a coordinator with no store prunes nothing and writes no entry.
///
/// Catches: a prune that fails or audits without a store.
#[test]
fn prune_without_a_store_does_nothing() {
    let rig = Rig::new("xcode-prune-no-store");
    let audited = rig.coordinator.ops().state().audit;
    rig.coordinator.prune_xcode_store();
    assert_eq!(rig.coordinator.ops().state().audit, audited);
}

/// Proves: an archive whose removal cannot be audited stays, with no file removed and no entry
/// written, and is pruned by the next run once the store takes commits; an archive whose file
/// cannot be removed is a log line and the prune goes on to the archives after it.
///
/// Catches: files removed before their audit line, a prune that stops at the first failure, or one
/// that panics.
#[test]
fn a_prune_that_cannot_audit_or_remove_leaves_the_archive_and_goes_on() {
    let farm = Farm::new("xcode-prune-failures");
    let oldest = farm.add("27.1", "27A10");
    let stuck = farm.add("27.2", "27B20");
    let newest = farm.add("27.3", "27C30");
    farm.world.fake.refuse(|_| Some(audit_failure()));
    farm.coordinator().prune_xcode_store();
    assert_eq!(farm.removals(), vec![]);
    assert_eq!(farm.held().len(), 3);
    farm.world.fake.heal();

    let file = farm.store.archive_path(&stuck.sha256);
    fs::remove_file(&file).expect("the archive removed");
    fs::create_dir(&file).expect("a directory in its place");
    farm.coordinator().prune_xcode_store();
    assert!(farm.files_gone(&oldest));
    assert!(farm.dir.join(format!("{}.json", stuck.sha256)).exists());
    assert_eq!(farm.removals().len(), 2);
    assert!(farm.store.archive_path(&newest.sha256).exists());
}

/// Proves: a store whose directory cannot be listed is pruned as nothing: no entry, no panic.
///
/// Catches: a prune that treats an unreadable store as one with nothing to keep.
#[test]
fn prune_of_a_store_that_cannot_be_listed_does_nothing() {
    let farm = Farm::new("xcode-prune-unreadable");
    farm.add("27.1", "27A10");
    fs::remove_dir_all(&farm.dir).expect("the directory removed");
    farm.coordinator().prune_xcode_store();
    assert_eq!(farm.removals(), vec![]);
}

/// Proves: a release a Mac reports installed stays however old: `kept` over four archives with
/// `keep` 1 and the oldest installed keeps the newest and the oldest, and a `keep` of 0 is read as
/// 1; a coordinator no host has reported to holds no installed release.
///
/// Catches: an installed Xcode pruned from under a Mac, or a store emptied by a keep of 0.
#[test]
fn kept_holds_each_installed_release_and_at_least_the_newest() {
    let at = |version: &str, build: &str| Archive {
        sha256: format!("{:0>64}", build.to_ascii_lowercase()),
        id: xcode_id(version, build),
        size: 1,
        by: "uploader".to_owned(),
        at_unix: AT,
    };
    let archives = [
        at("27.1", "a1"),
        at("27.4", "a4"),
        at("27.2", "a2"),
        at("27.3", "a3"),
    ];
    let installed = BTreeSet::from([xcode_id("27.1", "a1")]);
    let (stays, goes) = kept(&archives, 1, &installed);
    assert_eq!(stays, vec![at("27.4", "a4"), at("27.1", "a1")]);
    assert_eq!(goes, vec![at("27.3", "a3"), at("27.2", "a2")]);
    let (stays, goes) = kept(&archives, 0, &BTreeSet::new());
    assert_eq!(stays, vec![at("27.4", "a4")]);
    assert_eq!(goes.len(), 3);
    let rig = Rig::new("xcode-installed");
    assert_eq!(rig.coordinator.xcodes_installed(), BTreeSet::new());
}

/// Proves: `xcode_keep_largest` is the largest `xcode_keep` of the groups (0 with none), and
/// `xcode_keep_of` the `xcode_keep` of the host's own group: 0 for a host in a group that keeps
/// none, for one in no group and for one that never enrolled, and for a coordinator with no
/// enrollment.
///
/// Catches: a Mac read the largest keep of another group, or a host in no group allowed to read
/// the store.
#[test]
fn the_keep_is_the_largest_of_the_groups_and_a_hosts_is_its_groups() {
    let farm = Farm::new("xcode-keeps");
    assert_eq!(farm.coordinator().xcode_keep_largest(), 0);
    farm.group_keeping("alpha", 2);
    farm.group_keeping("beta", 1);
    farm.group_keeping("gamma", 0);
    for (host, group) in [
        ("d1", Some("alpha")),
        ("d2", Some("beta")),
        ("d3", Some("gamma")),
        ("d4", None),
    ] {
        farm.world.enroll(host);
        if let Some(group) = group {
            farm.world.move_to(host, group).expect("moved");
        }
    }
    assert_eq!(farm.coordinator().xcode_keep_largest(), 2);
    for (host, keep) in [("d1", 2), ("d2", 1), ("d3", 0), ("d4", 0), ("never", 0)] {
        assert_eq!(farm.coordinator().xcode_keep_of(&id(host)), keep, "{host}");
    }
    let rig = Rig::new("xcode-keeps-no-enrollment");
    assert_eq!(rig.coordinator.xcode_keep_of(&id("d1")), 0);
    assert_eq!(rig.coordinator.xcode_keep_largest(), 0);
}

/// Proves: a coordinator with no store has no refusal of a second build standing.
///
/// Catches: an alert raised on a coordinator that cannot refuse an upload.
#[test]
fn a_coordinator_without_a_store_has_no_taken_version() {
    let rig = Rig::new("xcode-no-taken");
    assert_eq!(rig.coordinator.xcode_taken(), vec![]);
}
