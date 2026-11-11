//! The Mac path from our ABM (`docs/design/coordinator-mdm.md`, 4.6): a Mac whose serial the
//! allowlist does not list is approved by its nonce when its serial is in the ABM table, the gate
//! says it is enrolled, supervised and enrolled through ADE, and a group resolves; each condition
//! has its own decline token, the inventory names the Mac and its group, the settings name the
//! rest, the allowlist overrides, and an operator's confirmation reads the table again.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::enroll::{MAC_NONCE_VALID_SECS, RequestState};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1::AddRecordRequest;

use super::mac::{GOOD, MAC_HOST, declined, gate_saying, mac_rig, state_of};
use super::support::{
    HostKey, NOW, Rig as EnrollmentRig, SERIAL, audit_after, audit_of, audited, mac_request,
    requested,
};
use crate::enrollment::{
    AbmDevices, AbmNaming, AbmUnavailable, ConfirmRefusal, DeviceStateError, DeviceStatus,
    FakeDeviceState, MacDecline, MacOutcome,
};
use crate::host_groups_support::{SELF, body, coordinator_on, put_of};
use crate::marks_support::operator;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::Rig;

/// What the ABM side says in these tests: the serials of its table, its settings, and whether
/// it can be read at all.
#[derive(Default)]
struct FakeAbm {
    table: Mutex<BTreeSet<String>>,
    naming: Mutex<Option<AbmNaming>>,
    table_down: Mutex<bool>,
    settings_down: Mutex<bool>,
}

impl FakeAbm {
    fn with(serials: &[&str], default_group: Option<&str>) -> Arc<FakeAbm> {
        let abm = FakeAbm::default();
        abm.table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(serials.iter().map(|s| (*s).to_owned()));
        abm.set_group(default_group);
        Arc::new(abm)
    }

    fn set_group(&self, default_group: Option<&str>) {
        let prefix = self
            .naming
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(|| "mac-".to_owned(), |n| n.host_id_prefix.clone());
        *self.naming.lock().unwrap_or_else(PoisonError::into_inner) = Some(AbmNaming {
            host_id_prefix: prefix,
            default_group: default_group.map(str::to_owned),
        });
    }

    fn set_prefix(&self, prefix: &str) {
        let mut naming = self.naming.lock().unwrap_or_else(PoisonError::into_inner);
        naming.as_mut().expect("naming set").host_id_prefix = prefix.to_owned();
    }

    fn remove(&self, serial: &str) {
        self.table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(serial);
    }

    fn break_table(&self) {
        *self
            .table_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
    }

    fn break_settings(&self) {
        *self
            .settings_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
    }
}

impl AbmDevices for FakeAbm {
    fn serial_in_abm(&self, serial: &str) -> Result<bool, AbmUnavailable> {
        if *self
            .table_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Err(AbmUnavailable("the table is down".to_owned()));
        }
        Ok(self
            .table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(serial))
    }

    fn naming(&self) -> Result<AbmNaming, AbmUnavailable> {
        if *self
            .settings_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Err(AbmUnavailable("the settings are down".to_owned()));
        }
        Ok(self
            .naming
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .expect("naming set"))
    }
}

/// A coordinator over a fake store and the Mac enrollment `mac`, which approves from `abm`.
struct World {
    rig: Rig,
    mac: EnrollmentRig,
    abm: Arc<FakeAbm>,
}

impl World {
    /// No allowlist, the groups `groups` held (none: the coordinator holds no group).
    fn new(name: &str, gate: &Arc<FakeDeviceState>, abm: &Arc<FakeAbm>, groups: &[&str]) -> World {
        World::listed(name, "", gate, abm, groups)
    }

    fn listed(
        name: &str,
        allowlist: &str,
        gate: &Arc<FakeDeviceState>,
        abm: &Arc<FakeAbm>,
        groups: &[&str],
    ) -> World {
        let mac = mac_rig(
            &format!("{name}-enrollment"),
            Arc::new(RealFs),
            gate,
            allowlist,
        );
        mac.enrollment.abm_by(abm.clone());
        let mut rig = Rig::new(name);
        rig.coordinator =
            coordinator_on(&Fake::new(), &rig.clock, Some(Arc::clone(&mac.enrollment)));
        rig.at(2_000);
        let world = World {
            rig,
            mac,
            abm: Arc::clone(abm),
        };
        for group in groups {
            world
                .coordinator()
                .host_groups_put(&operator(), put_of(body(group, &[SELF], None), None))
                .expect("put");
        }
        world
    }

    fn coordinator(&self) -> &Coordinator {
        &self.rig.coordinator
    }

    /// Records the Mac `serial` as `host`, in `group` (empty for none).
    fn record(&self, host: &str, group: &str, serial: &str) {
        self.coordinator()
            .add_record(
                &operator(),
                AddRecordRequest {
                    host_id: host.to_owned(),
                    host_group: group.to_owned(),
                    serial: serial.to_owned(),
                    ..AddRecordRequest::default()
                },
            )
            .expect("recorded");
    }

    /// Posts `key`'s request with a nonce issued for it, `after` seconds after the start.
    fn post(&self, key: &HostKey, after: u64) -> Vec<u8> {
        let e = &self.mac.enrollment;
        let nonce = e.issue_mac_nonce(SERIAL, &key.key_hash()).expect("a nonce");
        self.mac.clock.set(NOW + after);
        e.enroll(mac_request(key, "machine-mac", SERIAL, nonce.as_bytes()))
            .expect("enrolled");
        nonce.as_bytes().to_vec()
    }

    /// The approvals the store recorded, as `(host id, role, group, services, approver)`.
    fn approved(&self) -> Vec<(String, String, String, Vec<String>, String)> {
        self.mac
            .enrollment
            .store()
            .recorded_approvals()
            .expect("read")
            .into_iter()
            .map(|a| {
                (
                    a.host_id.as_str().to_owned(),
                    a.role.as_str().to_owned(),
                    a.host_group.as_str().to_owned(),
                    a.services.iter().map(ToString::to_string).collect(),
                    a.approver,
                )
            })
            .collect()
    }
}

/// The approval of a Mac as `host` in `group`, without services, by the nonce.
fn as_host(host: &str, group: &str) -> Vec<(String, String, String, Vec<String>, String)> {
    vec![(
        host.to_owned(),
        group.to_owned(),
        group.to_owned(),
        Vec::new(),
        "mdm-nonce".to_owned(),
    )]
}

/// Proves: the twelve decline tokens are exactly these, in the family of the built ones
/// (`nonce_*` for the nonce, `device_*` for what a device reports, `serial_*` for the list), and a
/// new case cannot be added without this test naming it.
///
/// Catches: a token renamed or dropped (an operator's audit search and the UI read these
/// strings), or a thirteenth added without a decision.
#[test]
fn the_twelve_decline_tokens_are_pinned() {
    use MacDecline::{
        DeviceNotEnrolled, DeviceNotSupervised, DeviceNotViaDep, DeviceStateUnavailable, NoGroup,
        NonceExpired, NonceUnknown, NonceUsed, NonceWrongRequest, NonceWrongSerial,
        SerialNotAllowed, SerialNotInAbm,
    };
    let all = [
        NonceUnknown,
        NonceExpired,
        NonceUsed,
        NonceWrongSerial,
        NonceWrongRequest,
        DeviceNotEnrolled,
        DeviceNotSupervised,
        DeviceNotViaDep,
        DeviceStateUnavailable,
        SerialNotAllowed,
        SerialNotInAbm,
        NoGroup,
    ];
    // No wildcard: a variant added to the enum stops this from compiling.
    let family = |why: MacDecline| match why {
        NonceUnknown | NonceExpired | NonceUsed | NonceWrongSerial | NonceWrongRequest => "nonce_",
        DeviceNotEnrolled | DeviceNotSupervised | DeviceNotViaDep | DeviceStateUnavailable => {
            "device_"
        }
        SerialNotAllowed | SerialNotInAbm => "serial_",
        NoGroup => "no_group",
    };
    assert_eq!(
        all.map(MacDecline::token),
        [
            "nonce_unknown",
            "nonce_expired",
            "nonce_used",
            "nonce_wrong_serial",
            "nonce_wrong_request",
            "device_not_enrolled",
            "device_not_supervised",
            "device_not_via_dep",
            "device_state_unavailable",
            "serial_not_allowed",
            "serial_not_in_abm",
            "no_group",
        ]
    );
    for why in all {
        assert!(why.token().starts_with(family(why)), "{}", why.token());
    }
}

/// One row of the table: what it changes from the Mac that is approved.
#[derive(Clone, Copy, Debug)]
enum Change {
    Nothing,
    NotInAbm,
    NotEnrolled,
    NotSupervised,
    NotViaDep,
    ViaDepUnknown,
    GateError,
    NoDefaultGroup,
    GroupNotHeld,
    NoGroupsHeld,
    UnknownNonce,
    Expired,
}

/// Proves: a Mac of our ABM that no allowlist entry lists is approved as `mac-<serial>` in the
/// default group with no service when every condition holds, and each one false leaves the
/// request pending with one audit entry of its own token (`serial_not_in_abm` for a Mac enrolled
/// by hand, `device_not_enrolled`, `device_not_supervised`, `device_not_via_dep` also for a Mac
/// that has not answered `SecurityInfo`, `device_state_unavailable`, `no_group` for no default
/// group, for one the coordinator does not hold and for a coordinator that holds none,
/// `nonce_unknown`, `nonce_expired`).
///
/// Catches: any one condition dropped, a manual enrollment of a serial outside the ABM approved
/// by its nonce, and a Mac approved into no group.
#[tokio::test]
async fn a_mac_of_our_abm_is_approved_only_when_every_condition_holds() {
    let rows = [
        (Change::Nothing, None),
        (Change::NotInAbm, Some(MacDecline::SerialNotInAbm)),
        (Change::NotEnrolled, Some(MacDecline::DeviceNotEnrolled)),
        (Change::NotSupervised, Some(MacDecline::DeviceNotSupervised)),
        (Change::NotViaDep, Some(MacDecline::DeviceNotViaDep)),
        (Change::ViaDepUnknown, Some(MacDecline::DeviceNotViaDep)),
        (Change::GateError, Some(MacDecline::DeviceStateUnavailable)),
        (Change::NoDefaultGroup, Some(MacDecline::NoGroup)),
        (Change::GroupNotHeld, Some(MacDecline::NoGroup)),
        (Change::NoGroupsHeld, Some(MacDecline::NoGroup)),
        (Change::UnknownNonce, Some(MacDecline::NonceUnknown)),
        (Change::Expired, Some(MacDecline::NonceExpired)),
    ];
    for (n, (change, decline)) in rows.into_iter().enumerate() {
        let status = match change {
            Change::NotEnrolled => Ok(DeviceStatus {
                enrolled: false,
                ..GOOD
            }),
            Change::NotSupervised => Ok(DeviceStatus {
                supervised: false,
                ..GOOD
            }),
            Change::NotViaDep => Ok(DeviceStatus {
                enrolled_via_dep: Some(false),
                ..GOOD
            }),
            Change::ViaDepUnknown => Ok(DeviceStatus {
                enrolled_via_dep: None,
                ..GOOD
            }),
            Change::GateError => Err(DeviceStateError("the gate is down".to_owned())),
            _ => Ok(GOOD),
        };
        let gate = gate_saying(SERIAL, status);
        let table: &[&str] = match change {
            Change::NotInAbm => &["C02OTHER0001"],
            _ => &[SERIAL],
        };
        let group = match change {
            Change::NoDefaultGroup => None,
            Change::GroupNotHeld => Some("ghost"),
            _ => Some("build"),
        };
        let held: &[&str] = match change {
            Change::NoGroupsHeld => &[],
            _ => &["build"],
        };
        let world = World::new(
            &format!("mac-abm-row-{n}"),
            &gate,
            &FakeAbm::with(table, group),
            held,
        );
        let e = &world.mac.enrollment;
        let key = HostKey::new();
        let before = audit_of(world.coordinator()).len();
        let offset = match change {
            Change::Expired => MAC_NONCE_VALID_SECS + 1,
            _ => 10,
        };
        let nonce = e.issue_mac_nonce(SERIAL, &key.key_hash()).expect("a nonce");
        let presented = match change {
            Change::UnknownNonce => b"a nonce the gate never issued".to_vec(),
            _ => nonce.as_bytes().to_vec(),
        };
        world.mac.clock.set(NOW + offset);
        let answer = e
            .enroll(mac_request(&key, "machine-mac", SERIAL, &presented))
            .expect("enrolled");
        match decline {
            None => {
                assert_eq!(answer.state, EnrollState::Unspecified as i32, "{change:?}");
                assert_eq!(state_of(e, &key), RequestState::Approved);
                assert_eq!(world.approved(), as_host(MAC_HOST, "build"), "{change:?}");
            }
            Some(why) => {
                assert_eq!(answer.state, EnrollState::Pending as i32, "{change:?}");
                assert_eq!(state_of(e, &key), RequestState::Pending, "{change:?}");
                assert!(world.approved().is_empty(), "{change:?}");
                assert_eq!(
                    audit_after(world.coordinator(), before),
                    [requested(&key, None, "pending"), declined(why, &key)],
                    "{change:?}"
                );
            }
        }
    }
}

/// Proves: the approval audit of a Mac of our ABM is `enroll_approve` by `mdm-nonce` about the
/// Mac's host id, in its group, with the same reason text as an allowlisted Mac's, and the
/// coordinator assigns no service.
///
/// Catches: an ABM approval that leaves no trace of the group it chose, or that assigns services.
#[tokio::test]
async fn a_mac_of_our_abm_is_audited_as_approved_in_its_group_with_no_service() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let world = World::new(
        "mac-abm-audit",
        &gate,
        &FakeAbm::with(&[SERIAL], Some("build")),
        &["build"],
    );
    let key = HostKey::new();
    let before = audit_of(world.coordinator()).len();
    world.post(&key, 10);
    assert_eq!(
        state_of(&world.mac.enrollment, &key),
        RequestState::Approved
    );
    let entries = audit_after(world.coordinator(), before);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0], requested(&key, None, "pending"));
    assert_eq!(
        entries[1],
        audited(
            "mdm-nonce",
            "enroll_approve",
            Some(MAC_HOST),
            &format!(
                "request {}; group build; claims machine_id=machine-mac mac_serial={SERIAL} \
                 platform_uuid=4C4C4544-0042-5A10-8048-B7C04F4E3332; approved by its MDM nonce",
                key.key_hash()
            )
        )
    );
    assert!(world.coordinator().ops().state().assignments.is_empty());
}

/// Proves: the inventory record of the serial (matched without case) names the Mac and its group:
/// its `host_id` and `host_group` win over the settings; a record that names no group gives only
/// the host id and the default group is its group; a Mac with no record gets the `host_id_prefix`
/// setting and the serial in lower case.
///
/// Catches: a Mac named by the settings while the operator recorded it, a group taken from the
/// default while the record names one, and a prefix that is not read from the setting.
#[tokio::test]
async fn the_inventory_names_the_mac_and_its_group_and_the_settings_name_the_rest() {
    let cases = [
        (Some(("lab-m3", "web")), "lab-m3", "web"),
        (Some(("lab-m3", "")), "lab-m3", "build"),
        (None, "lab-c02xk1abjg5j", "build"),
    ];
    for (n, (record, host, group)) in cases.into_iter().enumerate() {
        let gate = gate_saying(SERIAL, Ok(GOOD));
        let abm = FakeAbm::with(&[SERIAL], Some("build"));
        abm.set_prefix("lab-");
        let world = World::new(
            &format!("mac-abm-inventory-{n}"),
            &gate,
            &abm,
            &["build", "web"],
        );
        // A record that holds no serial is no record of this Mac.
        world
            .coordinator()
            .add_record(
                &operator(),
                AddRecordRequest {
                    host_id: "storage-1".to_owned(),
                    data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
                    ..AddRecordRequest::default()
                },
            )
            .expect("recorded");
        if let Some((record_host, record_group)) = record {
            world.record(record_host, record_group, &SERIAL.to_ascii_lowercase());
        }
        let key = HostKey::new();
        world.post(&key, 10);
        assert_eq!(world.approved(), as_host(host, group), "case {n}");
    }
}

/// Proves: an allowlist entry overrides: its serial is approved as the entry says (host id,
/// group) though it is not in the ABM table and a record names it otherwise, and a Mac not
/// enrolled through ADE is approved as before when the coordinator is not set to require it.
///
/// Catches: the override dropped by the ABM path, or the ADE requirement applied to the
/// allowlist.
#[tokio::test]
async fn the_allowlist_overrides_the_abm_table_the_inventory_and_the_settings() {
    let listed =
        "[[mac]]\nserial = \"C02XK1ABJG5J\"\nhost_id_prefix = \"mac-\"\nhost_group = \"build\"\n";
    let gate = gate_saying(
        SERIAL,
        Ok(DeviceStatus {
            enrolled_via_dep: None,
            ..GOOD
        }),
    );
    let abm = FakeAbm::with(&[], Some("web"));
    let world = World::listed("mac-abm-override", listed, &gate, &abm, &["build", "web"]);
    world.record("lab-m3", "web", SERIAL);
    let key = HostKey::new();
    world.post(&key, 10);
    assert_eq!(world.approved(), as_host(MAC_HOST, "build"));
}

/// Proves: a coordinator with no ABM side declines a serial the allowlist does not list with the
/// old token `serial_not_allowed`, so a deployment without MDM behaves as before.
///
/// Catches: a new token answering a coordinator that cannot know the new case.
#[tokio::test]
async fn without_an_abm_side_an_unlisted_serial_is_serial_not_allowed() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let mac = mac_rig("mac-abm-none-enrollment", Arc::new(RealFs), &gate, "");
    let mut rig = Rig::new("mac-abm-none");
    rig.coordinator = coordinator_on(&Fake::new(), &rig.clock, Some(Arc::clone(&mac.enrollment)));
    rig.at(2_000);
    let key = HostKey::new();
    let nonce = mac
        .enrollment
        .issue_mac_nonce(SERIAL, &key.key_hash())
        .expect("a nonce");
    mac.enrollment
        .enroll(mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes()))
        .expect("enrolled");
    assert_eq!(
        mac.enrollment
            .mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
        MacOutcome::Declined(MacDecline::SerialNotAllowed)
    );
    assert_eq!(state_of(&mac.enrollment, &key), RequestState::Pending);
}

/// Proves: a table or a settings store that cannot be read leaves the request pending, refused
/// `store_failed` and audited, never declined as a Mac that is not ours and never approved.
///
/// Catches: a store fault read as "not in ABM", which would hide it as an ordinary decline, or as
/// "in ABM".
#[tokio::test]
async fn an_unreadable_table_or_settings_store_refuses_and_approves_nothing() {
    for (n, break_table) in [true, false].into_iter().enumerate() {
        let gate = gate_saying(SERIAL, Ok(GOOD));
        let abm = FakeAbm::with(&[SERIAL], Some("build"));
        let world = World::new(&format!("mac-abm-down-{n}"), &gate, &abm, &["build"]);
        if break_table {
            abm.break_table();
        } else {
            abm.break_settings();
        }
        let key = HostKey::new();
        let before = audit_of(world.coordinator()).len();
        world.post(&key, 10);
        assert_eq!(state_of(&world.mac.enrollment, &key), RequestState::Pending);
        assert!(world.approved().is_empty());
        assert_eq!(
            audit_after(world.coordinator(), before),
            [
                requested(&key, None, "pending"),
                audited(
                    "mdm-nonce",
                    "enroll_approve_refused",
                    None,
                    &format!("store_failed; request {}", key.key_hash())
                ),
            ]
        );
    }
}

/// Proves: a declined attempt does not consume the nonce: declined `no_group`, the same nonce
/// approves once the default group is set.
///
/// Catches: a nonce burnt by a decline for a reason an operator fixes in the settings.
#[tokio::test]
async fn a_mac_declined_no_group_is_approved_by_the_same_nonce_once_a_group_is_set() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let abm = FakeAbm::with(&[SERIAL], None);
    let world = World::new("mac-abm-burn", &gate, &abm, &["build"]);
    let key = HostKey::new();
    let nonce = world.post(&key, 10);
    let e = &world.mac.enrollment;
    assert_eq!(state_of(e, &key), RequestState::Pending);
    abm.set_group(Some("build"));
    let again = e
        .enroll(mac_request(&key, "machine-mac", SERIAL, &nonce))
        .expect("posted again");
    assert_eq!(again.state, EnrollState::Unspecified as i32);
    assert_eq!(world.approved(), as_host(MAC_HOST, "build"));
}

/// Proves: the confirmation of an operator reads the table again: a serial in the table is
/// confirmed; the same serial removed from the table since the Mac asked is refused with
/// `serial_not_in_abm`; an unreadable table is a different answer, and a coordinator with no ABM
/// side confirms nothing.
///
/// Catches: a challenge issued to a serial that ABM disowned while it waited for the click.
#[tokio::test]
async fn an_operators_confirmation_reads_the_abm_table_again() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let abm = FakeAbm::with(&[SERIAL], Some("build"));
    let world = World::new("mac-abm-confirm", &gate, &abm, &["build"]);
    let e = &world.mac.enrollment;
    assert_eq!(e.confirm_in_abm(SERIAL), Ok(()));
    world.abm.remove(SERIAL);
    assert_eq!(
        e.confirm_in_abm(SERIAL),
        Err(ConfirmRefusal::Declined(MacDecline::SerialNotInAbm))
    );
    abm.break_table();
    assert_eq!(e.confirm_in_abm(SERIAL), Err(ConfirmRefusal::Unread));
    let bare = mac_rig("mac-abm-confirm-bare", Arc::new(RealFs), &gate, "");
    assert_eq!(
        bare.enrollment.confirm_in_abm(SERIAL),
        Err(ConfirmRefusal::Declined(MacDecline::SerialNotInAbm))
    );
}

/// Proves: a `host_id_prefix` that makes no host id (a store edited by hand, which the setting
/// refuses) leaves the request pending, refused `invalid_host_id` and audited, and approves
/// nothing.
///
/// Catches: a Mac approved under a host id the store would not hold, or one that waits with no
/// trace of why.
#[tokio::test]
async fn a_prefix_that_makes_no_host_id_refuses_the_mac_and_says_so() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let abm = FakeAbm::with(&[SERIAL], Some("build"));
    abm.set_prefix("Not A Prefix ");
    let world = World::new("mac-abm-bad-prefix", &gate, &abm, &["build"]);
    let key = HostKey::new();
    let before = audit_of(world.coordinator()).len();
    world.post(&key, 10);
    assert_eq!(state_of(&world.mac.enrollment, &key), RequestState::Pending);
    assert!(world.approved().is_empty());
    assert_eq!(
        audit_after(world.coordinator(), before),
        [
            requested(&key, None, "pending"),
            audited(
                "mdm-nonce",
                "enroll_approve_refused",
                None,
                &format!("invalid_host_id; request {}", key.key_hash())
            ),
        ]
    );
}
