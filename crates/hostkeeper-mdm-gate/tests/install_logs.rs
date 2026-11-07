//! The log lines of the gate's install of the pkg, in a binary of its own: a test
//! subscriber hears only the events of callsites first seen while it is set, so no other
//! test may run in this process.

mod support;

use std::path::PathBuf;

use hostkeeper_mdm_gate::PkgDigest;
use hostkeeper_mdm_gate::install::INSTALL_RESEND_SECS;
use support::enrollment::EnrollRig;
use support::{SERIAL, UDID, result};

const PKG: &[u8] = b"the bytes of a hostkeeper pkg, version 0.1.66";
const INSTALL: &str = "InstallEnterpriseApplication";

fn put(rig: &EnrollRig, digest: &PkgDigest, bytes: &[u8]) -> PathBuf {
    let dir = rig.rig.scratch.path().join("pkg");
    std::fs::create_dir_all(&dir).expect("the pkg directory");
    let path = dir.join(digest.file_name());
    std::fs::write(&path, bytes).expect("the pkg file");
    path
}

async fn answer_last(rig: &EnrollRig, kind: &str, status: &str, now: u64) {
    rig.clock().set(now);
    let uuid = rig.last_uuid(kind);
    rig.rig.post(&result(UDID, Some(&uuid), status, &[])).await;
}

async fn look(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.gate().tick().await;
}

/// Catches: a send not logged with the device, the serial, the digest and its count, or a pkg
/// held back with no warning naming why; any other line at info or above.
#[tokio::test]
async fn each_send_is_logged_with_its_count_and_a_held_pkg_with_its_reason() {
    let (_guard, logs) = support::capture_logs();
    let mut rig = EnrollRig::new("install-logs").await;
    let digest = PkgDigest::of(PKG);
    let path = put(&rig, &digest, PKG);
    rig.install(&format!(
        "[[pkg]]\nsha256 = \"{digest}\"\nversion = \"0.1.66\"\npath = \"{}\"\n",
        path.display()
    ));
    rig.fake.allow(SERIAL);
    rig.enroll(SERIAL, true).await;
    let before = logs.events().len();
    answer_last(&rig, "InstallProfile", "Acknowledged", 1_005).await;
    let sent: support::LogEvent = [
        ("level", "INFO".to_owned()),
        ("message", "sent the pkg".to_owned()),
        ("enrollment_id", UDID.to_owned()),
        ("serial", format!("{SERIAL:?}")),
        ("sha256", digest.to_string()),
        ("send", "1".to_owned()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    assert_eq!(logs.events()[before..], [sent]);

    put(&rig, &digest, b"other bytes");
    answer_last(&rig, INSTALL, "Acknowledged", 1_006).await;
    look(&rig, 1_006 + INSTALL_RESEND_SECS).await;
    let why = format!(
        "the pkg {}'s bytes are {}, not its listed sha256; it is not sent",
        path.display(),
        PkgDigest::of(b"other bytes")
    );
    let held: support::LogEvent = [
        ("level", "WARN".to_owned()),
        ("message", "the pkg is not sent".to_owned()),
        ("enrollment_id", UDID.to_owned()),
        ("why", why),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    assert_eq!(logs.events()[before + 1..], [held]);
}
