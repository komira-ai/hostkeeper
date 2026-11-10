//! Inventory records by host group and approval by record, from a coordinator that holds host
//! groups and from one that holds none (`docs/design/hostgroups.md` sections 1.2 and 7.2): a
//! record names a group by its `host_group` or its `role`, is answered as both, and approves a
//! request that matches it into that group.

use hostkeeper_enrollment::enroll::Membership;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollRequest, EnrollState};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal;

use super::support::{HostKey, NOW, claiming};
use crate::enrollment::{ByRecord, Waits};
use crate::host_groups_support::{REASON, SELF, UNIX, World, operator_entry, remove_of};
use crate::inventory::{DEFAULT_APPROVE_FOR_SECS, InventoryRecord};
use crate::marks_support::operator;
use crate::ops::{AuditEntry, Write};

use proto::EnrollmentRefusal as Refusal;

/// The data MAC of the record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";

/// The BMC MAC of the record `db-1`.
const DB1_BMC: &str = "0c:c4:7a:00:00:01";

/// A world whose coordinator holds the group `web`.
fn grouped(name: &str) -> World {
    let world = World::new(name);
    world.put("web", &[SELF], None).expect("put");
    world
}

/// The add of the record `db-1` naming `host_group` and `role` (either may be empty).
fn add_naming(host_group: &str, role: &str) -> proto::AddRecordRequest {
    proto::AddRecordRequest {
        host_id: "db-1".to_owned(),
        role: role.to_owned(),
        host_group: host_group.to_owned(),
        data_macs: vec![DB1_DATA.to_owned()],
        bmc_mac: DB1_BMC.to_owned(),
        ..proto::AddRecordRequest::default()
    }
}

/// The record `db-1` as it is stored, naming `host_group` and `role`, with no window.
fn stored(host_group: &str, role: &str) -> InventoryRecord {
    InventoryRecord {
        host_id: "db-1".to_owned(),
        role: role.to_owned(),
        host_group: host_group.to_owned(),
        data_macs: vec![DB1_DATA.to_owned()],
        bmc_mac: String::new(),
        serial: String::new(),
        notes: String::new(),
        by: "alice".to_owned(),
        at_unix: 1,
        approve_until_unix: 0,
        used: None,
        platform: None,
    }
}

/// Writes `record` straight into the ops state, as an earlier release's file would hold it.
fn seed(world: &World, record: InventoryRecord) {
    world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| state.inventory.push(record),
        )
        .expect("seeded");
}

/// The add of `request`, through the coordinator.
fn add(
    world: &World,
    request: proto::AddRecordRequest,
) -> Result<proto::AddRecordResponse, Refusal> {
    world.coordinator().add_record(&operator(), request)
}

/// The records `Enrollment.ListRecords` answers.
fn listed(world: &World) -> Vec<proto::InventoryRecord> {
    world
        .coordinator()
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed")
        .records
}

/// The audit reason of the request of `key` auto-approved into `group` as the record `db-1`.
fn auto_reason(key: &HostKey, group: &str) -> String {
    format!(
        "request {}; record db-1; group {group}; claims machine_id={machine} \
         addresses={DB1_DATA}; approved by its inventory record db-1",
        key.key_hash(),
        machine = "m-1",
    )
}

/// The request of `key` claiming the data MAC and the BMC MAC of `db-1`.
fn db1_request(key: &HostKey) -> EnrollRequest {
    claiming(key, "m-1", &[DB1_DATA], DB1_BMC)
}

/// Proves: from a coordinator that holds `web`, `AddRecord` naming `host_group: web` stores the
/// record with `web` as both its role and its host group and with the default approval window,
/// answers it as `web` in both, and audits `record_add` with the reason beginning `group web`.
///
/// Catches: a record kept with only a role, which a reader of `host_group` would take as in no
/// group.
#[test]
fn a_record_naming_a_host_group_keeps_it_as_role_and_group_with_a_window() {
    let world = grouped("rg-add-group");
    let before = world.audit().len();

    let answer = add(&world, add_naming("web", "")).expect("added");

    let record = answer.record.expect("a record");
    assert_eq!(
        (record.role.as_str(), record.host_group.as_str()),
        ("web", "web")
    );
    let ops = world.ops();
    let [kept] = ops.inventory.as_slice() else {
        panic!("one record: {:?}", ops.inventory);
    };
    assert_eq!(
        (kept.role.as_str(), kept.host_group.as_str()),
        ("web", "web")
    );
    assert_eq!(kept.approve_until_unix, UNIX + DEFAULT_APPROVE_FOR_SECS);
    let mut wanted = AuditEntry::new("operator", "record_add");
    wanted.seq = before as u64 + 1;
    wanted.node_id = Some("db-1".to_owned());
    wanted.reason = Some(format!(
        "group web; data {DB1_DATA}; bmc {DB1_BMC}; approve until {}",
        UNIX + DEFAULT_APPROVE_FOR_SECS
    ));
    assert_eq!(world.audit_after(before), [wanted]);
}

/// Proves: a role that names a held group, with no `host_group`, is read as the group: the stored
/// record, the answer and the audit are the ones of the same add naming `host_group`.
///
/// Catches: a client that names only a role losing its group when the coordinator holds groups.
#[test]
fn a_role_alone_is_read_as_the_group_it_names() {
    let by_role = grouped("rg-add-role");
    let by_group = grouped("rg-add-role-twin");

    let from_role = add(&by_role, add_naming("", "web")).expect("added");
    let from_group = add(&by_group, add_naming("web", "")).expect("added");

    assert_eq!(from_role.record, from_group.record);
    assert_eq!(by_role.ops().inventory, by_group.ops().inventory);
    assert_eq!(
        by_role.audit().pop().and_then(|entry| entry.reason),
        by_group.audit().pop().and_then(|entry| entry.reason)
    );
    let both = add(&by_group, add_naming("web", "web"));
    assert_eq!(
        both,
        Err(Refusal::RecordExists),
        "a role equal to the group is the same name"
    );
}

/// Proves: in group mode an add whose group is not held (`unknown_host_group`, by `host_group`
/// and by role), or whose role is another name than its `host_group` (`invalid_role`), or whose
/// role breaks the grammar (`invalid_role`), is refused with the inventory and the audit as they
/// were; a `host_group` that breaks the grammar names no group.
///
/// Catches: a record kept for a group nothing holds, which approval would then place nowhere.
#[test]
fn an_add_naming_no_held_group_or_two_names_is_refused_and_changes_nothing() {
    let world = grouped("rg-add-refused");
    let before = world.state();

    let cases = [
        (add_naming("ghost", ""), Refusal::UnknownHostGroup),
        (add_naming("", "ghost"), Refusal::UnknownHostGroup),
        (add_naming("Bad Name", ""), Refusal::UnknownHostGroup),
        (add_naming("web", "ghost"), Refusal::InvalidRole),
        (add_naming("", "Bad Role!"), Refusal::InvalidRole),
    ];
    for (request, refusal) in cases {
        assert_eq!(add(&world, request.clone()), Err(refusal), "{request:?}");
        assert_eq!(world.state(), before, "{request:?}");
    }
}

/// Proves: in group mode an add that names neither a group nor a role is kept with no window and
/// empty role and host group, answered so, and audited with no group in its reason.
#[test]
fn an_add_naming_no_group_in_group_mode_has_no_window() {
    let world = grouped("rg-add-none");
    let before = world.audit().len();

    let answer = add(&world, add_naming("", "")).expect("added");

    let record = answer.record.expect("a record");
    assert_eq!(
        (
            record.role.as_str(),
            record.host_group.as_str(),
            record.approve_until_unix
        ),
        ("", "", 0)
    );
    let ops = world.ops();
    let [kept] = ops.inventory.as_slice() else {
        panic!("one record: {:?}", ops.inventory);
    };
    assert_eq!(
        (
            kept.role.as_str(),
            kept.host_group.as_str(),
            kept.approve_until_unix
        ),
        ("", "", 0)
    );
    let audit = world.audit_after(before);
    let [entry] = audit.as_slice() else {
        panic!("one entry: {audit:?}");
    };
    assert_eq!(
        (entry.action.as_str(), entry.reason.as_deref()),
        (
            "record_add",
            Some(format!("data {DB1_DATA}; bmc {DB1_BMC}").as_str())
        )
    );
}

/// Proves: from a coordinator that holds no group, an add naming a `host_group` is refused
/// `unknown_host_group` with nothing written, and an add naming a role keeps the role as before,
/// with its window, and answers it with an empty `host_group`.
///
/// Catches: a coordinator before groups reading a role as a group, or keeping a group it cannot
/// place.
#[test]
fn a_coordinator_with_no_group_refuses_a_host_group_and_keeps_a_role() {
    let world = World::new("rg-add-legacy");
    let before = world.state();

    assert_eq!(
        add(&world, add_naming("web", "")),
        Err(Refusal::UnknownHostGroup)
    );
    assert_eq!(
        add(&world, add_naming("web", "storage")),
        Err(Refusal::UnknownHostGroup)
    );
    // The group is refused before the other fields are read, as before groups.
    let malformed = proto::AddRecordRequest {
        host_id: "Not A Host".to_owned(),
        ..add_naming("web", "")
    };
    assert_eq!(add(&world, malformed), Err(Refusal::UnknownHostGroup));
    assert_eq!(world.state(), before);

    let answer = add(&world, add_naming("", "storage")).expect("added");

    let record = answer.record.expect("a record");
    assert_eq!(
        (record.role.as_str(), record.host_group.as_str()),
        ("storage", "")
    );
    let ops = world.ops();
    let [kept] = ops.inventory.as_slice() else {
        panic!("one record: {:?}", ops.inventory);
    };
    assert_eq!(
        (
            kept.role.as_str(),
            kept.host_group.as_str(),
            kept.approve_until_unix
        ),
        ("storage", "", UNIX + DEFAULT_APPROVE_FOR_SECS)
    );
}

/// Proves: `ListRecords` from a coordinator that holds groups answers a record that names its
/// group only by role (an earlier release's) with that name as both `role` and `host_group`; from
/// one that holds none it answers the role as stored and an empty `host_group`.
///
/// Catches: a record of an earlier release listed with no group once the coordinator holds groups.
#[test]
fn a_listed_record_names_its_group_as_role_and_host_group_only_in_group_mode() {
    let world = grouped("rg-list");
    seed(&world, stored("", "web"));
    let shown = |world: &World| {
        let records = listed(world);
        let [record] = records.as_slice() else {
            panic!("one record: {records:?}");
        };
        (record.role.clone(), record.host_group.clone())
    };

    assert_eq!(shown(&world), ("web".to_owned(), "web".to_owned()));

    let legacy = World::new("rg-list-legacy");
    seed(&legacy, stored("", "web"));
    assert_eq!(shown(&legacy), ("web".to_owned(), String::new()));
}

/// Proves: a pending request posted before its record is approved into the record's group when the
/// record is added (`Coordinator::add_record`), for a record that names the group by `host_group`
/// and a request that claims its data MAC and BMC MAC: the host is a member of `web`, the audit
/// holds the exact `enrollment.auto_approved` entry with `host_group` `web` and no service
/// assignment, and the record is used.
///
/// Catches: an approval by record that places the host by role alone, in no group.
#[test]
fn adding_a_record_approves_the_pending_request_into_its_group() {
    let world = grouped("rg-approve-add");
    let key = HostKey::new();
    world
        .enrollment
        .enrollment
        .enroll(db1_request(&key))
        .expect("posted");
    world.enrollment.clock.set(UNIX);
    let before = world.audit().len();

    add(&world, add_naming("web", "")).expect("added");

    assert_eq!(
        world.membership("db-1"),
        Membership {
            host_group: "web".to_owned(),
            moving_from: None,
        }
    );
    let audit = world.audit_after(before);
    let actions: Vec<&str> = audit.iter().map(|entry| entry.action.as_str()).collect();
    assert_eq!(actions, ["record_add", "enrollment.auto_approved"]);
    let approved = &audit[1];
    assert_eq!(approved.by, "inventory");
    assert_eq!(approved.node_id.as_deref(), Some("db-1"));
    assert_eq!(approved.host_group.as_deref(), Some("web"));
    assert_eq!(
        approved.reason.as_deref(),
        Some(auto_reason(&key, "web").as_str())
    );
    let ops = world.ops();
    assert_eq!(ops.inventory.len(), 1);
    assert!(ops.inventory[0].used.is_some(), "the record is used");
}

/// Proves: a request that matches a record naming `web` only by role is approved into `web` when
/// it is posted through `Coordinator::enroll`, the answer says to poll, and the host is a member
/// of `web` with the exact audit entry.
///
/// Catches: a record that names its group by role alone approving nothing once groups are held.
#[test]
fn posting_a_request_that_matches_a_record_approves_it_into_the_group() {
    let world = grouped("rg-approve-post");
    world.enrollment.clock.set(UNIX);
    add(&world, add_naming("", "web")).expect("added");
    let key = HostKey::new();
    let before = world.audit().len();

    let answer = world
        .coordinator()
        .enroll(&world.enrollment.enrollment, db1_request(&key))
        .expect("posted");

    assert_eq!(answer.state, EnrollState::Unspecified as i32);
    assert_eq!(world.membership("db-1").host_group, "web");
    let audit = world.audit_after(before);
    let approved = audit
        .iter()
        .find(|entry| entry.action == "enrollment.auto_approved")
        .expect("auto approved");
    assert_eq!(approved.host_group.as_deref(), Some("web"));
    assert_eq!(
        approved.reason.as_deref(),
        Some(auto_reason(&key, "web").as_str())
    );
    assert!(world.ops().inventory[0].used.is_some());
}

/// Proves: in group mode a record that names by role a group the coordinator does not hold
/// (a record from before the groups) approves nothing: `approve_by_record` answers
/// `Waits::UnknownHostGroup`, the request stays pending, no host is enrolled and the audit is
/// unchanged.
///
/// Catches: a host approved into a group nothing holds.
#[test]
fn a_record_naming_a_group_not_held_leaves_the_request_pending() {
    let world = grouped("rg-by-record-unknown");
    let key = HostKey::new();
    let enrollment = &world.enrollment.enrollment;
    enrollment.enroll(db1_request(&key)).expect("posted");
    let mut record = stored("", "ghost");
    record.bmc_mac = DB1_BMC.to_owned();
    record.approve_until_unix = NOW + 3600;
    let before = world.state();

    let answer = enrollment.approve_by_record(&key.key_hash(), &[record]);

    assert_eq!(answer, ByRecord::Waits(Waits::UnknownHostGroup));
    assert_eq!(world.state(), before);
    let view = enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held");
    assert_eq!(
        view.state,
        hostkeeper_enrollment::enroll::RequestState::Pending
    );
}

/// Proves: `Enrollment.List` from a coordinator that holds groups shows the matched record's
/// group as both `record_role` and `record_host_group`, a record naming it by role alone too;
/// from one that holds none it shows the role and an empty `record_host_group`.
#[test]
fn the_pending_list_shows_the_matched_records_group() {
    let shown = |world: &World, record: InventoryRecord| {
        seed(world, record);
        let key = HostKey::new();
        world
            .enrollment
            .enrollment
            .enroll(claiming(&key, "m-1", &[DB1_DATA], ""))
            .expect("posted");
        let listed = world
            .coordinator()
            .enrollment_list(&operator(), &proto::ListPendingRequest::default())
            .expect("listed");
        let [pending] = listed.requests.as_slice() else {
            panic!("one request: {:?}", listed.requests);
        };
        (
            pending.inventory_match() == proto::InventoryMatch::Matched,
            pending.record_role.clone(),
            pending.record_host_group.clone(),
        )
    };

    let world = grouped("rg-list-pending");
    assert_eq!(
        shown(&world, stored("web", "web")),
        (true, "web".to_owned(), "web".to_owned())
    );
    let by_role = grouped("rg-list-pending-role");
    assert_eq!(
        shown(&by_role, stored("", "web")),
        (true, "web".to_owned(), "web".to_owned())
    );
    let legacy = World::new("rg-list-pending-legacy");
    assert_eq!(
        shown(&legacy, stored("", "storage")),
        (true, "storage".to_owned(), String::new())
    );
}

/// Proves: `OpenRecord` opens a record that names a group only by `host_group` (empty role, window
/// closed) for the time asked, and refuses `invalid_window` a record that names neither, leaving
/// the state as it was.
///
/// Catches: a window refused for want of a role on a record that names its group.
#[test]
fn opening_a_record_needs_a_group_not_a_role() {
    let world = grouped("rg-open");
    seed(&world, stored("web", ""));
    let open = proto::OpenRecordRequest {
        host_id: "db-1".to_owned(),
        approve_for_secs: 3600,
        ..proto::OpenRecordRequest::default()
    };

    let opened = world
        .coordinator()
        .open_record(&operator(), open.clone())
        .expect("opened");

    let record = opened.record.expect("a record");
    assert_eq!(
        (record.host_group.as_str(), record.approve_until_unix),
        ("web", UNIX + 3600)
    );
    assert_eq!(world.ops().inventory[0].approve_until_unix, UNIX + 3600);

    let bare = grouped("rg-open-none");
    seed(&bare, stored("", ""));
    let before = bare.state();
    assert_eq!(
        bare.coordinator().open_record(&operator(), open),
        Err(Refusal::InvalidWindow)
    );
    assert_eq!(bare.state(), before);
}

/// Proves: `HostGroups.Remove` refuses `not_empty` a group that only a record naming it by role
/// holds, with the state unchanged, and removes it once that record is removed
/// (`Enrollment.RemoveRecord`), with the exact audit entry.
///
/// Catches: a group removed from under a record that would then be approved into nothing.
#[test]
fn a_group_only_a_role_only_record_names_is_not_empty() {
    let world = grouped("rg-remove");
    seed(&world, stored("", "web"));
    let before = world.state();

    let refused = world
        .coordinator()
        .host_groups_remove(&operator(), remove_of("web"));

    assert_eq!(refused, Err(HostGroupsRefusal::NotEmpty));
    assert_eq!(world.state(), before);

    world
        .coordinator()
        .remove_record(
            &operator(),
            proto::RemoveRecordRequest {
                host_id: "db-1".to_owned(),
                reason: REASON.to_owned(),
            },
        )
        .expect("record removed");
    let audited = world.audit().len() as u64;
    world
        .coordinator()
        .host_groups_remove(&operator(), remove_of("web"))
        .expect("nothing names it now");

    assert_eq!(
        world.audit().pop(),
        Some(operator_entry(
            audited + 1,
            "host_group_remove",
            Some("web"),
            None,
            REASON
        ))
    );
}
