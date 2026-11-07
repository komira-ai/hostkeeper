//! The coordinator's hold of the MDM policy gate: the gate it opens over its records, the
//! coordinator's [`DeviceState`] port answered from them, and the Mac path through that port.
//! The records are written with the gate's own store; no event is posted and NanoHUB is not
//! called, so the API URL names a loopback port nothing listens on.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_mdm_gate::{DeviceRecord, DeviceStore, EnrollmentId, FileDeviceStore, Phase};
use hostkeeper_testkit::Scratch;

use crate::enrollment::tests::support::{FixedClock, HostKey, NOW, Rig, SERIAL, mac_request};
use crate::enrollment::{
    DeviceStateError, DeviceStatus, MacAllowlist, MacDecline, MacOutcome, MacPolicy, os_random,
};
use crate::mdm::{DEVICES_DIR, Mdm, MdmFlags};
use crate::testutil::scratch;

/// The allowlist entry of [`SERIAL`]: host id `mac-<serial>`, role `build`, service `kbf`.
const ALLOWED: &str = r#"
[[mac]]
serial = "C02XK1ABJG5J"
host_id_prefix = "mac-"
role = "build"
services = ["kbf"]
"#;

/// A record of the enrollment `id` in `phase`, whose `DeviceInformation` answer gave `serial`
/// and `supervised`.
pub(crate) fn record(
    id: &str,
    serial: Option<&str>,
    phase: Phase,
    supervised: Option<bool>,
) -> DeviceRecord {
    let mut record = DeviceRecord::new(EnrollmentId::parse(id).expect("an id"), NOW);
    record.udid = id.to_owned();
    record.serial = serial.map(str::to_owned);
    record.phase = phase;
    record.supervised = supervised;
    record
}

/// The URL of a loopback port that was free a moment ago and has no listener.
fn unused_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    format!("http://127.0.0.1:{port}")
}

/// The gate flags over `dir`: a key file there, an API nothing listens on, and a webhook
/// address.
pub(crate) fn flags_in(dir: &Scratch) -> MdmFlags {
    let key = dir.path().join("api.key");
    std::fs::write(&key, "a-nanohub-api-key-for-the-tests\n").expect("the key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("mode");
    MdmFlags {
        mdm_api_url: Some(unused_url()),
        mdm_api_key_file: Some(key),
        mdm_webhook_listen: Some("127.0.0.1:9100".parse().expect("an address")),
    }
}

/// The directory of the gate's records under the enrollment directory `dir`.
pub(crate) fn devices_dir(dir: &Scratch) -> PathBuf {
    dir.path().join(DEVICES_DIR)
}

/// A gate over `records`, written to the store under `dir` before it opens.
pub(crate) fn mdm_with(dir: &Scratch, records: &[DeviceRecord]) -> Mdm {
    let store = FileDeviceStore::open(Arc::new(RealFs), &devices_dir(dir)).expect("a store");
    for record in records {
        store.save(record).expect("saved");
    }
    Mdm::open(&flags_in(dir), dir.path(), FixedClock::at(NOW), os_random())
        .expect("the gate opens")
        .expect("the flags name a gate")
}

/// The device status the gate says of `serial` through the coordinator's port.
fn said(mdm: &Mdm, serial: &str) -> Result<DeviceStatus, DeviceStateError> {
    Mdm::device_state(Some(mdm)).status(serial)
}

/// The state the store holds `key`'s request in.
fn state_of(rig: &Rig, key: &HostKey) -> RequestState {
    rig.enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state
}

/// Catches: a coordinator without the gate that answers a status for some serial, or an error
/// text other than the one the Mac path audits.
#[test]
fn without_a_gate_every_serial_is_unknown() {
    let port = Mdm::device_state(None);
    let why = port.status(SERIAL).expect_err("no gate, no device");
    assert_eq!(
        why.to_string(),
        "no device state: no device-state gate is configured"
    );
}

/// Catches: the port copying the gate's enrolled and supervised flags wrongly (swapped, or
/// supervision read from a device that has not answered), a checked-out device counted
/// enrolled, a re-enrolled device answered from its old record, an unknown serial answered, and
/// the gate's reason changed on its way to the Mac path.
#[test]
fn the_port_answers_from_the_gates_records() {
    let dir = scratch("mdm-port");
    let mdm = mdm_with(
        &dir,
        &[
            record("AAAA-1", Some("GOODSERIAL01"), Phase::Enrolled, Some(true)),
            record("AAAA-2", Some("LOOSESERIAL1"), Phase::Enrolled, Some(false)),
            record("AAAA-3", Some("SILENTSERIAL"), Phase::Enrolled, None),
            record(
                "AAAA-4",
                Some("GONESERIAL01"),
                Phase::CheckedOut,
                Some(true),
            ),
            record(
                "AAAA-5",
                Some("TWICESERIAL1"),
                Phase::CheckedOut,
                Some(false),
            ),
            record("AAAA-6", Some("TWICESERIAL1"), Phase::Enrolled, Some(true)),
            record("AAAA-7", Some("DUPSERIAL001"), Phase::Enrolled, Some(true)),
            record("AAAA-8", Some("DUPSERIAL001"), Phase::Enrolled, Some(true)),
        ],
    );
    let status = |enrolled, supervised| {
        Ok(DeviceStatus {
            enrolled,
            supervised,
        })
    };
    assert_eq!(said(&mdm, "GOODSERIAL01"), status(true, true));
    assert_eq!(said(&mdm, "LOOSESERIAL1"), status(true, false));
    assert_eq!(said(&mdm, "SILENTSERIAL"), status(true, false));
    assert_eq!(said(&mdm, "GONESERIAL01"), status(false, true));
    assert_eq!(said(&mdm, "TWICESERIAL1"), status(true, true));
    assert_eq!(
        said(&mdm, "NOSUCHSERIAL").expect_err("unknown").to_string(),
        "no device state: the gate knows no device with this serial"
    );
    assert_eq!(
        said(&mdm, "DUPSERIAL001")
            .expect_err("ambiguous")
            .to_string(),
        "no device state: the serial is the device of more than one enrollment"
    );
}

/// A case of the Mac path: its name, the phase and supervision of each enrollment the gate
/// holds with the Mac's serial, and the decline expected, if any.
type Row = (&'static str, Vec<(Phase, Option<bool>)>, Option<MacDecline>);

/// Catches: the Mac path approving on a gate answer it should not, or declining with another
/// token: through the real gate, a device that is enrolled and supervised approves; one not
/// supervised, not yet answered, checked out, unknown, or the serial of two enrollments does
/// not, and each names its own decline; the declined request stays pending.
#[test]
fn the_mac_path_approves_only_an_enrolled_and_supervised_device_of_the_gate() {
    let rows: [Row; 7] = [
        ("approved", vec![(Phase::Enrolled, Some(true))], None),
        (
            "unsupervised",
            vec![(Phase::Enrolled, Some(false))],
            Some(MacDecline::DeviceNotSupervised),
        ),
        (
            "unanswered",
            vec![(Phase::Enrolled, None)],
            Some(MacDecline::DeviceNotSupervised),
        ),
        (
            "checked-out",
            vec![(Phase::CheckedOut, Some(true))],
            Some(MacDecline::DeviceNotEnrolled),
        ),
        ("unknown", vec![], Some(MacDecline::DeviceStateUnavailable)),
        (
            "ambiguous",
            vec![(Phase::Enrolled, Some(true)), (Phase::Enrolled, Some(true))],
            Some(MacDecline::DeviceStateUnavailable),
        ),
        (
            "re-enrolled",
            vec![
                (Phase::CheckedOut, Some(false)),
                (Phase::Enrolled, Some(true)),
            ],
            None,
        ),
    ];
    for (name, devices, decline) in rows {
        let dir = scratch(&format!("mdm-mac-{name}"));
        let records: Vec<DeviceRecord> = devices
            .into_iter()
            .enumerate()
            .map(|(n, (phase, supervised))| {
                record(&format!("DEVICE-{n}"), Some(SERIAL), phase, supervised)
            })
            .collect();
        let mdm = mdm_with(&dir, &records);
        let policy = MacPolicy {
            device_state: Mdm::device_state(Some(&mdm)),
            allowlist: MacAllowlist::parse(ALLOWED).expect("an allowlist"),
        };
        let rig = Rig::configured(
            &format!("mdm-mac-rig-{name}"),
            Arc::new(RealFs),
            os_random(),
            |enrollment| enrollment.with_mac(policy),
        );
        let e = &rig.enrollment;
        let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
        let key = HostKey::new();
        e.enroll(mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes()))
            .expect("enrolled");
        match decline {
            None => assert_eq!(state_of(&rig, &key), RequestState::Approved, "{name}"),
            Some(why) => {
                assert_eq!(state_of(&rig, &key), RequestState::Pending, "{name}");
                assert_eq!(
                    e.mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
                    MacOutcome::Declined(why),
                    "{name}"
                );
            }
        }
    }
}
