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
use crate::mdm::{DEVICES_DIR, Devices, Mdm, MdmFlags};
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
    record.enrolled_since_unix = (phase != Phase::Authenticated).then_some(NOW);
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
        mdm_profile_identifier: None,
        mdm_daemon_config: None,
        mdm_stores: None,
        mdm_macos_pkg: None,
        mdm_pkg_allowlist: None,
        mdm_host: None,
        mdm_listen: None,
        mdm_command_timeout_s: crate::mdm::DEFAULT_COMMAND_TIMEOUT_S,
        mdm_queue_sweep_s: crate::mdm::DEFAULT_QUEUE_SWEEP_S,
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
    Mdm::open(
        &flags_in(dir),
        Devices::Files(dir.path()),
        FixedClock::at(NOW),
        os_random(),
    )
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
            enrolled_since_unix: Some(NOW),
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
        let key = HostKey::new();
        let nonce = e.issue_mac_nonce(SERIAL, &key.key_hash()).expect("a nonce");
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

/// The real file system, but every file `read_private` opens is shown as root's, mode 0644: a
/// file of root's cannot be made without root.
struct RootFs;

impl hostkeeper_enrollment::fs::Fs for RootFs {
    fn read(&self, path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
        RealFs.read(path)
    }
    fn read_private(
        &self,
        path: &std::path::Path,
    ) -> std::io::Result<Option<(Vec<u8>, hostkeeper_enrollment::fs::FileStat)>> {
        let root = |(bytes, stat): (Vec<u8>, hostkeeper_enrollment::fs::FileStat)| {
            let stat = hostkeeper_enrollment::fs::FileStat {
                uid: 0,
                mode: 0o644,
                ..stat
            };
            (bytes, stat)
        };
        Ok(RealFs.read_private(path)?.map(root))
    }
    fn write_new(&self, path: &std::path::Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
        RealFs.write_new(path, bytes, mode)
    }
    fn sync_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        RealFs.sync_file(path)
    }
    fn rename(&self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        RealFs.rename(from, to)
    }
    fn remove(&self, path: &std::path::Path) -> std::io::Result<()> {
        RealFs.remove(path)
    }
    fn sync_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        RealFs.sync_dir(dir)
    }
    fn create_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        RealFs.create_dir(dir)
    }
    fn list(&self, dir: &std::path::Path) -> std::io::Result<Vec<String>> {
        RealFs.list(dir)
    }
    fn lock(&self, path: &std::path::Path) -> std::io::Result<hostkeeper_enrollment::fs::FileLock> {
        RealFs.lock(path)
    }
}

/// Catches: the pkg install built from `--mdm-pkg-allowlist` and `--mdm-host` with another
/// host or list than they name (the newest entry, the host in lower case), or built without
/// the two flags; a root-owned allowlist refused.
#[test]
fn the_pkg_flags_make_the_install_of_the_newest_entry() {
    let dir = scratch("mdm-pkg-install");
    let (a, b) = ("a".repeat(64), "b".repeat(64));
    let entry = |sha: &str, version: &str| {
        format!(
            "[[pkg]]\nsha256 = \"{sha}\"\nversion = \"{version}\"\npath = \"/var/lib/hostkeeper-mdm-pkg/{sha}.pkg\"\n"
        )
    };
    let list = dir.path().join("pkgs.toml");
    std::fs::write(&list, entry(&a, "0.1.9") + &entry(&b, "0.1.66")).expect("written");
    let flags = MdmFlags {
        mdm_pkg_allowlist: Some(list),
        mdm_host: Some("MDM.example.com".to_owned()),
        ..flags_in(&dir)
    };
    let install = super::install_of(&flags, Arc::new(RootFs))
        .expect("read")
        .expect("an install");
    assert_eq!(install.allowlist.newest().sha256.as_str(), b);
    assert_eq!(
        install.host.pkg_url(&install.allowlist.newest().sha256),
        format!("https://mdm.example.com/pkg/{b}.pkg")
    );
    assert!(
        super::install_of(&flags_in(&dir), Arc::new(RootFs))
            .expect("no flags")
            .is_none()
    );
}
