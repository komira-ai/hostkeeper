//! A host joined to its host-inventory record: `Hosts.Get` and `Hosts.List` answer the record
//! an enrolled host's claims match (its host id, data MACs, BMC MAC and serial) and when the
//! host was approved; `Enrollment.ListRecords` marks each record a host is enrolled against and
//! filters by it; `Enrollment.AddRecord` answers a record an enrolled host already matches as
//! enrolled. The match is the one `Enrollment.List` shows a pending request
//! ([`crate::inventory::match_claims`]), which [`RecordIndex`] answers too.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::Claims;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::enrollment::tests::support::{HostKey, NOW, Rig as EnrollmentRig, enroll_request};
use crate::inventory::{InventoryMatch, InventoryRecord, RecordIndex, match_claims};
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator, start};
use crate::testutil::Rig;

/// The data MACs, BMC MAC and serial of the record `db-1`.
const DB1_DATA: [&str; 2] = ["0c:c4:7a:00:00:02", "0c:c4:7a:00:00:03"];
const DB1_BMC: &str = "0c:c4:7a:00:00:01";
const DB1_SERIAL: &str = "SN-1";

/// The data MAC of the records `db-2` and `db-3`, which no host enrolls against.
const DB2_DATA: &str = "0c:c4:7a:00:00:12";
const DB3_DATA: &str = "0c:c4:7a:00:00:22";

/// The data MACs of the records `db-4` and `db-5`, both claimed by the host `amb-1`.
const DB4_DATA: &str = "0c:c4:7a:00:00:42";
const DB5_DATA: &str = "0c:c4:7a:00:00:52";

/// The data MAC of `web-9`, which no record holds when it enrolls.
const WEB9_DATA: &str = "0c:c4:7a:00:00:92";

/// When the enrollment's clock approves `db-1`, `web-9` and `amb-1`.
const DB1_APPROVED: u64 = NOW + 4_321;
const WEB9_APPROVED: u64 = NOW + 5_000;
const AMB1_APPROVED: u64 = NOW + 6_000;

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

    /// Records `host` with `data`, `bmc` and `serial`.
    fn record(&self, host: &str, data: &[&str], bmc: &str, serial: &str) -> proto::InventoryRecord {
        self.rig
            .coordinator
            .add_record(
                &operator(),
                proto::AddRecordRequest {
                    host_id: host.to_owned(),
                    data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
                    bmc_mac: bmc.to_owned(),
                    serial: serial.to_owned(),
                    ..proto::AddRecordRequest::default()
                },
            )
            .expect("recorded")
            .record
            .expect("the record")
    }

    /// A key that posted from machine `machine`, claiming `addresses` and the BMC MAC `bmc`,
    /// approved by the operator as `host` (empty: the record's) at `at`, then heard from.
    fn enroll(&self, machine: &str, addresses: &[&str], bmc: &str, host: &str, at: u64) -> HostKey {
        let key = HostKey::new();
        let mut request = enroll_request(&key, machine);
        let claims = request.claims.as_mut().expect("claims");
        claims.addresses = addresses.iter().map(|a| (*a).to_owned()).collect();
        claims.bmc_mac = bmc.to_owned();
        let posted = self.enrollment.enrollment.enroll(request).expect("posted");
        assert_eq!(posted.state, EnrollState::Pending as i32);
        self.enrollment.clock.set(at);
        let approved = self
            .rig
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
        ask(
            &self.rig,
            &approved.host_id,
            "storage",
            beat_of(NEW_DAEMON, Vec::new()),
        );
        key
    }

    /// The records `db-1` to `db-5`; `db-1` enrolled by MAC, `web-9` enrolled matching no
    /// record, and `amb-1` enrolled matching both `db-4` and `db-5`.
    fn seeded(name: &str) -> Fixture {
        let fixture = Fixture::new(name);
        fixture.record("db-1", &DB1_DATA, DB1_BMC, DB1_SERIAL);
        fixture.record("db-2", &[DB2_DATA], "0c:c4:7a:00:00:11", "SN-2");
        fixture.record("db-3", &[DB3_DATA], "", "");
        fixture.record("db-4", &[DB4_DATA], "", "");
        fixture.record("db-5", &[DB5_DATA], "", "");
        fixture.enroll(
            "m-1",
            &["192.0.2.10", DB1_DATA[1]],
            DB1_BMC,
            "",
            DB1_APPROVED,
        );
        fixture.enroll("m-9", &[WEB9_DATA], "", "web-9", WEB9_APPROVED);
        fixture.enroll("m-a", &[DB4_DATA, DB5_DATA], "", "amb-1", AMB1_APPROVED);
        fixture
    }

    fn get(&self, node_id: &str) -> Result<proto::Host, proto::HostsRefusal> {
        self.rig
            .coordinator
            .get_host(
                &operator(),
                &proto::GetHostRequest {
                    node_id: node_id.to_owned(),
                },
            )
            .map(|got| got.host.expect("a host"))
    }

    fn list(&self) -> Result<Vec<proto::Host>, proto::HostsRefusal> {
        self.rig
            .coordinator
            .list_hosts(&operator(), proto::ListHostsRequest::default())
            .map(|listed| listed.hosts)
    }

    fn records(
        &self,
        limit: u32,
        token: &str,
        enrolled: Option<bool>,
    ) -> Result<proto::ListRecordsResponse, proto::EnrollmentRefusal> {
        self.rig.coordinator.list_records(
            &operator(),
            proto::ListRecordsRequest {
                limit,
                page_token: token.to_owned(),
                enrolled,
            },
        )
    }

    /// Each listed record's host id and `enrolled`, with `enrolled` asked for.
    fn marks(&self, enrolled: Option<bool>) -> Vec<(String, bool)> {
        self.records(0, "", enrolled)
            .expect("listed")
            .records
            .into_iter()
            .map(|r| (r.host_id, r.enrolled))
            .collect()
    }
}

/// What the inventory says of a host: its record id, data MACs, BMC MAC, serial and enrollment
/// time.
fn inventory_of(host: &proto::Host) -> (&str, Vec<&str>, &str, &str, u64) {
    (
        host.inventory_record_id.as_str(),
        host.data_macs.iter().map(String::as_str).collect(),
        host.bmc_mac.as_str(),
        host.serial.as_str(),
        host.enrolled_unix,
    )
}

fn marked(pairs: &[(&str, bool)]) -> Vec<(String, bool)> {
    pairs.iter().map(|(h, e)| ((*h).to_owned(), *e)).collect()
}

/// Proves: a host enrolled by one of its record's data MACs answers, in `Hosts.Get` and in
/// `Hosts.List`, that record's host id, every data MAC of it (not only the one it matched by),
/// its BMC MAC and serial, and the time its approval was made by the enrollment's clock; a host
/// whose claims match no record, or two records, answers OK with every record field empty and
/// its approval time.
///
/// Catches: the fields left unset, filled from the host's claims instead of the record, the
/// time taken from another clock or another host, an ambiguous match resolved to one of its
/// records, and a host without a record refused or dropped from the list.
#[test]
fn a_host_answers_the_record_it_enrolled_against_and_when() {
    let fixture = Fixture::seeded("host-inventory-get");
    let db1 = ("db-1", DB1_DATA.to_vec(), DB1_BMC, DB1_SERIAL, DB1_APPROVED);
    let none = |at| ("", Vec::new(), "", "", at);

    assert_eq!(inventory_of(&fixture.get("db-1").expect("db-1")), db1);
    assert_eq!(
        inventory_of(&fixture.get("web-9").expect("web-9")),
        none(WEB9_APPROVED)
    );
    assert_eq!(
        inventory_of(&fixture.get("amb-1").expect("amb-1")),
        none(AMB1_APPROVED)
    );
    let listed = fixture.list().expect("listed");
    assert_eq!(
        listed.iter().map(inventory_of).collect::<Vec<_>>(),
        [none(AMB1_APPROVED), db1, none(WEB9_APPROVED)]
    );
    assert_eq!(
        listed
            .iter()
            .map(|h| h.node_id.as_str())
            .collect::<Vec<_>>(),
        ["amb-1", "db-1", "web-9"]
    );
}

/// Proves: `ListRecords` marks only the record a host enrolled against; `enrolled` unset lists
/// every record with its mark, `true` only the enrolled one, `false` only the others (records
/// an ambiguous host claims included); the filter pages by host id over the records it keeps.
///
/// Catches: a mark on a record no host matched (by an ambiguous claim above all), the filter
/// ignored or inverted, and a page token that counts the records the filter left out.
#[test]
fn list_records_marks_and_filters_the_enrolled_records() {
    let fixture = Fixture::seeded("host-inventory-records");
    let all = [
        ("db-1", true),
        ("db-2", false),
        ("db-3", false),
        ("db-4", false),
        ("db-5", false),
    ];
    assert_eq!(fixture.marks(None), marked(&all));
    assert_eq!(fixture.marks(Some(true)), marked(&all[..1]));
    assert_eq!(fixture.marks(Some(false)), marked(&all[1..]));

    let first = fixture.records(3, "", Some(false)).expect("a page");
    assert_eq!(
        (
            first
                .records
                .iter()
                .map(|r| r.host_id.as_str())
                .collect::<Vec<_>>(),
            first.next_page_token.as_str()
        ),
        (vec!["db-2", "db-3", "db-4"], "db-4")
    );
    let second = fixture.records(3, "db-4", Some(false)).expect("a page");
    assert_eq!(
        (
            second
                .records
                .iter()
                .map(|r| r.host_id.as_str())
                .collect::<Vec<_>>(),
            second.next_page_token.as_str()
        ),
        (vec!["db-5"], "")
    );
    let only = fixture.records(1, "", Some(true)).expect("a page");
    assert_eq!(only.records.len(), 1);
    assert_eq!(only.next_page_token, "", "no enrolled record after db-1");
}

/// Proves: `ListRecords` names the host enrolled against a record (`enrolled_host_id`), empty for
/// a record no host is enrolled against, and an ambiguous host names none; the answer of
/// `AddRecord` leaves it out.
///
/// Catches: the Inventory row saying "enrolled" without saying which host, and a host named on a
/// record it only half matched.
#[test]
fn list_records_names_the_host_enrolled_against_each_record() {
    let fixture = Fixture::seeded("host-inventory-host-names");
    let named: Vec<(String, String)> = fixture
        .records(0, "", None)
        .expect("listed")
        .records
        .into_iter()
        .map(|r| (r.host_id, r.enrolled_host_id))
        .collect();
    let want = [
        ("db-1", "db-1"),
        ("db-2", ""),
        ("db-3", ""),
        ("db-4", ""),
        ("db-5", ""),
    ];
    assert_eq!(
        named,
        want.iter()
            .map(|(r, h)| ((*r).to_owned(), (*h).to_owned()))
            .collect::<Vec<_>>()
    );
}

/// Proves: a record added after its host enrolled (by that host's data MAC) is answered by
/// `AddRecord` as enrolled, and from then on the host answers it; a record no host matches is
/// answered as not enrolled.
///
/// Catches: `enrolled` computed only by `ListRecords`, so the add's answer contradicts the
/// next list, and a host's record read once at its enrollment instead of at each call.
#[test]
fn a_record_added_after_its_host_enrolled_is_enrolled_at_once() {
    let fixture = Fixture::seeded("host-inventory-late-record");
    assert!(
        !fixture
            .record("db-6", &["0c:c4:7a:00:00:62"], "", "")
            .enrolled
    );

    let late = fixture.record("web-9", &[WEB9_DATA], "", "SN-9");
    assert!(late.enrolled);
    assert_eq!(
        inventory_of(&fixture.get("web-9").expect("web-9")),
        ("web-9", vec![WEB9_DATA], "", "SN-9", WEB9_APPROVED)
    );
}

/// Proves: a host whose id is denied is no longer enrolled: `Hosts.Get` and `Hosts.List` (it is
/// still heard of) answer no record and an enrollment time of 0, and its record is still listed,
/// as not enrolled, among the records the "not enrolled" filter keeps.
///
/// Catches: a retired host kept as its record's enrolled host, which would hide the record
/// from the "not enrolled" view when the server is rebuilt.
#[test]
fn a_denied_host_is_no_longer_enrolled_against_its_record() {
    let fixture = Fixture::seeded("host-inventory-denied");
    fixture
        .enrollment
        .enrollment
        .store()
        .deny(&crate::enrollment::tests::support::host_id("db-1"))
        .expect("denied");

    assert_eq!(
        inventory_of(&fixture.get("db-1").expect("db-1")),
        ("", Vec::new(), "", "", 0)
    );
    let db1 = fixture
        .list()
        .expect("listed")
        .into_iter()
        .find(|h| h.node_id == "db-1")
        .expect("db-1 is still listed");
    assert_eq!(inventory_of(&db1), ("", Vec::new(), "", "", 0));
    assert_eq!(fixture.marks(Some(true)), marked(&[]));
    let all = fixture.marks(None);
    assert_eq!(all.first(), Some(&("db-1".to_owned(), false)));
    assert_eq!(all.len(), 5, "every record is still listed");
    assert_eq!(
        fixture.marks(Some(false)),
        all,
        "db-1 is among the not enrolled"
    );
}

/// Proves: when the enrollment store cannot be read (a host's request file is gone),
/// `Hosts.Get`, `Hosts.List` and `ListRecords` refuse `store_failed`, and `AddRecord` refuses
/// it without writing the record.
///
/// Catches: an unreadable store answered as "no host enrolled", which would show every host
/// without its record and every record as not enrolled, and an add written whose answer then
/// claims nothing changed.
#[test]
fn an_unreadable_enrollment_store_is_refused_not_shown_empty() {
    let fixture = Fixture::seeded("host-inventory-unreadable");
    let key = fixture.enroll("m-7", &["0c:c4:7a:00:00:72"], "", "db-7", NOW + 7_000);
    let request = fixture
        .enrollment
        .files
        .store
        .join(format!("requests/{}.pb", key.key_hash().to_hex()));
    std::fs::remove_file(request).expect("removed");

    assert_eq!(fixture.get("db-7"), Err(proto::HostsRefusal::StoreFailed));
    assert_eq!(fixture.list(), Err(proto::HostsRefusal::StoreFailed));
    assert_eq!(
        fixture.records(0, "", None),
        Err(proto::EnrollmentRefusal::StoreFailed)
    );
    let refused = fixture.rig.coordinator.add_record(
        &operator(),
        proto::AddRecordRequest {
            host_id: "db-8".to_owned(),
            serial: "SN-8".to_owned(),
            ..proto::AddRecordRequest::default()
        },
    );
    assert_eq!(refused, Err(proto::EnrollmentRefusal::StoreFailed));
    let held = fixture
        .rig
        .coordinator
        .ops
        .read(|state| state.inventory.iter().any(|r| r.host_id == "db-8"));
    assert!(!held, "nothing is written");
}

/// A record as the ops file keeps it.
fn record(host: &str, data: &[&str], bmc: &str, serial: &str) -> InventoryRecord {
    InventoryRecord {
        host_group: String::new(),
        host_id: host.to_owned(),
        role: String::new(),
        data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        serial: serial.to_owned(),
        notes: String::new(),
        by: "operator".to_owned(),
        at_unix: NOW,
        approve_until_unix: 0,
        used: None,
        platform: None,
    }
}

/// Proves: [`RecordIndex::matched`] answers what [`match_claims`] answers for each kind of
/// claim: one data MAC of one record, two data MACs of one record, data MACs of two records,
/// a data MAC with the record's BMC MAC, with another BMC MAC, an InfiniBand or IP address
/// only, no address, a Mac serial in another case, a Mac serial no record holds, and a Mac
/// serial that would match while its data MACs are another record's (the serial decides).
///
/// Catches: an index that misses a record [`match_claims`] finds (a serial compared with its
/// case, a MAC in another spelling), finds one it does not (a BMC MAC not checked, a serial
/// matched for a Linux request), or resolves an ambiguous match.
#[test]
fn the_index_matches_as_match_claims_does() {
    let records = [
        record("db-1", &DB1_DATA, DB1_BMC, "SN-1"),
        record("db-2", &[DB2_DATA], "", ""),
        record("mac-1", &[], "", "C02XK1ABJG5J"),
    ];
    let index = RecordIndex::new(&records);
    let claims = |addresses: &[&str], bmc: &str, mac_serial: &str| Claims {
        addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        mac_serial: mac_serial.to_owned(),
        ..Claims::default()
    };
    let cases = [
        (claims(&["0C-C4-7A-00-00-02"], "", ""), Some("db-1")),
        (claims(&DB1_DATA, "", ""), Some("db-1")),
        (claims(&[DB1_DATA[0], DB2_DATA], "", ""), None),
        (claims(&[DB1_DATA[1]], DB1_BMC, ""), Some("db-1")),
        (claims(&[DB1_DATA[1]], "0c:c4:7a:00:00:99", ""), None),
        (
            claims(
                &["192.0.2.10", "80:00:02:08:fe:80:00:00:00:00:00:00:00:02"],
                "",
                "",
            ),
            None,
        ),
        (claims(&[], "", ""), None),
        (claims(&[], "", "c02xk1abjg5j"), Some("mac-1")),
        (claims(&[], "", "C02OTHER"), None),
        (claims(&[DB2_DATA], "", "C02XK1ABJG5J"), Some("mac-1")),
        (claims(&[DB2_DATA], "", "SN-1"), Some("db-1")),
    ];
    for (claims, want) in cases {
        let by_index = index.matched(&claims).map(|r| r.host_id.as_str());
        let by_scan = match match_claims(&claims, &records) {
            InventoryMatch::Matched(record) => Some(record.host_id.as_str()),
            InventoryMatch::Unmatched | InventoryMatch::Ambiguous(_) => None,
        };
        assert_eq!((by_index, by_scan), (want, want), "{claims:?}");
    }
}
