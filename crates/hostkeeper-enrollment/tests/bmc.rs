//! The BMC a host's daemon reported ([`EnrollmentStore::note_bmc`]): written into the record of
//! what the host runs when its address or MAC changes and not when only the time does, kept when
//! what the host runs is rewritten, kept by a reopened store, cleared by every approval of the
//! host id, never written before the host runs anything or for a host id never approved; and the
//! record's compatibility with the release before, in both directions.

mod support;

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::pb;
use hostkeeper_enrollment::enroll::{
    BMC_REMEMBERED_SECS, EnrollmentStore, HostBmc, HostRunning, KeptBmc, RunningService, StoreError,
};
use hostkeeper_enrollment::fs::{FaultFs, RealFs};
use hostkeeper_enrollment::names::MacAddress;
use prost::Message as _;
use support::{HostKey, NOW, decision, host_id, request, scratch};

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

/// A daemon `0.2.0` running `web` at `version`.
fn runs(version: &str) -> HostRunning {
    HostRunning {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "0.2.0".to_owned(),
        services: vec![RunningService {
            service: "web".to_owned(),
            version: version.to_owned(),
            installer: "deb".to_owned(),
            updating: None,
        }],
    }
}

/// The BMC at `192.0.2.<last>` with the MAC `0c:c4:7a:00:00:<mac>`.
fn bmc(last: u8, mac: u8) -> HostBmc {
    HostBmc {
        ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)),
        mac: MacAddress::parse(&format!("0c:c4:7a:00:00:{mac:02x}")).expect("a MAC"),
    }
}

/// What `store` answers for `host`'s BMC.
fn kept(store: &EnrollmentStore, host: &str) -> Option<KeptBmc> {
    store
        .enrolled_host(&host_id(host))
        .expect("read")
        .expect("enrolled")
        .bmc
}

/// Catches: a report that is not written, one written again for the time alone (which would write
/// the host file at every heartbeat), one that keeps the time of the latest heartbeat instead of
/// the one that first reported it, a new address or a new MAC that is not written, one kept in
/// memory only (lost when the store is opened again), and a BMC lost when what the host runs is
/// rewritten, or what it runs lost when the BMC is.
#[test]
fn a_bmc_is_written_when_it_changes_and_kept_by_a_rewrite_of_what_the_host_runs() {
    let dir = scratch("bmc-changes");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    store
        .note_running(&web, &runs("1.0"), NOW + 10)
        .expect("ran");
    assert_eq!(kept(&store, "web-1"), None, "nothing reported yet");

    store.note_bmc(&web, bmc(12, 1), NOW + 20).expect("noted");
    store.note_bmc(&web, bmc(12, 1), NOW + 35).expect("noted");
    let first = KeptBmc {
        bmc: bmc(12, 1),
        at_unix: NOW + 20,
    };
    assert_eq!(kept(&store, "web-1"), Some(first));

    store
        .note_running(&web, &runs("1.1"), NOW + 50)
        .expect("ran");
    let enrolled = store.enrolled_host(&web).expect("read").expect("enrolled");
    assert_eq!(enrolled.bmc, Some(first), "kept by a rewrite of what runs");
    assert_eq!(enrolled.running, Some(runs("1.1")));
    assert_eq!(enrolled.running_unix, Some(NOW + 50));

    store.note_bmc(&web, bmc(40, 1), NOW + 65).expect("noted");
    let moved = KeptBmc {
        bmc: bmc(40, 1),
        at_unix: NOW + 65,
    };
    assert_eq!(kept(&store, "web-1"), Some(moved), "a new address");
    let enrolled = store.enrolled_host(&web).expect("read").expect("enrolled");
    assert_eq!(enrolled.running, Some(runs("1.1")), "what runs is kept");
    assert_eq!(enrolled.running_unix, Some(NOW + 50));

    store.note_bmc(&web, bmc(40, 2), NOW + 80).expect("noted");
    assert_eq!(
        kept(&open(dir.path()), "web-1"),
        Some(KeptBmc {
            bmc: bmc(40, 2),
            at_unix: NOW + 80,
        }),
        "a new MAC, kept on disk"
    );
}

/// Catches: a report of a host id never approved refused, or making a host file for it, and a
/// report before the host runs anything written (as a record of a host that runs nothing) or
/// remembered as written, so that the next one after the host runs something is never written.
#[test]
fn no_bmc_is_recorded_before_the_host_runs_anything_or_for_a_host_never_approved() {
    let dir = scratch("bmc-none");
    let store = open(dir.path());
    store
        .note_bmc(&host_id("web-9"), bmc(12, 1), NOW)
        .expect("nothing to record");
    assert!(!dir.path().join("hosts/web-9.pb").exists());

    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    store.note_bmc(&web, bmc(12, 1), NOW + 10).expect("noted");
    let enrolled = store.enrolled_host(&web).expect("read").expect("enrolled");
    assert_eq!((enrolled.running, enrolled.bmc), (None, None));

    store
        .note_running(&web, &runs("1.0"), NOW + 20)
        .expect("ran");
    store.note_bmc(&web, bmc(12, 1), NOW + 25).expect("noted");
    assert_eq!(
        kept(&store, "web-1"),
        Some(KeptBmc {
            bmc: bmc(12, 1),
            at_unix: NOW + 25,
        })
    );
}

/// Catches: every report that says what was said last taking the store's lock and reading the
/// host file, a BMC remembered past a write that failed (never written at all) or past an
/// approval (its next report never written), and a host file that does not decode read as a host
/// with no BMC and written over.
#[test]
fn a_report_that_changes_nothing_touches_no_file_until_something_changes() {
    let dir = scratch("bmc-memory");
    let fs = Arc::new(FaultFs::new());
    let store = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    store
        .note_running(&web, &runs("1.0"), NOW + 10)
        .expect("ran");

    // Noting takes the lock (1), reads the host file (2), then writes it anew (3): that fails.
    fs.fail_at(fs.count() + 3);
    store
        .note_bmc(&web, bmc(12, 1), NOW + 20)
        .expect_err("not written");
    assert_eq!(kept(&store, "web-1"), None);
    store.note_bmc(&web, bmc(12, 1), NOW + 30).expect("noted");
    let before = fs.count();
    store.note_bmc(&web, bmc(12, 1), NOW + 45).expect("noted");
    assert_eq!(fs.count(), before, "answered from memory");

    enroll(&store, &HostKey::new(), "m-9", "web-1", true);
    store
        .note_running(&web, &runs("1.0"), NOW + 300)
        .expect("ran");
    assert_eq!(kept(&store, "web-1"), None, "an approval clears it");
    store.note_bmc(&web, bmc(12, 1), NOW + 310).expect("noted");
    assert_eq!(kept(&store, "web-1").map(|k| k.at_unix), Some(NOW + 310));

    std::fs::write(dir.path().join("hosts/web-1.pb"), b"\xff").expect("write");
    let reopened = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    let refused = reopened
        .note_bmc(&web, bmc(12, 1), NOW + 400)
        .expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
    assert_eq!(
        std::fs::read(dir.path().join("hosts/web-1.pb")).expect("read"),
        b"\xff"
    );
}

/// Catches: a kept BMC whose address or MAC does not read answered as some BMC (or refusing the
/// host's read), and then not written over by the next report.
#[test]
fn a_kept_bmc_that_does_not_read_is_none_and_written_over() {
    for (ip, mac) in [
        ("not an address", "0c:c4:7a:00:00:01"),
        ("192.0.2.12", "0c:c4"),
    ] {
        let dir = scratch("bmc-unread");
        let store = open(dir.path());
        enroll(&store, &HostKey::new(), "m-1", "web-1", false);
        let web = host_id("web-1");
        store
            .note_running(&web, &runs("1.0"), NOW + 10)
            .expect("ran");
        let path = dir.path().join("hosts/web-1.pb");
        let mut host =
            pb::Host::decode(std::fs::read(&path).expect("read").as_slice()).expect("decodes");
        if let Some(running) = host.running.as_mut() {
            running.bmc = Some(pb::RunningBmc {
                ip: ip.to_owned(),
                mac: mac.to_owned(),
                at_unix: NOW + 5,
            });
        }
        std::fs::write(&path, host.encode_to_vec()).expect("write");
        let reopened = open(dir.path());
        assert_eq!(kept(&reopened, "web-1"), None, "{ip} {mac}");
        reopened
            .note_bmc(&web, bmc(12, 1), NOW + 20)
            .expect("noted");
        assert_eq!(
            kept(&reopened, "web-1"),
            Some(KeptBmc {
                bmc: bmc(12, 1),
                at_unix: NOW + 20,
            })
        );
    }
}

/// The record of what a host runs as the release before wrote it: no field 5.
#[derive(Clone, PartialEq, prost::Message)]
struct RunningBefore {
    #[prost(string, tag = "1")]
    agent_name: String,
    #[prost(string, tag = "2")]
    agent_version: String,
    #[prost(uint64, tag = "4")]
    at_unix: u64,
}

/// Catches: a record of the release before read with a BMC, or one that this release writes
/// refused by, or read changed by, a reader that does not know field 5 (prost keeps no unknown
/// field, so what that reader knows is read intact and the BMC is dropped when it writes).
#[test]
fn the_running_record_is_compatible_with_the_release_before_in_both_directions() {
    let before = RunningBefore {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "0.2.0".to_owned(),
        at_unix: NOW,
    };
    let read = pb::Running::decode(before.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.bmc, None);
    assert_eq!(read.at_unix, NOW);

    let now = pb::Running {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "0.2.0".to_owned(),
        at_unix: NOW,
        bmc: Some(pb::RunningBmc {
            ip: "192.0.2.12".to_owned(),
            mac: "0c:c4:7a:00:00:01".to_owned(),
            at_unix: NOW,
        }),
        ..pb::Running::default()
    };
    let bytes = now.encode_to_vec();
    assert!(bytes.contains(&0x2a), "field 5 is on the wire");
    let old = RunningBefore::decode(bytes.as_slice()).expect("the release before reads it");
    assert_eq!(old, before);
}

/// Catches: a store with nothing in memory (a coordinator restarted, or another one on the same
/// store) writing the record again for a BMC it already holds, or moving the time of the report
/// that first named it (the console's `reported_unix`).
#[test]
fn a_reopened_store_that_holds_the_bmc_does_not_write_it_again() {
    let dir = scratch("bmc-reopened");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    store
        .note_running(&web, &runs("1.0"), NOW + 10)
        .expect("ran");
    store.note_bmc(&web, bmc(12, 1), NOW + 20).expect("noted");
    let path = dir.path().join("hosts/web-1.pb");
    let written = std::fs::read(&path).expect("read");
    let reopened = open(dir.path());
    reopened
        .note_bmc(&web, bmc(12, 1), NOW + 90)
        .expect("noted");
    assert_eq!(std::fs::read(&path).expect("read"), written);
    assert_eq!(kept(&reopened, "web-1").map(|k| k.at_unix), Some(NOW + 20));
}

/// Catches: a BMC remembered for ever, so that a record another writer changed (another
/// coordinator, or a release before this one that dropped the BMC when it rewrote what the host
/// runs) is never written again while the host keeps reporting the same BMC; and one forgotten
/// at once (a read at every heartbeat).
#[test]
fn a_bmc_another_writer_dropped_is_written_again_once_the_memory_of_it_lapses() {
    let dir = scratch("bmc-lapse");
    let fs = Arc::new(FaultFs::new());
    let store = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    store
        .note_running(&web, &runs("1.0"), NOW + 10)
        .expect("ran");
    store.note_bmc(&web, bmc(12, 1), NOW + 20).expect("noted");

    let path = dir.path().join("hosts/web-1.pb");
    let mut host =
        pb::Host::decode(std::fs::read(&path).expect("read").as_slice()).expect("decodes");
    if let Some(running) = host.running.as_mut() {
        running.bmc = None;
    }
    std::fs::write(&path, host.encode_to_vec()).expect("written by another writer");

    let before = fs.count();
    store
        .note_bmc(&web, bmc(12, 1), NOW + 20 + BMC_REMEMBERED_SECS - 1)
        .expect("noted");
    assert_eq!(fs.count(), before, "answered from memory");
    assert_eq!(kept(&store, "web-1"), None);
    store
        .note_bmc(&web, bmc(12, 1), NOW + 20 + BMC_REMEMBERED_SECS)
        .expect("noted");
    assert_eq!(
        kept(&store, "web-1"),
        Some(KeptBmc {
            bmc: bmc(12, 1),
            at_unix: NOW + 20 + BMC_REMEMBERED_SECS,
        })
    );
    assert_eq!(BMC_REMEMBERED_SECS, 600);
    let before = fs.count();
    store
        .note_bmc(&web, bmc(12, 1), NOW + 5)
        .expect("a clock set back");
    assert_ne!(
        fs.count(),
        before,
        "a time before the memory's is not answered from it"
    );
}
