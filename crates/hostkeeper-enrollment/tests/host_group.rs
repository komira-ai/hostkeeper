//! The host group of an approved host, as the host file keeps it (`store.proto`'s `Host` fields
//! 11 to 13: its group, the group it moves from, the generation of its facts) and as its approval
//! records it (`Approval.host_group`, field 9): an approval that names a group puts the host in it;
//! a renewal keeps the host's, and one, or a `replace`, naming another is refused
//! `host_group_differs`; an approval of a new key that names none starts the host anew without
//! them.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::EnrollmentStore;
use hostkeeper_enrollment::enroll::records::pb;
use hostkeeper_enrollment::fs::RealFs;
use prost::Message as _;
use support::{HostKey, NOW, TestCa, decision, renewal, request, scratch};

fn open(dir: &Path) -> EnrollmentStore {
    EnrollmentStore::open(Arc::new(RealFs), dir).expect("open")
}

fn host_file(dir: &Path) -> std::path::PathBuf {
    dir.join("hosts/web-1.pb")
}

fn host_of(dir: &Path) -> pb::Host {
    pb::Host::decode(
        std::fs::read(host_file(dir))
            .expect("the host file")
            .as_slice(),
    )
    .expect("decodes")
}

/// The approval the request `id` holds.
fn approval_of(dir: &Path, id: &[u8]) -> pb::Approval {
    let name = id.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let path = dir.join(format!("requests/{name}.pb"));
    pb::Request::decode(std::fs::read(path).expect("the request file").as_slice())
        .expect("decodes")
        .approved
        .expect("approved")
}

/// Proves: the host file of a host in the group `web`, moving from `db`, with facts of
/// generation 3 (as a move writes it), keeps all three through a renewal's approval, and the
/// renewal's approval records the group; an approval of a new key for the host id (`replace`)
/// writes the host without them, and its approval names no group; a host file of a host approved
/// before groups reads with none.
///
/// Catches: a renewal, which is no move, taking a host out of its group or ending its move (its
/// next heartbeat would then place it by nothing), or a new key inheriting a move under way.
#[test]
fn a_renewal_keeps_the_hosts_group_and_a_new_key_starts_without_it() {
    let ca = TestCa::new("host-group-ca");
    let dir = scratch("host-group-renewal");
    let store = open(dir.path());
    let key = HostKey::new();
    let id = store
        .post(request(&key, "m-1"), NOW)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &key.key_hash(), &decision("web-1", false))
        .expect("approved");
    let fresh = host_of(dir.path());
    assert_eq!(
        (
            fresh.host_group.as_str(),
            fresh.moving_from.as_deref(),
            fresh.facts_generation
        ),
        ("", None, None)
    );
    assert_eq!(approval_of(dir.path(), id.as_bytes()).host_group, "");
    store.issue(&ca.ca, &key.key_hash()).expect("issued");
    drop(store);

    let moved = pb::Host {
        host_group: "web".to_owned(),
        moving_from: Some("db".to_owned()),
        facts_generation: Some(3),
        ..fresh
    };
    std::fs::write(host_file(dir.path()), moved.encode_to_vec()).expect("written");
    let store = open(dir.path());
    let renewing = store
        .post(renewal(&key, "m-1", "web-1"), NOW + 100)
        .expect("a renewal");
    store
        .approve(
            &renewing.request_id,
            &key.key_hash(),
            &decision("web-1", false),
        )
        .expect("the renewal approved");
    let renewed = host_of(dir.path());
    assert_eq!(
        (
            renewed.host_group.as_str(),
            renewed.moving_from.as_deref(),
            renewed.facts_generation
        ),
        ("web", Some("db"), Some(3))
    );
    assert_eq!(
        approval_of(dir.path(), renewing.request_id.as_bytes()).host_group,
        "web"
    );

    let other = HostKey::new();
    let taken = store
        .post(request(&other, "m-9"), NOW + 200)
        .expect("posted")
        .request_id;
    store
        .approve(&taken, &other.key_hash(), &decision("web-1", true))
        .expect("approved over the host id");
    let anew = host_of(dir.path());
    assert_eq!(
        (
            anew.host_group.as_str(),
            anew.moving_from.as_deref(),
            anew.facts_generation
        ),
        ("", None, None)
    );
    assert_eq!(approval_of(dir.path(), taken.as_bytes()).host_group, "");
    let reopened = open(dir.path());
    assert!(
        reopened
            .enrolled_host(&support::host_id("web-1"))
            .expect("read")
            .is_some()
    );
}

/// `decision` of `host` (replacing or not) naming the host group `group`.
fn in_group(host: &str, replace: bool, group: &str) -> hostkeeper_enrollment::enroll::Decision {
    hostkeeper_enrollment::enroll::Decision {
        host_group: Some(group.to_owned()),
        ..decision(host, replace)
    }
}

/// The record of an approval by `who`.
fn by(who: &str) -> hostkeeper_enrollment::enroll::ApprovalRecord {
    hostkeeper_enrollment::enroll::ApprovalRecord {
        approver: who.to_owned(),
        approved_unix: NOW,
        reason: "racked".to_owned(),
        mac_nonce: None,
        mac_enrolled_since_unix: None,
    }
}

/// Proves: an approval that names the host group `web` writes it to the host file and to the
/// approval, answers it, and lists it with the recorded approval (what the coordinator audits).
///
/// Catches: a group named at approval dropped on the way to the store, which would leave the
/// host in no group and ordered nothing.
#[test]
fn an_approval_naming_a_group_puts_the_host_in_it() {
    let dir = scratch("host-group-named");
    let store = open(dir.path());
    let key = HostKey::new();
    let id = store
        .post(request(&key, "m-1"), NOW)
        .expect("posted")
        .request_id;
    let approval = store
        .approve_recorded(
            &id,
            &key.key_hash(),
            &in_group("web-1", false, "web"),
            &by("alice"),
        )
        .expect("approved");
    assert_eq!(approval.host_group, "web");
    assert_eq!(host_of(dir.path()).host_group, "web");
    assert_eq!(approval_of(dir.path(), id.as_bytes()).host_group, "web");
    let recorded = store.recorded_approvals().expect("read");
    assert_eq!(
        recorded
            .iter()
            .map(|a| (a.host_id.as_str(), a.host_group.as_str()))
            .collect::<Vec<_>>(),
        [("web-1", "web")]
    );
}

/// Proves: a host in the group `web` is renewed, and its id replaced by a new key, when the
/// approval names `web`; naming `db` instead is refused `host_group_differs` (the error names
/// both groups), and the host file and the request are as they were.
///
/// Catches: a renewal or a re-approval moving a host to another group, past `HostGroups.Move`
/// and its order of uninstalls and installs.
#[test]
fn a_renewal_or_a_replace_naming_another_group_is_refused() {
    let ca = TestCa::new("host-group-differs-ca");
    let dir = scratch("host-group-differs");
    let store = open(dir.path());
    let key = HostKey::new();
    let id = store
        .post(request(&key, "m-1"), NOW)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &key.key_hash(), &in_group("web-1", false, "web"))
        .expect("approved");
    store.issue(&ca.ca, &key.key_hash()).expect("issued");
    let before = host_of(dir.path());

    let renewing = store
        .post(renewal(&key, "m-1", "web-1"), NOW + 100)
        .expect("a renewal")
        .request_id;
    let refused = store
        .approve(&renewing, &key.key_hash(), &in_group("web-1", false, "db"))
        .expect_err("another group");
    assert_eq!(refused.token(), Some("host_group_differs"));
    assert_eq!(
        refused.to_string(),
        "host_group_differs: the host is in host group web, not db"
    );
    assert_eq!(host_of(dir.path()), before);
    assert!(
        store
            .request(&renewing)
            .expect("read")
            .expect("held")
            .state
            .is_pending()
    );

    let other = HostKey::new();
    let taking = store
        .post(request(&other, "m-9"), NOW + 200)
        .expect("posted")
        .request_id;
    let refused = store
        .approve(&taking, &other.key_hash(), &in_group("web-1", true, "db"))
        .expect_err("another group");
    assert_eq!(refused.token(), Some("host_group_differs"));
    assert_eq!(host_of(dir.path()), before);

    store
        .approve(&renewing, &key.key_hash(), &in_group("web-1", false, "web"))
        .expect("the renewal in its own group");
    assert_eq!(host_of(dir.path()).host_group, "web");
    store
        .approve(&taking, &other.key_hash(), &in_group("web-1", true, "web"))
        .expect("the replace in its own group");
    let taken = host_of(dir.path());
    assert_eq!(
        (taken.host_group.as_str(), taken.key_hash),
        ("web", other.key_hash().as_bytes().to_vec())
    );
}

/// Proves: an approval that replaces a host in no group (approved before the coordinator held
/// one) puts the new key's host in the group it names, and so does one that replaces a denied
/// host, whose group ended when it was retired.
///
/// Catches: a host approved before groups, or a retired host's id, held to a group it is not in.
#[test]
fn a_replace_of_a_host_in_no_group_or_of_a_denied_one_takes_the_named_group() {
    let dir = scratch("host-group-replace");
    let store = open(dir.path());
    let first = HostKey::new();
    let id = store
        .post(request(&first, "m-1"), NOW)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &first.key_hash(), &decision("web-1", false))
        .expect("approved in no group");

    let second = HostKey::new();
    let id = store
        .post(request(&second, "m-2"), NOW + 100)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &second.key_hash(), &in_group("web-1", true, "web"))
        .expect("replaced into web");
    assert_eq!(host_of(dir.path()).host_group, "web");

    assert!(store.deny(&support::host_id("web-1")).expect("denied"));
    let third = HostKey::new();
    let id = store
        .post(request(&third, "m-3"), NOW + 200)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &third.key_hash(), &in_group("web-1", true, "db"))
        .expect("the denied id replaced into db");
    assert_eq!(host_of(dir.path()).host_group, "db");
}
