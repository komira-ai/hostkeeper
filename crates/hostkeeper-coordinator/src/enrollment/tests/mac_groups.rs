//! The Mac allowlist by host group (`docs/design/hostgroups.md` sections 1.2 and 7.2): an entry
//! names a `host_group` (or the `role` it was written with, read as the group's name), a
//! coordinator that holds groups approves a Mac by its nonce into that group and assigns
//! nothing, refuses an entry that names services or a group it does not hold, keeps a group the
//! allowlist names from `HostGroups.Remove`, and holds a wiped Mac that rejoins to the group it
//! was moved to.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{Membership, RequestState};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use super::mac::{GOOD, MAC_HOST, gate_saying, mac_rig, state_of};
use super::support::{
    HostKey, NOW, Rig as EnrollmentRig, SERIAL, audit_after, audit_of, audited, mac_request,
    requested, signed_poll,
};
use crate::enrollment::{AllowlistError, DeviceStatus, MacAllowlist};
use crate::host_groups_support::{SELF, body, coordinator_on, move_of, put_of, remove_of};
use crate::marks_support::operator;
use crate::ops::AuditEntry;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::Rig;

/// The allowlist of [`SERIAL`] naming `group` through the key `key` and the extra lines `more`.
fn entry(key: &str, group: &str, more: &str) -> String {
    format!(
        "[[mac]]\nserial = \"{SERIAL}\"\nhost_id_prefix = \"mac-\"\n{key} = \"{group}\"\n{more}\n"
    )
}

/// The text of the refusal of the allowlist `text`.
fn refused(text: &str) -> String {
    match MacAllowlist::parse(text) {
        Err(error @ AllowlistError::Invalid(_)) => error.to_string(),
        other => panic!("the allowlist should be invalid, got {other:?}"),
    }
}

/// A coordinator over a fake store and the Mac enrollment `rig`.
struct Fleet {
    rig: Rig,
    fake: Fake,
    mac: EnrollmentRig,
}

impl Fleet {
    fn new(name: &str, allowlist: &str, gate: &Arc<crate::enrollment::FakeDeviceState>) -> Self {
        let mac = mac_rig(
            &format!("{name}-enrollment"),
            Arc::new(RealFs),
            gate,
            allowlist,
        );
        let fake = Fake::new();
        let mut rig = Rig::new(name);
        rig.coordinator = coordinator_on(&fake, &rig.clock, Some(Arc::clone(&mac.enrollment)));
        rig.at(2_000);
        Fleet { rig, fake, mac }
    }

    fn coordinator(&self) -> &Coordinator {
        &self.rig.coordinator
    }

    fn put(&self, group: &str) {
        self.coordinator()
            .host_groups_put(&operator(), put_of(body(group, &[SELF], None), None))
            .expect("put");
    }

    /// Posts `key`'s Mac request with a nonce issued for it: the answer's state.
    fn enroll(&self, key: &HostKey, machine: &str) -> i32 {
        let e = &self.mac.enrollment;
        let nonce = e.issue_mac_nonce(SERIAL, &key.key_hash()).expect("a nonce");
        self.coordinator()
            .enroll(e, mac_request(key, machine, SERIAL, nonce.as_bytes()))
            .expect("enrolled")
            .state
    }

    fn audit(&self) -> Vec<AuditEntry> {
        self.coordinator().ops().state().audit
    }

    fn membership(&self) -> Option<Membership> {
        let id = HostId::parse(MAC_HOST).expect("a host id");
        self.mac.enrollment.store().membership(&id).expect("read")
    }
}

/// The claims of a Mac request as the audit reason prints them.
fn claims(machine: &str) -> String {
    format!(
        "machine_id={machine} mac_serial={SERIAL} platform_uuid=4C4C4544-0042-5A10-8048-B7C04F4E3332"
    )
}

/// Proves: an entry naming `host_group` reads with that name as its role and `groups()` yields
/// it; an entry written with `role` alone reads the same; both keys naming one name are
/// accepted; `naming_services()` is the serial of the first entry that names services and
/// `None` when no entry does.
///
/// Catches: an entry written before groups that no longer reads, or a role and a group of one
/// name treated as two.
#[test]
fn an_entry_names_its_group_by_host_group_or_by_role() {
    let by_group = MacAllowlist::parse(&entry("host_group", "build", "")).expect("parsed");
    let by_role = MacAllowlist::parse(&entry("role", "build", "")).expect("parsed");
    let both =
        MacAllowlist::parse(&entry("host_group", "build", "role = \"build\"")).expect("parsed");
    for list in [&by_group, &by_role, &both] {
        let found = list.get(SERIAL).expect("the entry");
        assert_eq!(found.role.as_str(), "build");
        assert_eq!(found.host_id.as_str(), MAC_HOST);
        assert_eq!(list.groups().collect::<Vec<_>>(), ["build"]);
        assert_eq!(list.naming_services(), None);
    }
    assert_eq!(by_group, by_role);

    let services = MacAllowlist::parse(&format!(
        "{}\n[[mac]]\nserial = \"C02ZZZ\"\nhost_id_prefix = \"mac-\"\nhost_group = \"web\"\nservices = [\"kbf\"]\n",
        entry("host_group", "build", "")
    ))
    .expect("parsed");
    assert_eq!(services.naming_services(), Some("C02ZZZ"));
    assert_eq!(services.groups().collect::<Vec<_>>(), ["build", "web"]);
}

/// Proves: an entry naming a `host_group` and a different `role`, an entry naming neither, and
/// an entry whose name breaks the rule of a group's name (capital letters, a leading digit) are
/// each refused with the exact text the start prints.
///
/// Catches: an entry whose two names disagree resolved silently to one, or a name that no group
/// can have accepted and never matched.
#[test]
fn an_entry_that_names_no_group_or_two_or_a_bad_one_is_refused() {
    assert_eq!(
        refused(&entry("host_group", "build", "role = \"other\"")),
        format!(
            "--mac-serial-allowlist: the entry of serial {SERIAL} names host_group \"build\" and role \"other\""
        )
    );
    assert_eq!(
        refused(&entry("services", "", "").replace("services = \"\"\n", "")),
        format!("--mac-serial-allowlist: the entry of serial {SERIAL} names no host_group")
    );
    for bad in ["Build", "1st"] {
        assert_eq!(
            refused(&entry("host_group", bad, "")),
            format!("--mac-serial-allowlist: host group \"{bad}\" breaks its rule")
        );
        assert_eq!(
            refused(&entry("role", bad, "")),
            format!("--mac-serial-allowlist: host group \"{bad}\" breaks its rule")
        );
    }
}

/// Proves: a coordinator that holds the group `build` approves a Mac by its nonce into it: the
/// request is approved, the host's membership is `build`, the approval records `build` as its
/// group and role, the audit entry is `enroll_approve` by `mdm-nonce` about the host with the
/// group and the reason `request ..; group build; claims ..; approved by its MDM nonce`, and no
/// `assign` entry follows.
///
/// Catches: a Mac approved by the group's name as a role only, left in no group, or assigned
/// services.
#[test]
fn a_nonce_approval_puts_the_mac_in_its_group() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let fleet = Fleet::new(
        "mac-groups-approve",
        &entry("host_group", "build", ""),
        &gate,
    );
    fleet.put("build");
    let e = &fleet.mac.enrollment;
    let key = HostKey::new();
    let before = fleet.audit().len();

    let state = fleet.enroll(&key, "machine-mac");

    assert_eq!(state, EnrollState::Unspecified as i32);
    assert_eq!(state_of(e, &key), RequestState::Approved);
    assert_eq!(
        fleet.membership(),
        Some(Membership {
            host_group: "build".to_owned(),
            moving_from: None
        })
    );
    let recorded = e.store().recorded_approvals().expect("read");
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        (
            recorded[0].host_id.as_str(),
            recorded[0].role.as_str(),
            recorded[0].host_group.as_str(),
            recorded[0].approver.as_str()
        ),
        (MAC_HOST, "build", "build", "mdm-nonce")
    );
    assert!(recorded[0].services.is_empty());
    assert!(recorded[0].mac_nonce);
    let wanted = AuditEntry {
        seq: 0,
        by: "mdm-nonce".to_owned(),
        action: "enroll_approve".to_owned(),
        service: None,
        node_id: Some(MAC_HOST.to_owned()),
        host_group: Some("build".to_owned()),
        reason: Some(format!(
            "request {}; group build; claims {}; approved by its MDM nonce",
            key.key_hash(),
            claims("machine-mac")
        )),
        subject: None,
        source: None,
    };
    let entries: Vec<AuditEntry> = fleet
        .audit()
        .split_off(before)
        .into_iter()
        .map(|entry| AuditEntry { seq: 0, ..entry })
        .collect();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0].action, "enroll_requested");
    assert_eq!(entries[1], wanted);
    assert!(fleet.coordinator().ops().state().assignments.is_empty());
}

/// Proves: a coordinator that holds groups refuses a Mac whose entry names services
/// (`services_retired`) or a group it does not hold (`unknown_host_group`): the answer is
/// pending, the request stays pending, no host or membership is made, and the audit gains the
/// request and one `enroll_approve_refused` entry by `mdm-nonce` about the Mac's host id, with
/// the reason `<token>; request <hex>`.
///
/// Catches: an entry the coordinator cannot honour approving the Mac anyway into no group, or
/// leaving no trace of why it waits.
#[test]
fn an_entry_the_coordinator_cannot_honour_is_refused_and_the_mac_waits() {
    let cases = [
        (
            entry("host_group", "build", "services = [\"kbf\"]"),
            "services_retired",
        ),
        (entry("host_group", "ghost", ""), "unknown_host_group"),
    ];
    for (n, (allowlist, token)) in cases.into_iter().enumerate() {
        let gate = gate_saying(SERIAL, Ok(GOOD));
        let fleet = Fleet::new(&format!("mac-groups-refused-{n}"), &allowlist, &gate);
        fleet.put("build");
        let e = &fleet.mac.enrollment;
        let key = HostKey::new();
        let before = audit_of(fleet.coordinator()).len();
        let state_before = (fleet.fake.desired(), fleet.fake.ops());

        let state = fleet.enroll(&key, "machine-mac");

        assert_eq!(state, EnrollState::Pending as i32, "{token}");
        assert_eq!(state_of(e, &key), RequestState::Pending, "{token}");
        assert_eq!(
            audit_after(fleet.coordinator(), before),
            [
                requested(&key, None, "pending"),
                audited(
                    "mdm-nonce",
                    "enroll_approve_refused",
                    Some(MAC_HOST),
                    &format!("{token}; request {}", key.key_hash())
                ),
            ],
            "{token}"
        );
        assert_eq!(fleet.membership(), None, "{token}");
        assert!(e.store().recorded_approvals().expect("read").is_empty());
        assert_eq!(
            (fleet.fake.desired(), fleet.fake.ops().assignments),
            (state_before.0, state_before.1.assignments),
            "{token}"
        );
    }
}

/// Proves: `HostGroups.Remove` of a group only an allowlist entry names (no host, no record) is
/// `not_empty` and changes nothing, and a group no entry names is removed.
///
/// Catches: a group removed from under the allowlist, which would make the next Mac's approval
/// fail `unknown_host_group`.
#[test]
fn a_group_the_allowlist_names_is_not_removed() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let fleet = Fleet::new(
        "mac-groups-remove",
        &entry("host_group", "build", ""),
        &gate,
    );
    fleet.put("build");
    fleet.put("spare");
    let before = (fleet.fake.desired(), fleet.fake.ops());

    let refusal = fleet
        .coordinator()
        .host_groups_remove(&operator(), remove_of("build"))
        .expect_err("named by the allowlist");

    assert_eq!(refusal, Refusal::NotEmpty);
    assert_eq!((fleet.fake.desired(), fleet.fake.ops()), before);

    fleet
        .coordinator()
        .host_groups_remove(&operator(), remove_of("spare"))
        .expect("removed");
    let held = fleet.fake.desired();
    assert!(
        held.host_groups
            .0
            .keys()
            .map(ToString::to_string)
            .eq(["build".to_owned()])
    );
}

/// Proves: a Mac moved from `build` to `other` and then wiped, whose new key presents a fresh
/// nonce while the gate says its enrollment began after the first approval, is refused
/// `host_group_differs` (audited by `mdm-nonce` about its host id), its request stays pending,
/// and the host stays in `other` with its old key.
///
/// Catches: a wiped Mac that takes its host id back into the group the allowlist names, undoing
/// an operator's move.
#[test]
fn a_moved_mac_that_rejoins_is_held_to_its_group() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let fleet = Fleet::new(
        "mac-groups-rejoin",
        &entry("host_group", "build", ""),
        &gate,
    );
    fleet.put("build");
    fleet.put("other");
    let e = &fleet.mac.enrollment;
    let old = HostKey::new();
    assert_eq!(
        fleet.enroll(&old, "machine-old"),
        EnrollState::Unspecified as i32
    );
    fleet
        .coordinator()
        .host_groups_move(&operator(), move_of(MAC_HOST, "other"))
        .expect("moved");
    let moved = fleet.membership();
    assert_eq!(moved.as_ref().map(|m| m.host_group.as_str()), Some("other"));

    // The Mac is wiped: a new MDM enrollment, then a new key.
    fleet.mac.clock.set(NOW + 900);
    gate.set(
        SERIAL,
        Ok(DeviceStatus {
            enrolled_since_unix: Some(NOW + 1),
            enrolled_via_dep: None,
            ..GOOD
        }),
    );
    let new = HostKey::new();
    let before = fleet.audit().len();
    let state_before = (fleet.fake.desired(), fleet.fake.ops());

    let state = fleet.enroll(&new, "machine-new");

    assert_eq!(state, EnrollState::Pending as i32);
    assert_eq!(state_of(e, &new), RequestState::Pending);
    assert_eq!(
        audit_after(fleet.coordinator(), before),
        [
            requested(&new, None, "pending"),
            audited(
                "mdm-nonce",
                "enroll_approve_refused",
                Some(MAC_HOST),
                &format!("host_group_differs; request {}", new.key_hash())
            ),
        ]
    );
    assert_eq!(fleet.membership(), moved);
    let host = HostId::parse(MAC_HOST).expect("a host id");
    assert!(
        e.store()
            .host_role_for(&host, &old.key_hash())
            .expect("read")
            .is_some()
    );
    assert_eq!(
        e.store()
            .host_role_for(&host, &new.key_hash())
            .expect("read"),
        None
    );
    assert!(signed_poll(e, &old, &old).is_ok());
    assert_eq!(fleet.fake.desired(), state_before.0);
    assert_eq!(fleet.fake.ops().assignments, state_before.1.assignments);
}
