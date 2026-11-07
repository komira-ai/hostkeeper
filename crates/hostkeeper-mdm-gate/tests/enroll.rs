//! The gate's enrollment side against a fake NanoHUB and a fake of the coordinator's
//! enrollment, as design section 10 names its tests (task M12): the four conditions of a nonce
//! (5.1), its binding's guard, the stale request, the live nonce, the re-issue at the hour, the
//! re-push after an error, and the removal after the approval; and the config profile
//! (section 4). No byte of a nonce in a log or the audit is `tests/nonce_logs.rs`, a binary of
//! its own, so no other test's events reach its subscriber. Time is the rig's clock; progress is the
//! gate's own look ([`Gate::tick`]), called by the test.

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use hostkeeper_enrollment::fs::{Fs, RealFs};

use hostkeeper_mdm_gate::enroll::{
    CONFIG_RESEND_SECS, NONCE_RESEND_SECS, NONCE_VALID_SECS, TICK, run,
};
use hostkeeper_mdm_gate::{DeviceStore, FileDeviceStore, Phase};
use plist::{Dictionary, Value};
use support::enrollment::{EnrollRig, IDENTIFIER, key_hash, profile_dict, request};
use support::{
    Answer, OTHER_UDID, Rig, SERIAL, TestFs, UDID, authenticate, check_out, device_information,
    no_random, result, token_update,
};

/// When the rig's device enrolls: its first `TokenUpdate`.
const SINCE: u64 = 1_000;

fn config_id() -> String {
    format!("{IDENTIFIER}.config")
}

fn nonce_id() -> String {
    format!("{IDENTIFIER}.nonce")
}

/// A rig whose device [`UDID`] is enrolled since [`SINCE`] with [`SERIAL`], `supervised`, its
/// serial on the allowlist when `allowed`.
async fn rig(name: &str, supervised: bool, allowed: bool) -> EnrollRig {
    let rig = EnrollRig::new(name).await;
    if allowed {
        rig.fake.allow(SERIAL);
    }
    rig.enroll(SERIAL, supervised).await;
    rig
}

/// The identifiers of the profiles installed so far.
fn installed_ids(rig: &EnrollRig) -> Vec<String> {
    rig.installed().into_iter().map(|(id, _)| id).collect()
}

/// One look at `now`.
async fn look(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.gate().tick().await;
}

/// The device's answer `status` to the last command of `kind`.
async fn answer_last(rig: &EnrollRig, kind: &str, status: &str) {
    let uuid = rig.last_uuid(kind);
    rig.rig.post(&result(UDID, Some(&uuid), status, &[])).await;
}

/// The hex of the key hash of request `n`.
fn request_hex(n: u8) -> String {
    key_hash(n).to_hex()
}

/// Catches: a nonce issued to a device that is not supervised, that checked out, or that never
/// sent its `TokenUpdate`, when every other condition holds; and (the positive case) no nonce
/// for one that is both.
#[tokio::test]
async fn no_nonce_for_a_device_that_is_not_enrolled_and_supervised() {
    let unsupervised = rig("nonce-unsupervised", false, true).await;
    unsupervised.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&unsupervised, 1_010).await;
    assert!(unsupervised.fake.issued().is_empty());
    // The config profile needs no supervision (design 6.3): an unsupervised Mac is approved by
    // an operator, and its daemon needs its config all the same.
    assert_eq!(installed_ids(&unsupervised), [config_id()]);

    let checked_out = rig("nonce-checked-out", true, true).await;
    checked_out.rig.post(&check_out(UDID)).await;
    checked_out.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&checked_out, 1_010).await;
    assert!(checked_out.fake.issued().is_empty());

    // Authenticated with a serial, no `TokenUpdate`: not enrolled, nothing at all.
    let authenticated = EnrollRig::new("nonce-authenticated").await;
    authenticated.fake.allow(SERIAL);
    authenticated
        .rig
        .post(&authenticate(UDID, Some(SERIAL)))
        .await;
    authenticated.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&authenticated, 1_010).await;
    assert!(authenticated.fake.issued().is_empty());
    assert!(authenticated.profile_commands().is_empty());

    let both = rig("nonce-both", true, true).await;
    both.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&both, 1_010).await;
    assert_eq!(both.fake.issued().len(), 1);
}

/// Catches: anything sent to a Mac whose serial is off the allowlist (neither the config
/// profile nor a nonce), and a serial added later not acted on at the next look.
#[tokio::test]
async fn no_nonce_for_a_serial_off_the_allowlist() {
    let rig = rig("nonce-allowlist", true, false).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    assert!(rig.fake.issued().is_empty());
    assert!(rig.profile_commands().is_empty());

    rig.fake.allow(SERIAL);
    look(&rig, 1_011).await;
    assert_eq!(installed_ids(&rig), [config_id()]);
    // One profile command a look: the nonce at the next.
    look(&rig, 1_012).await;
    assert_eq!(installed_ids(&rig), [config_id(), nonce_id()]);
}

/// Catches: a nonce issued on enrolled, supervised and allowlisted alone, before the daemon
/// posted; a nonce bound to another request than the one that appeared, or issued for another
/// serial.
#[tokio::test]
async fn no_nonce_before_a_candidate_request_is_pending() {
    let rig = rig("nonce-no-candidate", true, true).await;
    look(&rig, 1_010).await;
    assert!(rig.fake.issued().is_empty());
    assert_eq!(installed_ids(&rig), [config_id()]);

    rig.fake.wait(SERIAL, request(7, UDID, 1_020));
    look(&rig, 1_021).await;
    let issued = rig.fake.issued();
    assert_eq!(issued.len(), 1);
    assert_eq!((issued[0].0.as_str(), issued[0].1), (SERIAL, key_hash(7)));
    assert_eq!(rig.nonces_pushed(), [(issued[0].2.clone(), request_hex(7))]);
    let nonce = rig.rig.record(UDID).nonce.expect("a nonce record");
    assert_eq!(
        (nonce.serial.as_str(), nonce.request_id, nonce.issued_unix),
        (SERIAL, key_hash(7), 1_021)
    );
    assert_eq!((nonce.enrollment, nonce.pushes), (1, 1));
}

/// Catches: a nonce pushed while two requests are candidates (the local reader of design 5.2:
/// the operator decides), or a duplicate claim counted as a candidate (so it blocks the
/// daemon's request) or taken as the one.
#[tokio::test]
async fn two_candidates_for_one_device_get_no_nonce() {
    let rig = rig("nonce-two", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    rig.fake.wait(SERIAL, request(2, UDID, 1_006));
    look(&rig, 1_010).await;
    assert!(rig.fake.issued().is_empty());
    assert_eq!(installed_ids(&rig), [config_id()]);

    let duplicate = self::rig("nonce-duplicate", true, true).await;
    let mut flagged = request(1, UDID, 1_005);
    flagged.duplicate_claim = true;
    duplicate.fake.wait(SERIAL, flagged.clone());
    look(&duplicate, 1_010).await;
    assert!(
        duplicate.fake.issued().is_empty(),
        "a duplicate claim alone"
    );
    duplicate.fake.wait(SERIAL, request(2, UDID, 1_006));
    look(&duplicate, 1_011).await;
    let issued = duplicate.fake.issued();
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].1, key_hash(2));
}

/// Catches: a request that claims the serial and another platform UUID (a remote claim of the
/// serial) taken as a candidate, alone or beside the daemon's; the UDID compared with its
/// letter case, so the daemon's lower-case spelling is no candidate.
#[tokio::test]
async fn a_request_with_the_serial_and_another_uuid_is_no_candidate() {
    let rig = rig("nonce-other-uuid", true, true).await;
    rig.fake.wait(SERIAL, request(1, OTHER_UDID, 1_005));
    look(&rig, 1_010).await;
    assert!(rig.fake.issued().is_empty());

    rig.fake
        .wait(SERIAL, request(2, &UDID.to_ascii_lowercase(), 1_006));
    look(&rig, 1_011).await;
    let issued = rig.fake.issued();
    assert_eq!(
        issued.len(),
        1,
        "the remote claim does not block the daemon"
    );
    assert_eq!(issued[0].1, key_hash(2));
}

/// Catches: a request posted before the current enrollment began (the key before a wipe)
/// taken as a candidate, or blocking the new one; a request posted in the enrollment's own
/// second refused.
#[tokio::test]
async fn a_request_older_than_the_enrollment_is_no_candidate() {
    let rig = rig("nonce-stale", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, SINCE - 1));
    look(&rig, 1_010).await;
    assert!(rig.fake.issued().is_empty());

    rig.fake.wait(SERIAL, request(2, UDID, SINCE));
    look(&rig, 1_011).await;
    let issued = rig.fake.issued();
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].1, key_hash(2));
}

/// Catches: a second nonce issued while one issued for the serial is good, at a later look
/// within its hour, for a new candidate, after the device enrolled again (a wipe), or for
/// another record of the same serial.
#[tokio::test]
async fn a_live_nonce_blocks_a_second_issue() {
    let rig = rig("nonce-live", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    assert_eq!(rig.fake.issued().len(), 1);
    look(&rig, 1_010 + 1_800).await;
    assert_eq!(rig.fake.issued().len(), 1);

    // Wiped: a new enrollment, a new key's request; the nonce of the old one still lives.
    rig.clock().set(2_000);
    rig.rig.post(&authenticate(UDID, Some(SERIAL))).await;
    rig.rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.rig.sent(UDID, "DeviceInformation");
    rig.rig
        .post(&device_information(UDID, &uuid, Some(SERIAL), Some(true)))
        .await;
    rig.fake.wait(SERIAL, request(2, UDID, 2_005));
    look(&rig, 2_010).await;
    assert_eq!(rig.fake.issued().len(), 1, "the old nonce still lives");
    look(&rig, 1_010 + NONCE_VALID_SECS + 1).await;
    // Now two candidates wait (the stale one is from before this enrollment, so only one).
    let issued = rig.fake.issued();
    assert_eq!(issued.len(), 2);
    assert_eq!(issued[1].1, key_hash(2));
}

/// Catches: a nonce issued again before its hour is out (3600 seconds), or not at 3601; a new
/// one bound to another request than the same, carrying the old bytes, or under another
/// identifier (so it would not replace the old profile); and a second issue within the hour of
/// the new one.
#[tokio::test]
async fn an_expired_nonce_is_reissued_once_per_hour() {
    let rig = rig("nonce-hour", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    let first = 1_010;
    look(&rig, first).await;
    // The device acknowledges the push, so nothing is pushed again within the hour.
    answer_last(&rig, "InstallProfile", "Acknowledged").await;

    look(&rig, first + NONCE_VALID_SECS).await;
    assert_eq!(rig.fake.issued().len(), 1, "good for exactly one hour");
    look(&rig, first + NONCE_VALID_SECS + 1).await;
    let issued = rig.fake.issued();
    assert_eq!(issued.len(), 2);
    assert_eq!(issued[1].1, key_hash(1), "bound to the same request");
    let pushed = rig.nonces_pushed();
    assert_eq!(pushed.len(), 2);
    assert_ne!(pushed[0].0, pushed[1].0);
    assert_eq!(pushed[1], (issued[1].2.clone(), request_hex(1)));
    assert_eq!(installed_ids(&rig).last(), Some(&nonce_id()));

    let second = first + NONCE_VALID_SECS + 1;
    answer_last(&rig, "InstallProfile", "Acknowledged").await;
    look(&rig, second + NONCE_VALID_SECS).await;
    assert_eq!(rig.fake.issued().len(), 2);
    look(&rig, second + NONCE_VALID_SECS + 1).await;
    assert_eq!(rig.fake.issued().len(), 3);
}

/// Catches: an `Error` answer to the nonce profile issuing a new nonce, its re-push sooner
/// than a minute after the answer or never, a re-push with other bytes or another request; a
/// push the device has not answered sent again; a push NanoHUB did not queue never sent again.
#[tokio::test]
async fn a_failed_profile_install_is_resent_with_the_same_nonce() {
    let rig = rig("nonce-resend", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    // Unanswered: never sent again, whatever the time.
    look(&rig, 1_010 + 600).await;
    assert_eq!(rig.nonces_pushed().len(), 1);

    rig.clock().set(1_700);
    answer_last(&rig, "InstallProfile", "Error").await;
    let record = rig.rig.record(UDID);
    assert_eq!(
        record.last_error.as_deref(),
        Some("the nonce profile: the device answered an error")
    );
    look(&rig, 1_700 + NONCE_RESEND_SECS - 1).await;
    assert_eq!(rig.nonces_pushed().len(), 1);
    look(&rig, 1_700 + NONCE_RESEND_SECS).await;
    let pushed = rig.nonces_pushed();
    assert_eq!(pushed.len(), 2);
    assert_eq!(pushed[1], pushed[0], "the same nonce, for the same request");
    assert_eq!(rig.fake.issued().len(), 1, "no new nonce");
    assert_eq!(rig.rig.record(UDID).nonce.expect("a nonce").pushes, 2);

    // NanoHUB does not queue the next push: it is sent again at the next look.
    rig.clock().set(1_900);
    answer_last(&rig, "InstallProfile", "CommandFormatError").await;
    rig.hub().answer(Answer::Status(
        500,
        r#"{"command_error":"the queue is full"}"#.to_owned(),
    ));
    look(&rig, 1_900 + NONCE_RESEND_SECS).await;
    assert_eq!(rig.nonces_pushed().len(), 3);
    assert_eq!(rig.rig.record(UDID).nonce.expect("a nonce").pushes, 2);
    rig.hub().answer(Answer::Queued);
    look(&rig, 1_900 + NONCE_RESEND_SECS + 1).await;
    let pushed = rig.nonces_pushed();
    assert_eq!(pushed.len(), 4);
    assert_eq!(pushed[3], pushed[0]);
    assert_eq!(rig.rig.record(UDID).nonce.expect("a nonce").pushes, 3);
    assert_eq!(rig.fake.issued().len(), 1);
}

/// Catches: the nonce profile left on an approved Mac, removed twice, removed by another
/// identifier; anything else sent after the approval (a new nonce, the config profile again);
/// an approval from before this enrollment read as one of it.
#[tokio::test]
async fn the_nonce_profile_is_removed_after_the_approval() {
    let rig = rig("nonce-remove", true, true).await;
    rig.fake.approve(SERIAL, SINCE - 1);
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    assert_eq!(installed_ids(&rig), [config_id(), nonce_id()]);
    assert!(rig.removed().is_empty());

    rig.fake.approve(SERIAL, 1_020);
    look(&rig, 1_021).await;
    assert_eq!(rig.removed(), [nonce_id()]);
    let removal = rig
        .rig
        .record(UDID)
        .nonce
        .and_then(|nonce| nonce.removal)
        .expect("a removal");
    assert_eq!(removal.sent_unix, 1_021);

    look(&rig, 1_022).await;
    answer_last(&rig, "RemoveProfile", "Acknowledged").await;
    // After the hour, and after an error answer to the config profile's hour: nothing.
    look(&rig, 1_021 + NONCE_VALID_SECS + CONFIG_RESEND_SECS).await;
    assert_eq!(rig.removed(), [nonce_id()]);
    assert_eq!(installed_ids(&rig), [config_id(), nonce_id()]);
    assert_eq!(rig.fake.issued().len(), 1);
    assert!(
        rig.rig
            .record(UDID)
            .nonce
            .and_then(|nonce| nonce.removal)
            .and_then(|removal| removal.acknowledged_unix())
            .is_some()
    );
}

/// Catches: a removal NanoHUB did not queue never sent again; one the device has not answered
/// sent again; a Mac approved before the gate sent it anything in this enrollment sent the
/// config profile, a nonce, or a removal.
#[tokio::test]
async fn a_removal_nanohub_did_not_queue_is_sent_again_and_only_then() {
    let rig = rig("nonce-remove-again", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    rig.fake.approve(SERIAL, 1_020);
    rig.hub()
        .answer(Answer::Status(500, r#"{"command_error":"no"}"#.to_owned()));
    look(&rig, 1_021).await;
    assert_eq!(rig.removed(), [nonce_id()]);
    rig.hub().answer(Answer::Queued);
    look(&rig, 1_022).await;
    assert_eq!(rig.removed(), [nonce_id(), nonce_id()]);
    look(&rig, 1_023).await;
    assert_eq!(rig.removed().len(), 2, "unanswered: not sent again");

    // Approved in this enrollment (at its first second) before the gate sent it anything.
    let approved = EnrollRig::new("nonce-remove-none").await;
    approved.fake.allow(SERIAL);
    approved.fake.approve(SERIAL, SINCE);
    approved.enroll(SERIAL, true).await;
    approved.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&approved, 1_010).await;
    assert!(approved.profile_commands().is_empty());
}

/// Catches: a config profile under another identifier, domain or payload type, a key of the
/// daemon's config lost or changed on the way (a number made text, a table flattened), or a
/// profile not of system scope; sent twice in one enrollment, or not again in the next.
#[tokio::test]
async fn the_config_profile_carries_the_loaders_keys_once_per_enrollment() {
    let rig = rig("config-once", false, true).await;
    let installs = rig.profile_commands();
    assert_eq!(installs.len(), 1);
    let profile = profile_dict(&installs[0]);
    let text = |key: &str| profile[key].as_string().expect("text").to_owned();
    assert_eq!(text("PayloadIdentifier"), config_id());
    assert_eq!(text("PayloadType"), "Configuration");
    assert_eq!(text("PayloadScope"), "System");
    assert_eq!(profile["PayloadVersion"].as_signed_integer(), Some(1));
    let payload = profile["PayloadContent"].as_array().expect("payloads");
    assert_eq!(payload.len(), 1);
    let payload = payload[0].as_dictionary().expect("a payload");
    assert_eq!(
        payload["PayloadType"].as_string(),
        Some("com.apple.ManagedClient.preferences")
    );
    assert_eq!(
        payload["PayloadIdentifier"].as_string(),
        Some(format!("{IDENTIFIER}.config.preferences").as_str())
    );
    assert_ne!(payload["PayloadUUID"], profile["PayloadUUID"]);
    let domains = payload["PayloadContent"].as_dictionary().expect("domains");
    assert_eq!(
        domains.keys().collect::<Vec<_>>(),
        ["dev.hostkeeper.config"]
    );
    let (_, settings) = rig.installed().remove(0);
    assert_eq!(settings, expected_settings());

    look(&rig, 1_010).await;
    answer_last(&rig, "InstallProfile", "Acknowledged").await;
    look(&rig, 1_010 + CONFIG_RESEND_SECS * 2).await;
    assert_eq!(rig.installed().len(), 1);
    let record = rig.rig.record(UDID);
    assert!(
        record
            .config_profile
            .and_then(|command| command.acknowledged_unix())
            .is_some()
    );

    // A new enrollment: the config profile again.
    rig.clock().set(9_000);
    rig.rig.post(&authenticate(UDID, Some(SERIAL))).await;
    assert_eq!(rig.rig.record(UDID).config_profile, None);
    rig.rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.rig.sent(UDID, "DeviceInformation");
    rig.rig
        .post(&device_information(UDID, &uuid, Some(SERIAL), Some(false)))
        .await;
    assert_eq!(installed_ids(&rig), [config_id(), config_id()]);
}

/// The settings the config profile forces: the keys of [`support::enrollment::CONFIG`].
fn expected_settings() -> Dictionary {
    let text = |s: &str| Value::from(s);
    let int = |n: i64| Value::Integer(n.into());
    let mut signers = Dictionary::new();
    signers.insert("organisation".into(), text("komira-ai"));
    signers.insert("workflow".into(), text(".github/workflows/release.yml"));
    signers.insert("refs".into(), Value::Array(vec![text("refs/tags/*")]));
    signers.insert("hosted_runner".into(), Value::Boolean(true));
    let mut bounds = Dictionary::new();
    for (key, value) in [
        ("max_package_bytes", 536_870_912),
        ("max_unpacked_bytes", 2_147_483_648),
        ("max_entries", 20_000),
        ("fetch_timeout_secs", 2_100),
        ("attestation_timeout_secs", 60),
        ("force_start_secs", 60),
    ] {
        bounds.insert(key.into(), int(value));
    }
    let mut settings = Dictionary::new();
    settings.insert(
        "coordinator".into(),
        text("https://coordinator.example:8443"),
    );
    settings.insert(
        "ca_sha256".into(),
        text("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"),
    );
    settings.insert("release_channel".into(), text("https://releases.example"));
    settings.insert("trusted_signers".into(), Value::Dictionary(signers));
    settings.insert("bounds".into(), Value::Dictionary(bounds));
    settings
}

/// Catches: a config profile the device answered with an error sent again sooner than the
/// hour, or never; one NanoHUB did not queue never sent again; one unanswered sent again.
#[tokio::test]
async fn a_config_profile_that_failed_is_sent_again() {
    let rig = EnrollRig::new("config-again").await;
    rig.enroll(SERIAL, false).await;
    rig.fake.allow(SERIAL);
    rig.hub()
        .answer(Answer::Status(500, r#"{"command_error":"no"}"#.to_owned()));
    look(&rig, 1_009).await;
    assert_eq!(rig.installed().len(), 1, "sent, and not queued");
    assert_eq!(rig.rig.record(UDID).sent, []);
    rig.hub().answer(Answer::Queued);
    let before = rig.installed().len();
    look(&rig, 1_010).await;
    assert_eq!(rig.installed().len(), before + 1, "not queued: sent again");
    look(&rig, 1_011).await;
    assert_eq!(
        rig.installed().len(),
        before + 1,
        "unanswered: not sent again"
    );

    rig.clock().set(1_100);
    answer_last(&rig, "InstallProfile", "Error").await;
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some("the config profile: the device answered an error")
    );
    look(&rig, 1_100 + CONFIG_RESEND_SECS - 1).await;
    assert_eq!(rig.installed().len(), before + 1);
    look(&rig, 1_100 + CONFIG_RESEND_SECS).await;
    assert_eq!(rig.installed().len(), before + 2);
}

/// Catches: a nonce issued while one issued for the same serial on another record (an
/// enrollment that checked out) still lives; a nonce of another serial blocking it.
#[tokio::test]
async fn a_live_nonce_of_another_record_of_the_serial_blocks_an_issue() {
    let rig = EnrollRig::new("nonce-elsewhere").await;
    rig.fake.allow(SERIAL);
    rig.fake.allow("C02OTHERSERIAL");
    rig.rig.enroll(OTHER_UDID, SERIAL, true).await;
    rig.fake.wait(SERIAL, request(1, OTHER_UDID, 1_005));
    look(&rig, 1_010).await;
    assert_eq!(rig.fake.issued().len(), 1);
    rig.rig.post(&check_out(OTHER_UDID)).await;

    // A third device, of another serial, with a live nonce of its own.
    rig.rig
        .enroll(
            "0D4F1C2E-0000-4C55-9E21-5A6B7C8D9E0F",
            "C02OTHERSERIAL",
            true,
        )
        .await;
    rig.fake.wait(
        "C02OTHERSERIAL",
        request(3, "0D4F1C2E-0000-4C55-9E21-5A6B7C8D9E0F", 1_010),
    );
    look(&rig, 1_011).await;
    look(&rig, 1_012).await;
    assert_eq!(rig.fake.issued().len(), 2);

    rig.enroll(SERIAL, true).await;
    rig.fake.wait(SERIAL, request(2, UDID, 1_020));
    look(&rig, 1_021).await;
    look(&rig, 1_022).await;
    assert_eq!(
        rig.fake.issued().len(),
        2,
        "the other record's nonce of the serial lives"
    );
    look(&rig, 1_010 + NONCE_VALID_SECS + 1).await;
    let issued = rig.fake.issued();
    assert_eq!(issued.len(), 3);
    assert_eq!(issued[2].1, key_hash(2));
}

/// Catches: an approval in a later enrollment removing a nonce profile pushed in an earlier
/// one (the wipe removed it, and the gate would send a `RemoveProfile` for nothing).
#[tokio::test]
async fn an_approval_removes_no_profile_of_an_earlier_enrollment() {
    let rig = rig("nonce-remove-earlier", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    rig.clock().set(2_000);
    rig.enroll(SERIAL, true).await;
    rig.fake.approve(SERIAL, 2_010);
    look(&rig, 2_011).await;
    assert!(rig.removed().is_empty());
}

/// Catches: a request that claims no platform UUID taken as a candidate of a device whose
/// check-ins named no UDID.
#[tokio::test]
async fn a_device_without_a_udid_has_no_candidate() {
    let mut rig = rig("nonce-no-udid", true, true).await;
    let mut record = rig.rig.record(UDID);
    record.udid = String::new();
    FileDeviceStore::open(Arc::new(RealFs), &rig.rig.dir())
        .expect("the store")
        .save(&record)
        .expect("saved");
    rig.restart().expect("the gate opens");
    rig.fake.wait(SERIAL, request(1, "", 1_005));
    look(&rig, 1_010).await;
    look(&rig, 1_011).await;
    assert!(rig.fake.issued().is_empty());
}

/// Catches: a look that issues, re-pushes or removes with UUIDs it could not draw, or one that
/// keeps no reason; a draw that fails part-way (the profile's own UUID drawn, its payload's
/// not) sending anything; nothing sent once random bytes are there again.
#[tokio::test]
async fn a_profile_with_no_random_bytes_is_not_sent_and_the_reason_is_kept() {
    let (random, budget) = support::budgeted();
    let rig = EnrollRig::with_random("enroll-budget", random).await;
    rig.enroll(SERIAL, true).await;
    rig.fake.allow(SERIAL);
    let reason = Some("no random bytes for a profile command".to_owned());

    budget.store(0, Ordering::SeqCst);
    look(&rig, 1_010).await;
    assert!(rig.profile_commands().is_empty());
    assert_eq!(rig.rig.record(UDID).last_error, reason);
    budget.store(-1, Ordering::SeqCst);
    look(&rig, 1_011).await;
    assert_eq!(installed_ids(&rig), [config_id()]);

    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    budget.store(1, Ordering::SeqCst);
    look(&rig, 1_012).await;
    assert!(rig.fake.issued().is_empty());
    assert_eq!(rig.rig.record(UDID).nonce, None);
    budget.store(-1, Ordering::SeqCst);
    look(&rig, 1_013).await;
    assert_eq!(rig.fake.issued().len(), 1);

    rig.clock().set(1_100);
    answer_last(&rig, "InstallProfile", "Error").await;
    budget.store(2, Ordering::SeqCst);
    look(&rig, 1_100 + NONCE_RESEND_SECS).await;
    assert_eq!(rig.nonces_pushed().len(), 1);
    budget.store(-1, Ordering::SeqCst);
    look(&rig, 1_101 + NONCE_RESEND_SECS).await;
    assert_eq!(rig.nonces_pushed().len(), 2);

    rig.fake.approve(SERIAL, 1_200);
    budget.store(0, Ordering::SeqCst);
    look(&rig, 1_201).await;
    assert!(rig.removed().is_empty());
    budget.store(-1, Ordering::SeqCst);
    look(&rig, 1_202).await;
    assert_eq!(rig.removed(), [nonce_id()]);
}

/// Catches: a profile sent, or a nonce issued, when the record that says so cannot be written
/// first (so a restart would not know of it); nothing sent once the writes work again.
#[tokio::test]
async fn nothing_is_sent_or_issued_when_its_record_cannot_be_written() {
    let fs = Arc::new(TestFs::default());
    let rig = EnrollRig::on_fs("enroll-writes", Arc::clone(&fs) as Arc<dyn Fs>).await;
    rig.enroll(SERIAL, true).await;
    rig.fake.allow(SERIAL);
    let heard = rig.hub().heard().len();

    fs.fail_writes_from(fs.writes() + 1);
    look(&rig, 1_010).await;
    assert_eq!(rig.hub().heard().len(), heard);
    fs.fail_writes_from(0);
    look(&rig, 1_011).await;
    assert_eq!(installed_ids(&rig), [config_id()]);

    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    fs.fail_writes_from(fs.writes() + 1);
    look(&rig, 1_012).await;
    assert!(
        rig.fake.issued().is_empty(),
        "the record is written before the issue"
    );
    fs.fail_writes_from(0);
    look(&rig, 1_013).await;
    assert_eq!(rig.fake.issued().len(), 1);

    rig.clock().set(1_100);
    answer_last(&rig, "InstallProfile", "Error").await;
    fs.fail_writes_from(fs.writes() + 1);
    look(&rig, 1_100 + NONCE_RESEND_SECS).await;
    assert_eq!(rig.nonces_pushed().len(), 1);
    fs.fail_writes_from(0);
    look(&rig, 1_101 + NONCE_RESEND_SECS).await;
    assert_eq!(rig.nonces_pushed().len(), 2);
}

/// Catches: a nonce issued when the enrollment cannot list the Mac's requests.
#[tokio::test]
async fn no_nonce_when_the_requests_cannot_be_listed() {
    let rig = rig("nonce-list-fails", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    rig.fake.fail_waiting(true);
    look(&rig, 1_010).await;
    assert!(rig.fake.issued().is_empty());
    rig.fake.fail_waiting(false);
    look(&rig, 1_011).await;
    assert_eq!(rig.fake.issued().len(), 1);
}

/// Catches: a nonce pushed again after a restart from bytes the gate cannot have, or a new one
/// issued while the old one lives; the record of the nonce lost at the restart; no new nonce
/// once the old one expired.
#[tokio::test]
async fn after_a_restart_a_live_nonce_is_kept_and_a_new_one_waits_for_its_expiry() {
    let mut rig = rig("nonce-restart", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    rig.clock().set(1_100);
    answer_last(&rig, "InstallProfile", "Error").await;
    let kept = rig.rig.record(UDID).nonce;
    rig.restart().expect("the gate opens");
    assert_eq!(rig.rig.record(UDID).nonce, kept);

    look(&rig, 1_100 + NONCE_RESEND_SECS).await;
    assert_eq!(rig.nonces_pushed().len(), 1, "no bytes to push again");
    assert_eq!(rig.fake.issued().len(), 1);
    look(&rig, 1_010 + NONCE_VALID_SECS + 1).await;
    assert_eq!(rig.fake.issued().len(), 2);
    assert_eq!(rig.nonces_pushed().len(), 2);
}

/// Catches: a record left saying a nonce was issued when the enrollment issued none (so the
/// device waits an hour for nothing); the enrollment's failure to read sending anything.
#[tokio::test]
async fn a_nonce_the_enrollment_did_not_issue_is_not_recorded() {
    let rig = rig("nonce-not-issued", true, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    rig.fake.fail_issue(true);
    look(&rig, 1_010).await;
    let record = rig.rig.record(UDID);
    assert_eq!(record.nonce, None);
    let kinds: Vec<&str> = record
        .sent
        .iter()
        .map(|s| s.request_type.as_str())
        .collect();
    assert_eq!(kinds, ["InstallProfile"], "the config profile's alone");
    assert_eq!(
        record.config_profile.map(|c| c.uuid.as_str().to_owned()),
        Some(record.sent[0].uuid.as_str().to_owned())
    );
    assert_eq!(installed_ids(&rig), [config_id()]);

    rig.fake.fail_issue(false);
    rig.fake.fail_reads(true);
    look(&rig, 1_011).await;
    assert_eq!(installed_ids(&rig), [config_id()]);
    rig.fake.fail_reads(false);
    look(&rig, 1_012).await;
    assert_eq!(installed_ids(&rig), [config_id(), nonce_id()]);
}

/// Catches: a profile sent with no command UUID (no random bytes), or without the reason kept.
#[tokio::test]
async fn no_random_bytes_sends_no_profile_and_says_why() {
    let rig = EnrollRig::with_random("enroll-no-random", no_random()).await;
    rig.fake.allow(SERIAL);
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    rig.rig.post(&token_update(UDID, 1)).await;
    look(&rig, 1_010).await;
    assert!(rig.profile_commands().is_empty());
    assert!(rig.fake.issued().is_empty());
}

/// Catches: a profile or a nonce for a serial two live enrollments have (design 6.1: the
/// gate says nothing of it), even when one of them is otherwise ready.
#[tokio::test]
async fn nothing_is_sent_for_a_serial_of_two_live_enrollments() {
    let rig = EnrollRig::new("enroll-ambiguous").await;
    rig.fake.allow(SERIAL);
    rig.rig.enroll(OTHER_UDID, SERIAL, true).await;
    let other = rig.profile_commands().len();
    assert_eq!(
        other, 1,
        "the first enrollment alone gets its config profile"
    );
    rig.rig.enroll(UDID, SERIAL, true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_010).await;
    assert_eq!(rig.profile_commands().len(), other);
    assert!(rig.fake.issued().is_empty());
}

/// Catches: the loop not looking when woken (a request posted waits for the minute), or a
/// wake given before the loop waits lost.
#[tokio::test]
async fn the_gate_looks_when_woken() {
    let EnrollRig { rig, fake } = rig("enroll-wake", true, true).await;
    let Rig {
        scratch: _scratch,
        hub,
        gate,
        ..
    } = rig;
    let gate = Arc::new(gate);
    let heard = hub.heard().len();
    fake.wait(SERIAL, request(1, UDID, 1_005));
    gate.wake();
    // An hour between ticks: only the wake makes it look during the test.
    let looping = tokio::spawn(run(Arc::clone(&gate), TICK * 60));
    hub.heard_at_least(heard + 1).await;
    assert_eq!(fake.issued().len(), 1);
    looping.abort();
}

/// Catches: a gate without the enrollment side sending a profile at a look.
#[tokio::test]
async fn a_gate_without_the_enrollment_side_sends_no_profile() {
    let rig = Rig::new("enroll-none").await;
    rig.enroll(UDID, SERIAL, true).await;
    let heard = rig.hub.heard().len();
    rig.gate.tick().await;
    assert_eq!(rig.hub.heard().len(), heard);
    assert_eq!(rig.record(UDID).phase, Phase::Enrolled);
}
