//! What a host's daemon said it runs ([`EnrollmentStore::note_running`]): written into the host
//! file beside the first heartbeat when the daemon or a service changes and not when only the
//! time does, kept by a reopened store, cleared by every approval of the host id, never written
//! for a host id the store never approved; and the host file's compatibility with the release
//! before, in both directions.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::pb;
use hostkeeper_enrollment::enroll::{
    EnrolledHost, EnrollmentStore, HostRunning, RunningService, StoreError,
};
use hostkeeper_enrollment::fs::{FaultFs, RealFs};
use prost::Message as _;
use support::{HostKey, NOW, TestCa, decision, host_id, renewal, request, scratch};

fn open(dir: &Path) -> EnrollmentStore {
    EnrollmentStore::open(Arc::new(RealFs), dir).expect("open")
}

/// Posts and approves `key`'s request from `machine` as `host`, taking the id over when
/// `replace`.
fn enroll(store: &EnrollmentStore, key: &HostKey, machine: &str, host: &str, replace: bool) {
    let id = store
        .post(request(key, machine), NOW)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &key.key_hash(), &decision(host, replace))
        .expect("approved");
}

fn service(name: &str, version: &str, installer: &str) -> RunningService {
    RunningService {
        service: name.to_owned(),
        version: version.to_owned(),
        installer: installer.to_owned(),
    }
}

/// A daemon `0.2.0` running `web` at `version` with `installer`.
fn runs(version: &str, installer: &str) -> HostRunning {
    HostRunning {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "0.2.0".to_owned(),
        services: vec![service("web", version, installer)],
    }
}

/// What `store` answers for `host`: what it runs and when it was noted.
fn kept(store: &EnrollmentStore, host: &str) -> (Option<HostRunning>, Option<u64>) {
    let enrolled = store
        .enrolled_host(&host_id(host))
        .expect("read")
        .expect("enrolled");
    (enrolled.running, enrolled.running_unix)
}

/// Catches: a record that is not written, one written again for the time alone (which would
/// write the host file at every heartbeat), one that keeps the time of the latest heartbeat
/// instead of the one that changed it, a change of version, installer, service set or daemon
/// that is not written, one noted for a host given to another, one kept in memory only (lost
/// when the store is opened again), and the record written over the first heartbeat.
#[test]
fn what_a_host_runs_is_written_when_it_changes_and_not_for_the_time_alone() {
    let dir = scratch("running-changes");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    enroll(&store, &HostKey::new(), "m-2", "web-2", false);
    let web = host_id("web-1");
    store.note_heartbeat(&web, NOW + 10).expect("noted");
    assert_eq!(kept(&store, "web-1"), (None, None), "nothing said yet");

    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 60)
        .expect("noted");
    assert_eq!(
        kept(&store, "web-1"),
        (Some(runs("1.0", "deb")), Some(NOW + 60))
    );
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 75)
        .expect("noted");
    assert_eq!(kept(&store, "web-1").1, Some(NOW + 60), "the time alone");

    store
        .note_running(&web, &runs("1.1", "deb"), NOW + 90)
        .expect("noted");
    assert_eq!(kept(&store, "web-1").0, Some(runs("1.1", "deb")));
    store
        .note_running(&web, &runs("1.1", "hostkeeper-archive"), NOW + 105)
        .expect("noted");
    assert_eq!(
        kept(&store, "web-1"),
        (Some(runs("1.1", "hostkeeper-archive")), Some(NOW + 105))
    );
    let mut more = runs("1.1", "hostkeeper-archive");
    more.services.push(service("db", "9.1", "deb"));
    store.note_running(&web, &more, NOW + 120).expect("noted");
    assert_eq!(kept(&store, "web-1"), (Some(more.clone()), Some(NOW + 120)));
    let mut newer = more.clone();
    newer.agent_version = "0.3.0".to_owned();
    store.note_running(&web, &newer, NOW + 135).expect("noted");
    assert_eq!(
        kept(&store, "web-1"),
        (Some(newer.clone()), Some(NOW + 135))
    );

    assert_eq!(kept(&store, "web-2"), (None, None), "another host");
    let reopened = open(dir.path());
    assert_eq!(kept(&reopened, "web-1"), (Some(newer), Some(NOW + 135)));
    let enrolled = reopened.enrolled_host(&web).expect("read").expect("host");
    assert_eq!(enrolled.first_heartbeat_unix, Some(NOW + 10));
}

/// Catches: a service whose installer a heartbeat could not tell (empty) written over the
/// installer the record held for it, a held installer given to another service, and an
/// installer that was never known invented.
#[test]
fn a_service_whose_installer_is_not_known_keeps_the_one_the_record_holds() {
    let dir = scratch("running-unknown-installer");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    let two = |web_installer: &str, db_installer: &str| HostRunning {
        services: vec![
            service("db", "9.1", db_installer),
            service("web", "1.0", web_installer),
        ],
        ..runs("1.0", "")
    };
    store
        .note_running(&web, &two("deb", ""), NOW + 60)
        .expect("noted");
    assert_eq!(kept(&store, "web-1").0, Some(two("deb", "")));
    store
        .note_running(&web, &two("", "rpm"), NOW + 75)
        .expect("noted");
    assert_eq!(
        kept(&store, "web-1"),
        (Some(two("deb", "rpm")), Some(NOW + 75)),
        "web keeps deb, db learns rpm"
    );
    store
        .note_running(&web, &two("", ""), NOW + 90)
        .expect("noted");
    assert_eq!(
        kept(&store, "web-1"),
        (Some(two("deb", "rpm")), Some(NOW + 75)),
        "nothing changed"
    );
}

/// Catches: a re-enrollment, by a renewal of the same host or by a new key that takes the id
/// over, keeping what the host ran before the approval, in the file or in memory.
#[test]
fn every_approval_of_the_host_clears_what_it_runs() {
    let ca = TestCa::new("running-again-ca");
    let dir = scratch("running-again");
    let store = open(dir.path());
    let key = HostKey::new();
    enroll(&store, &key, "m-1", "web-1", false);
    let web = host_id("web-1");
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 60)
        .expect("noted");

    store.issue(&ca.ca, &key.key_hash()).expect("issued");
    let renewing = store
        .post(renewal(&key, "m-1", "web-1"), NOW + 100)
        .expect("a renewal");
    assert_eq!(kept(&store, "web-1").1, Some(NOW + 60), "a repost");
    store
        .approve(
            &renewing.request_id,
            &key.key_hash(),
            &decision("web-1", false),
        )
        .expect("the renewal approved");
    assert_eq!(kept(&store, "web-1"), (None, None), "after a renewal");
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 200)
        .expect("noted");
    assert_eq!(kept(&store, "web-1").1, Some(NOW + 200), "written again");

    enroll(&store, &HostKey::new(), "m-9", "web-1", true);
    assert_eq!(kept(&store, "web-1"), (None, None), "after a new key");
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 300)
        .expect("noted");
    assert_eq!(kept(&store, "web-1").1, Some(NOW + 300));
}

/// Catches: a report of a host id never approved refused, or making a host file for it.
#[test]
fn a_host_never_approved_records_nothing_it_runs() {
    let dir = scratch("running-unknown");
    let store = open(dir.path());
    store
        .post(request(&HostKey::new(), "m-1"), NOW)
        .expect("posted");
    store
        .note_running(&host_id("web-1"), &runs("1.0", "deb"), NOW + 60)
        .expect("nothing to record");
    assert!(!dir.path().join("hosts/web-1.pb").exists());
}

/// Catches: a host file that does not decode read as a host that ran nothing, and noted over.
#[test]
fn a_host_file_that_cannot_be_read_is_refused_what_the_host_runs() {
    let dir = scratch("running-corrupt");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    std::fs::write(dir.path().join("hosts/web-1.pb"), b"\xff").expect("write");
    let refused = store
        .note_running(&host_id("web-1"), &runs("1.0", "deb"), NOW + 60)
        .expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
    assert_eq!(
        std::fs::read(dir.path().join("hosts/web-1.pb")).expect("read"),
        b"\xff"
    );
}

/// Catches: every report that says what was said last taking the store's lock and reading the
/// host file, a host remembered as running something past a write that failed (never written
/// at all) or past an approval (its next record never written), and a report after a change
/// answered from the memory of the one before it.
#[test]
fn a_report_that_changes_nothing_touches_no_file_until_something_changes() {
    let dir = scratch("running-memory");
    let fs = Arc::new(FaultFs::new());
    let store = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");

    // Noting takes the lock (1), reads the host file (2), then writes it anew (3): that fails.
    fs.fail_at(fs.count() + 3);
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 50)
        .expect_err("not written");
    assert_eq!(kept(&store, "web-1"), (None, None));
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 60)
        .expect("noted");
    let before = fs.count();
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 75)
        .expect("noted");
    assert_eq!(fs.count(), before, "answered from memory");
    store
        .note_running(&web, &runs("1.1", "deb"), NOW + 90)
        .expect("noted");
    assert_ne!(fs.count(), before, "a change is written");
    assert_eq!(kept(&store, "web-1").1, Some(NOW + 90));
    let before = fs.count();
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 100)
        .expect("noted");
    assert_ne!(fs.count(), before, "the memory holds the last record only");

    enroll(&store, &HostKey::new(), "m-9", "web-1", true);
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 300)
        .expect("noted");
    assert_eq!(kept(&store, "web-1").1, Some(NOW + 300));
}

/// Catches: a store that already holds what the host runs (a restarted coordinator's) writing
/// it again at the first heartbeat, and one that reads the record but then does not answer
/// from memory.
#[test]
fn a_reopened_store_that_holds_the_record_does_not_write_it_again() {
    let dir = scratch("running-reopened");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    store
        .note_running(&web, &runs("1.0", "deb"), NOW + 60)
        .expect("noted");
    let reopened = open(dir.path());
    reopened
        .note_running(&web, &runs("1.0", "deb"), NOW + 500)
        .expect("noted");
    assert_eq!(kept(&reopened, "web-1").1, Some(NOW + 60));
}

/// Catches: an enrolled host listed without its role, a denied host among the enrolled ids, a
/// host id never approved among them, and ids out of host-id order.
#[test]
fn the_enrolled_ids_are_the_approved_hosts_that_are_not_denied() {
    let dir = scratch("running-ids");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-2", "web-2", false);
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    enroll(&store, &HostKey::new(), "m-3", "web-3", false);
    store
        .post(request(&HostKey::new(), "m-4"), NOW)
        .expect("posted");
    store.deny(&host_id("web-3")).expect("denied");
    assert_eq!(
        store.enrolled_host_ids().expect("ids"),
        [host_id("web-1"), host_id("web-2")]
    );
    let enrolled: EnrolledHost = store
        .enrolled_host(&host_id("web-1"))
        .expect("read")
        .expect("enrolled");
    assert_eq!(enrolled.role.as_str(), "web");
}

/// The host file as the release before wrote it: no field 10.
#[derive(Clone, PartialEq, prost::Message)]
struct HostBefore {
    #[prost(uint32, tag = "1")]
    schema: u32,
    #[prost(string, tag = "2")]
    host_id: String,
    #[prost(bytes = "vec", tag = "3")]
    key_hash: Vec<u8>,
    #[prost(string, tag = "5")]
    role: String,
    #[prost(optional, uint64, tag = "9")]
    first_serving_unix: Option<u64>,
}

/// Catches: a host file of the release before read with a record of what the host runs, or one
/// that this release writes refused by, or written back changed by, a reader that does not know
/// field 10 (prost keeps no unknown field, so what that reader knows is read intact).
#[test]
fn the_host_file_is_compatible_with_the_release_before_in_both_directions() {
    let before = HostBefore {
        schema: 1,
        host_id: "web-1".to_owned(),
        key_hash: vec![7; 32],
        role: "web".to_owned(),
        first_serving_unix: Some(NOW),
    };
    let read = pb::Host::decode(before.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.running, None);
    assert_eq!(read.first_serving_unix, Some(NOW));

    let now = pb::Host {
        schema: 1,
        host_id: "web-1".to_owned(),
        key_hash: vec![7; 32],
        role: "web".to_owned(),
        first_serving_unix: Some(NOW),
        running: Some(pb::Running {
            agent_name: "hostkeeper".to_owned(),
            agent_version: "0.2.0".to_owned(),
            services: vec![pb::RunningService {
                service: "web".to_owned(),
                version: "1.0".to_owned(),
                installer: "deb".to_owned(),
            }],
            at_unix: NOW + 60,
        }),
        ..pb::Host::default()
    };
    let bytes = now.encode_to_vec();
    assert!(bytes.contains(&0x52), "field 10 is on the wire");
    let old = HostBefore::decode(bytes.as_slice()).expect("the release before reads it");
    assert_eq!(old, before);
}
