//! No byte of a nonce reaches a log or the audit (design section 10, task M12): the whole log
//! and the whole audit of an issue, a failed push, its re-push and the removal, compared by
//! equality. The one test of this binary, so its subscriber hears no other test's events.

mod support;

use hostkeeper_mdm_gate::enroll::{ACTION_NONCE_PUSHED, NONCE_RESEND_SECS};
use hostkeeper_mdm_gate::{Command, CommandUuid, Nonce};
use support::enrollment::{EnrollRig, key_hash, request};
use support::{SERIAL, UDID, capture_logs, result};

/// One look at `now`.
async fn look(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.gate().tick().await;
}

/// Catches: a byte of a nonce in a log event or an audit line (in hex, base64, as numbers or
/// as its `Debug`), or a line that differs from what the gate says of an issue, a re-push and
/// a removal.
#[tokio::test(flavor = "current_thread")]
async fn no_byte_of_a_nonce_reaches_a_log_or_the_audit() {
    let (guard, logs) = capture_logs();
    let rig = EnrollRig::new("nonce-no-leak").await;
    rig.fake.allow(SERIAL);
    rig.enroll(SERIAL, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    rig.clock().set(1_100);
    let uuid = rig.last_uuid("InstallProfile");
    rig.rig.post(&result(UDID, Some(&uuid), "Error", &[])).await;
    look(&rig, 1_100 + NONCE_RESEND_SECS).await;
    rig.fake.approve(SERIAL, 1_200);
    look(&rig, 1_201).await;
    drop(guard);

    let request = key_hash(1).to_hex();
    assert_eq!(
        rig.fake.audit_lines(),
        [
            (
                ACTION_NONCE_PUSHED.to_owned(),
                format!("serial {SERIAL}; request {request}; push 1")
            ),
            (
                ACTION_NONCE_PUSHED.to_owned(),
                format!("serial {SERIAL}; request {request}; push 2")
            ),
        ]
    );
    let pushed = |push: &str| {
        [
            ("level", "INFO"),
            ("message", "pushed the nonce profile"),
            ("enrollment_id", UDID),
            ("serial", SERIAL),
            ("request_id", request.as_str()),
            ("push", push),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect::<support::LogEvent>()
    };
    let removed: support::LogEvent = [
        ("level", "INFO"),
        ("message", "removed the nonce profile: the Mac is approved"),
        ("enrollment_id", UDID),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let config: support::LogEvent = [
        ("level", "INFO"),
        ("message", "sent the config profile"),
        ("enrollment_id", UDID),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    assert_eq!(logs.events(), [config, pushed("1"), pushed("2"), removed]);

    let nonce = rig.fake.issued()[0].2.clone();
    let spellings = [
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        nonce.iter().map(|b| format!("{b:02X}")).collect::<String>(),
        format!("{nonce:?}"),
        base64_of(&nonce),
    ];
    assert_eq!(format!("{:?}", Nonce::new(nonce.clone())), "Nonce(..)");
    let command = Command::install_profile(CommandUuid::from_random([3; 16]), nonce.clone());
    let shown = format!("{command:?}");
    let record = format!("{:?}", rig.rig.record(UDID));
    for spelling in &spellings {
        assert!(!shown.contains(spelling.as_str()), "{shown}");
        assert!(!record.contains(spelling.as_str()), "{record}");
    }
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
