//! What `Mdm.ListDevices` adds to a device (the host id and group of its inventory record, the
//! baseline) and to the answer (whether the setup is configured and what is missing), and what
//! locks the MDM name.

use hostkeeper_mdm_gate::{CommandUuid, DeviceRecord, Phase, ProfileAnswer, ProfileCommand};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use crate::inventory::InventoryRecord;
use crate::login_support::World;
use crate::mdm::tests::{mdm_with, record};
use crate::mdm_secrets::MasterKey;
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::ops::{AuditEntry, Write};
use crate::testutil::scratch;
use hostkeeper_testkit::Scratch;

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

/// A world running a gate over `records`, and the directory of the gate, which outlives it.
fn gated(name: &str, records: &[DeviceRecord]) -> (World, Scratch) {
    let dir = scratch(&format!("{name}-gate"));
    let mdm = mdm_with(&dir, records);
    let world = World::build(
        name,
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| {
            c.with_mdm(Some(mdm)).with_mdm_setup(
                MdmSetup::volatile().with_key(MasterKey::from_bytes(&[0x21; 32]).expect("a key")),
            )
        },
    );
    (world, dir)
}

/// An inventory record of `host_id` in `group` with `serial`.
fn inventory(host_id: &str, group: &str, serial: &str) -> InventoryRecord {
    InventoryRecord {
        host_id: host_id.to_owned(),
        role: String::new(),
        host_group: group.to_owned(),
        data_macs: Vec::new(),
        bmc_mac: String::new(),
        serial: serial.to_owned(),
        notes: String::new(),
        by: "alice".to_owned(),
        at_unix: 1,
        approve_until_unix: 0,
        used: None,
        platform: None,
    }
}

fn keep(world: &World, records: Vec<InventoryRecord>) {
    world
        .coordinator
        .ops
        .change(
            Write::BeforeMemory,
            AuditEntry::new("alice", "test"),
            |state| {
                state.inventory = records;
            },
        )
        .expect("kept");
}

fn list(world: &World) -> proto::ListDevicesResponse {
    world
        .coordinator
        .mdm_list_devices(&alice(), &proto::ListDevicesRequest::default())
        .expect("listed")
}

fn rename(world: &World, name: &str) -> Result<proto::PutNameResponse, MdmRefusal> {
    world.coordinator.mdm_put_name(
        &alice(),
        &proto::PutNameRequest {
            name: name.to_owned(),
        },
    )
}

/// Proves: a device whose serial is the one of an inventory record, letters compared without case,
/// shows that record's host id and group; a device with no such record, with no serial, and one
/// whose serial two records hold (the ops file edited by hand) show neither.
#[test]
fn a_device_shows_the_host_id_and_group_of_its_inventory_record() {
    let (world, _dir) = gated(
        "mdm-devices-inventory",
        &[
            record("AAAA-1", Some("C02MATCH001"), Phase::Enrolled, Some(true)),
            record("BBBB-2", Some("C02NOBODY02"), Phase::Enrolled, Some(true)),
            record("CCCC-3", Some("C02TWICE003"), Phase::Enrolled, Some(true)),
            record("DDDD-4", None, Phase::Authenticated, None),
        ],
    );
    keep(
        &world,
        vec![
            inventory("lab-mac-1", "mac-builders", "c02match001"),
            inventory("lab-mac-2", "", "C02TWICE003"),
            inventory("lab-mac-3", "mac-builders", "c02twice003"),
            inventory("lab-mac-4", "mac-builders", ""),
        ],
    );
    let listed = list(&world);
    let shown: Vec<(&str, &str, &str)> = listed
        .devices
        .iter()
        .map(|d| {
            (
                d.enrollment_id.as_str(),
                d.host_id.as_str(),
                d.group.as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("AAAA-1", "lab-mac-1", "mac-builders"),
            ("BBBB-2", "", ""),
            ("CCCC-3", "", ""),
            ("DDDD-4", "", ""),
        ]
    );
}

/// Proves: a device's baseline is how far its config profile has got in this enrollment: none sent
/// is unspecified, sent and unanswered is pending, acknowledged is applied, and an error is
/// failed.
#[test]
fn a_devices_baseline_follows_its_config_profile() {
    let uuid = |n: u8| {
        CommandUuid::parse(&format!("{n:02}000000-0000-4000-8000-000000000000")).expect("a uuid")
    };
    let with = |id: &str, profile: Option<ProfileCommand>| {
        let mut device = record(id, Some("C02XK1ABJG5J"), Phase::Enrolled, Some(true));
        device.config_profile = profile;
        device
    };
    let answered = |n: u8, acknowledged: bool| {
        let mut sent = ProfileCommand::new(uuid(n), 100);
        sent.answer = Some(ProfileAnswer {
            acknowledged,
            at_unix: 200,
        });
        sent
    };
    let (world, _dir) = gated(
        "mdm-devices-baseline",
        &[
            with("A-NONE", None),
            with("B-PENDING", Some(ProfileCommand::new(uuid(1), 100))),
            with("C-APPLIED", Some(answered(2, true))),
            with("D-FAILED", Some(answered(3, false))),
        ],
    );
    let baselines: Vec<proto::MdmBaseline> =
        list(&world).devices.iter().map(|d| d.baseline()).collect();
    assert_eq!(
        baselines,
        [
            proto::MdmBaseline::Unspecified,
            proto::MdmBaseline::Pending,
            proto::MdmBaseline::Applied,
            proto::MdmBaseline::Failed,
        ]
    );
}

/// Proves: the answer says not configured and names what is missing before the setup is done and
/// still lists the devices, and says configured with nothing missing once the setup holds every
/// piece; a coordinator with no gate says the same of its setup with no device.
#[test]
fn the_list_says_whether_the_setup_is_configured_and_what_is_missing() {
    let (world, _dir) = gated(
        "mdm-devices-configured",
        &[record("AAAA-1", None, Phase::Enrolled, None)],
    );
    let listed = list(&world);
    assert_eq!((listed.devices.len(), listed.configured), (1, false));
    let missing: Vec<i32> = world
        .coordinator
        .mdm_status(&alice())
        .expect("a status")
        .missing;
    assert_eq!(listed.missing, missing);
    assert_eq!(missing.len(), 6);

    let ungated = World::build(
        "mdm-devices-no-gate",
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| c,
    );
    let listed = list(&ungated);
    assert_eq!(
        (
            listed.gate,
            listed.devices.len(),
            listed.configured,
            listed.missing.len()
        ),
        (false, 0, false, 6)
    );
}

/// Proves: the list is refused as every operator call is, and a page token that is no enrollment
/// id is refused before the setup is read.
#[test]
fn the_list_is_refused_to_a_caller_who_is_not_the_operator() {
    let (world, _dir) = gated("mdm-devices-refused", &[]);
    let call = |caller: &Caller, token: &str| {
        world.coordinator.mdm_list_devices(
            caller,
            &proto::ListDevicesRequest {
                limit: 0,
                page_token: token.to_owned(),
            },
        )
    };
    assert_eq!(call(&Caller::Anonymous, ""), Err(MdmRefusal::Unauthorized));
    assert_eq!(call(&Caller::Ci, ""), Err(MdmRefusal::Forbidden));
    assert_eq!(call(&alice(), "no/id"), Err(MdmRefusal::InvalidRequest));
}

/// Proves: once a Mac has enrolled the name cannot change: the first name is taken even then, the
/// same name is taken again, another is refused `name_locked` and audited with its reason, and the
/// status says the name is locked; a device that has only begun to enroll does not lock it.
#[test]
fn the_name_cannot_change_once_a_mac_has_enrolled() {
    let (world, _dir) = gated(
        "mdm-name-locked",
        &[record(
            "AAAA-1",
            Some("C02XK1ABJG5J"),
            Phase::Enrolled,
            Some(true),
        )],
    );
    assert!(
        !world
            .coordinator
            .mdm_status(&alice())
            .expect("a status")
            .name_locked
    );
    let first = rename(&world, "mdm.internal.example.com").expect("the first name is taken");
    assert_eq!(first.name, "mdm.internal.example.com");
    assert!(
        world
            .coordinator
            .mdm_status(&alice())
            .expect("a status")
            .name_locked
    );
    assert_eq!(
        rename(&world, " MDM.internal.example.com ")
            .expect("the same name")
            .name,
        "mdm.internal.example.com"
    );
    assert_eq!(
        rename(&world, "other.example.com"),
        Err(MdmRefusal::NameLocked)
    );
    let held = world.coordinator.mdm_status(&alice()).expect("a status");
    assert_eq!(held.name, "mdm.internal.example.com");
    let reasons: Vec<String> = world
        .audit()
        .into_iter()
        .map(|entry| format!("{}: {}", entry.action, entry.reason.unwrap_or_default()))
        .collect();
    assert_eq!(
        reasons,
        [
            "mdm_name_set: set to mdm.internal.example.com",
            "mdm_name_set: set to mdm.internal.example.com",
            "mdm_name_set: refused: MDM_REFUSAL_NAME_LOCKED for other.example.com",
        ]
    );

    let (early, _early_dir) = gated(
        "mdm-name-unlocked",
        &[record("AAAA-1", None, Phase::Authenticated, None)],
    );
    rename(&early, "mdm.internal.example.com").expect("named");
    assert!(
        !early
            .coordinator
            .mdm_status(&alice())
            .expect("a status")
            .name_locked
    );
    rename(&early, "other.example.com").expect("a device that has not enrolled does not lock it");
}

/// Proves: a setup store that cannot be read does not take the device list with it: the gate's
/// devices are listed, `configured` is false and no condition is named, so the Devices page keeps
/// its view while the store is down.
#[test]
fn the_list_keeps_its_devices_when_the_setup_cannot_be_read() {
    use crate::store::mdm_setup::files::FileSetup;
    use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};
    use std::sync::Arc;

    /// A store that takes writes and reads none.
    struct Down(FileSetup);
    impl SetupStore for Down {
        fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
            self.0.put_secret(row)
        }
        fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
            Err(SetupError::Store("unreadable".to_owned()))
        }
        fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
            self.0.prune_superseded(at)
        }
        fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
            Err(SetupError::Store("unreadable".to_owned()))
        }
        fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
            self.0.set_setting(name, value)
        }
    }
    let dir = scratch("mdm-devices-setup-down-gate");
    let mdm = mdm_with(&dir, &[record("AAAA-1", None, Phase::Enrolled, None)]);
    let world = World::build(
        "mdm-devices-setup-down",
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| {
            c.with_mdm(Some(mdm)).with_mdm_setup(MdmSetup::new(
                Arc::new(Down(FileSetup::volatile())),
                Some(MasterKey::from_bytes(&[0x21; 32]).expect("a key")),
                crate::enrollment::os_random(),
            ))
        },
    );
    assert_eq!(
        world.coordinator.mdm_status(&alice()),
        Err(MdmRefusal::StoreFailed)
    );
    let listed = list(&world);
    assert_eq!(
        (
            listed.gate,
            listed.devices.len(),
            listed.configured,
            listed.missing.len()
        ),
        (true, 1, false, 0)
    );
}
