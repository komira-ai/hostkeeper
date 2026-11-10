//! The host group of an approved host, as the host file keeps it (`store.proto`'s `Host` fields
//! 11 to 13: its group, the group it moves from, the generation of its facts) and as its approval
//! records it (`Approval.host_group`, field 9): a renewal keeps them, and an approval of a new key
//! for the host id starts the host anew without them.

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
