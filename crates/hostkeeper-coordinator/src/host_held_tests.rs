//! `InventoryRecord.host_held`: `Enrollment.ListRecords` and `Enrollment.AddRecord` say, for
//! each record, what the enrollment store holds for its host id (no host, an approved host, a
//! denied one or a retired one), read from the store by host id. It holds after a coordinator
//! restart and for a host never heard from, which `Hosts.List` (the hosts heard from since the
//! coordinator started) does not show; a coordinator without enrollment says nothing
//! (`UNSPECIFIED`); and a store that cannot be read is refused, never answered as "no host".

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enroll_request, host_id};
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator, start};
use crate::testutil::Rig;

use proto::HostHeld as Held;

/// A coordinator of `web` and `db`, past its quiet start, over its own enrollment.
struct Fixture {
    enrollment: EnrollmentRig,
    rig: Rig,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let enrollment = EnrollmentRig::new(&format!("{name}-enrollment"));
        let mut rig = Rig::new(name);
        rig.coordinator =
            start(&rig.dir, &rig.clock, "").with_enrollment(Arc::clone(&enrollment.enrollment));
        rig.at(2_000);
        Fixture { enrollment, rig }
    }

    /// The coordinator and its enrollment started again over the same files.
    fn restart(&mut self) {
        self.rig.coordinator =
            start(&self.rig.dir, &self.rig.clock, "").with_enrollment(self.enrollment.restart());
    }

    /// `AddRecord` of `host` with the data MAC `data`.
    fn add(
        &self,
        host: &str,
        data: &str,
    ) -> Result<proto::InventoryRecord, proto::EnrollmentRefusal> {
        self.rig
            .coordinator
            .add_record(
                &operator(),
                proto::AddRecordRequest {
                    host_id: host.to_owned(),
                    data_macs: vec![data.to_owned()],
                    ..proto::AddRecordRequest::default()
                },
            )
            .map(|added| added.record.expect("the record"))
    }

    /// A key that posted from machine `machine`, claiming no address (so it matches no record),
    /// approved by the operator as `host`; never heard from.
    fn approve(&self, machine: &str, host: &str) {
        let key = HostKey::new();
        let posted = self
            .enrollment
            .enrollment
            .enroll(enroll_request(&key, machine))
            .expect("posted");
        assert_eq!(posted.state, EnrollState::Pending as i32);
        self.rig
            .coordinator
            .enrollment_approve(
                &operator(),
                proto::ApproveRequest {
                    request_id: key.key_hash().to_hex(),
                    key_sha256: key.key_hash().to_hex(),
                    host_id: host.to_owned(),
                    role: "storage".to_owned(),
                    reason: "built today".to_owned(),
                    ..proto::ApproveRequest::default()
                },
            )
            .expect("approved");
    }

    /// `Hosts.Retire` of `host`, which holds no update, so its retirement is final at once.
    fn retire(&self, host: &str) {
        let retired = self
            .rig
            .coordinator
            .retire_host(
                &operator(),
                &proto::RetireRequest {
                    node_id: host.to_owned(),
                    reason: "decommissioned".to_owned(),
                },
            )
            .expect("retired");
        assert!(!retired.update_in_flight);
    }

    fn page(
        &self,
        limit: u32,
        token: &str,
    ) -> Result<proto::ListRecordsResponse, proto::EnrollmentRefusal> {
        self.rig.coordinator.list_records(
            &operator(),
            proto::ListRecordsRequest {
                limit,
                page_token: token.to_owned(),
                enrolled: None,
            },
        )
    }

    /// Each record's host id, `enrolled` and `host_held`, every record on one page.
    fn held(&self) -> Vec<(String, bool, Held)> {
        self.page(0, "")
            .expect("listed")
            .records
            .into_iter()
            .map(|r| (r.host_id.clone(), r.enrolled, r.host_held()))
            .collect()
    }

    /// The node ids `Hosts.List` lists.
    fn listed_hosts(&self) -> Vec<String> {
        self.rig
            .coordinator
            .list_hosts(&operator(), proto::ListHostsRequest::default())
            .expect("listed")
            .hosts
            .into_iter()
            .map(|h| h.node_id)
            .collect()
    }
}

fn row(host: &str, enrolled: bool, held: Held) -> (String, bool, Held) {
    (host.to_owned(), enrolled, held)
}

/// Proves: after a coordinator restart, `ListRecords` says `RETIRED` for the record of a host
/// retired before it, `DENIED` for one denied whose retirement is not final, `APPROVED` for
/// one approved and never heard from, and `NONE` only for the record no host id holds; each
/// answer is the one given before the restart; and only the approved hosts are in `Hosts.List`
/// after the restart, not live, with no `enrolled` record (their claims match none).
///
/// Catches: `host_held` read from the hosts heard from since start (the fleet, `Hosts.List`),
/// which after a restart would answer `NONE` for a retired, a denied and a silent approved
/// host, so that a network install would be offered to each.
#[test]
fn list_records_says_what_the_store_holds_for_each_host_id_after_a_restart() {
    let mut fixture = Fixture::new("host-held-restart");
    for (n, host) in ["db-0", "db-1", "db-2", "db-3", "db-4"].iter().enumerate() {
        fixture
            .add(host, &format!("0c:c4:7a:00:00:{n}2"))
            .expect("recorded");
    }
    fixture.approve("m-1", "db-1");
    ask(
        &fixture.rig,
        "db-1",
        "storage",
        beat_of(NEW_DAEMON, Vec::new()),
    );
    fixture.approve("m-2", "db-2");
    fixture.approve("m-3", "db-3");
    fixture.retire("db-3");
    fixture.approve("m-4", "db-4");
    fixture
        .enrollment
        .enrollment
        .store()
        .deny(&host_id("db-4"))
        .expect("denied, retirement not final");
    let want = vec![
        row("db-0", false, Held::None),
        row("db-1", false, Held::Approved),
        row("db-2", false, Held::Approved),
        row("db-3", false, Held::Retired),
        row("db-4", false, Held::Denied),
    ];
    assert_eq!(fixture.held(), want, "before the restart");

    fixture.restart();
    // Hosts enrolled and not denied are listed though none was heard since the restart.
    assert_eq!(fixture.listed_hosts(), ["db-1", "db-2"]);
    assert_eq!(fixture.held(), want, "after the restart");
}

/// Proves: `AddRecord` answers `host_held` for the id it records: `RETIRED` for an id retired
/// before the record existed, `APPROVED` for an id approved and never heard from, and `NONE`
/// for an id no host holds; `ListRecords` then answers the same.
///
/// Catches: `host_held` set only by `ListRecords`, so the add's answer says `UNSPECIFIED` and
/// contradicts the next list.
#[test]
fn add_record_answers_what_the_store_holds_for_its_host_id() {
    let fixture = Fixture::new("host-held-add");
    fixture.approve("m-1", "db-1");
    fixture.retire("db-1");
    fixture.approve("m-2", "db-2");

    let added: Vec<Held> = [
        ("db-1", "0c:c4:7a:00:00:12"),
        ("db-2", "0c:c4:7a:00:00:22"),
        ("db-3", "0c:c4:7a:00:00:32"),
    ]
    .iter()
    .map(|(host, mac)| fixture.add(host, mac).expect("recorded").host_held())
    .collect();
    assert_eq!(added, [Held::Retired, Held::Approved, Held::None]);
    assert_eq!(
        fixture.held(),
        [
            row("db-1", false, Held::Retired),
            row("db-2", false, Held::Approved),
            row("db-3", false, Held::None),
        ]
    );
}

/// Proves: a coordinator without enrollment answers `host_held` `UNSPECIFIED` in `AddRecord`
/// and `ListRecords`, never `NONE`.
///
/// Catches: a coordinator that has no store to read saying that no host holds the id.
#[test]
fn without_enrollment_host_held_is_unspecified() {
    let mut rig = Rig::new("host-held-no-enrollment");
    rig.coordinator = start(&rig.dir, &rig.clock, "");
    rig.at(2_000);
    let added = rig
        .coordinator
        .add_record(
            &operator(),
            proto::AddRecordRequest {
                host_id: "db-1".to_owned(),
                serial: "SN-1".to_owned(),
                ..proto::AddRecordRequest::default()
            },
        )
        .expect("recorded")
        .record
        .expect("the record");
    assert_eq!(added.host_held(), Held::Unspecified);
    let listed = rig
        .coordinator
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed");
    assert_eq!(
        listed
            .records
            .iter()
            .map(proto::InventoryRecord::host_held)
            .collect::<Vec<_>>(),
        [Held::Unspecified]
    );
}

/// Proves: when the deny file of a record's host id does not decode, `ListRecords` refuses
/// `store_failed` for the page that holds the record, and answers the page before it, which
/// does not; and `AddRecord` of that id refuses `store_failed` without writing the record.
///
/// Catches: an unreadable deny row answered as `NONE` (a retired host's record would then look
/// free), the whole inventory refused for one record a page does not show, and a record
/// written whose answer is a refusal.
#[test]
fn an_unreadable_deny_file_refuses_the_page_of_its_record_and_its_add() {
    let fixture = Fixture::new("host-held-unreadable");
    fixture.add("db-1", "0c:c4:7a:00:00:12").expect("recorded");
    fixture.add("db-2", "0c:c4:7a:00:00:22").expect("recorded");
    let store = fixture.enrollment.files.store.clone();
    for host in ["db-2", "db-3"] {
        std::fs::write(store.join(format!("deny/{host}.pb")), b"\xff").expect("write");
    }

    let first = fixture.page(1, "").expect("the first page");
    assert_eq!(
        (
            first
                .records
                .iter()
                .map(|r| (r.host_id.as_str(), r.host_held()))
                .collect::<Vec<_>>(),
            first.next_page_token.as_str()
        ),
        (vec![("db-1", Held::None)], "db-1")
    );
    assert_eq!(
        fixture.page(1, "db-1"),
        Err(proto::EnrollmentRefusal::StoreFailed)
    );
    assert_eq!(
        fixture.add("db-3", "0c:c4:7a:00:00:32"),
        Err(proto::EnrollmentRefusal::StoreFailed)
    );
    let held = fixture
        .rig
        .coordinator
        .ops
        .read(|state| state.inventory.iter().any(|r| r.host_id == "db-3"));
    assert!(!held, "nothing is written");
}
