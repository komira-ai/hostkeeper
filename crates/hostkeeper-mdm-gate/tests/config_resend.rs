//! The config profile sent again when its settings change (docs/design/hostkeeper-mdm.md
//! section 4) against a fake NanoHUB and a fake of the coordinator's enrollment: once per
//! change however many looks and webhook events follow, delivered only when the device
//! acknowledged it, an error answer retried only after [`CONFIG_RESEND_SECS`], an approved Mac
//! sent that profile and nothing else, and a record written before the digest sent it once.
//! Time is the rig's clock; progress is the gate's own look ([`Gate::tick`]) and the webhook
//! events the test posts. A change of settings is a new gate over the same records, as after a
//! restart of the coordinator with other flags.

mod support;

use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_mdm_gate::enroll::CONFIG_RESEND_SECS;
use hostkeeper_mdm_gate::{
    ConfigProfile, DeviceStore, FileDeviceStore, MacosPkgProfile, StoresProfile,
};
use support::enrollment::{EnrollRig, IDENTIFIER, config, profile_dict, request};
use support::{SERIAL, UDID, counting_from, idle, result};

const STORES: &str =
    "[[store]]\nname = \"lan\"\nkind = \"https\"\nbase = \"https://mirror.example/\"\n";

const SIGNER: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

/// The config with the stores of [`STORES`] and the signer [`SIGNER`].
fn changed() -> ConfigProfile {
    config()
        .with_stores(StoresProfile::from_toml(STORES).expect("stores"))
        .with_macos_pkg(
            MacosPkgProfile::from_toml(&format!("signers = [\"{SIGNER}\"]\n")).expect("signers"),
        )
}

/// A rig whose device is enrolled at 1_000, `supervised`, with its serial on the allowlist, and whose
/// config profile (the config alone) was sent and acknowledged at 1_010.
async fn acknowledged(name: &str, supervised: bool) -> EnrollRig {
    let rig = EnrollRig::new(name).await;
    rig.fake.allow(SERIAL);
    rig.enroll(SERIAL, supervised).await;
    assert_eq!(config_sends(&rig), 1);
    answer_last(&rig, "Acknowledged", 1_010).await;
    rig
}

/// The `InstallProfile` commands of the config profile NanoHUB heard.
fn config_sends(rig: &EnrollRig) -> usize {
    rig.installed()
        .iter()
        .filter(|(id, _)| id == &format!("{IDENTIFIER}.config"))
        .count()
}

/// The device's answer `status` to the last `InstallProfile`, at `now`.
async fn answer_last(rig: &EnrollRig, status: &str, now: u64) {
    rig.clock().set(now);
    let uuid = rig.last_uuid("InstallProfile");
    rig.rig.post(&result(UDID, Some(&uuid), status, &[])).await;
}

/// One look at `now`.
async fn look(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.gate().tick().await;
}

/// A webhook event of the device (an idle check-in) at `now`: it makes a look.
async fn event(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.rig.post(&idle(UDID)).await;
}

/// The preference domains of the last profile NanoHUB heard.
fn last_domains(rig: &EnrollRig) -> Vec<String> {
    let heard = rig.profile_commands().pop().expect("a command");
    let profile = profile_dict(&heard);
    profile["PayloadContent"].as_array().expect("payloads")[0]
        .as_dictionary()
        .expect("a payload")["PayloadContent"]
        .as_dictionary()
        .expect("domains")
        .keys()
        .cloned()
        .collect()
}

/// The digest the record holds of its config profile.
fn recorded(rig: &EnrollRig) -> Option<String> {
    rig.rig
        .record(UDID)
        .config_profile
        .and_then(|command| command.settings_sha256)
}

/// Catches: a changed profile sent on every look, webhook event or check-in while it is pending
/// (a loop of `InstallProfile`), or sent twice after the device acknowledged it; the stores and
/// signers missing from the profile sent; an unchanged profile sent again; the digest counted as
/// delivered before the answer.
#[tokio::test]
async fn a_change_of_settings_is_sent_once_and_only_after_the_change() {
    let mut rig = acknowledged("resend-once", false).await;
    let first = recorded(&rig).expect("the digest of the first send");
    for now in [1_100, 1_200, 1_300] {
        look(&rig, now).await;
        event(&rig, now + 1).await;
    }
    assert_eq!(config_sends(&rig), 1, "the same settings: nothing");

    rig.reconfigure(changed(), counting_from(100));
    look(&rig, 2_000).await;
    assert_eq!(config_sends(&rig), 2);
    assert_eq!(
        last_domains(&rig),
        [
            "dev.hostkeeper.config",
            "dev.hostkeeper.stores",
            "dev.hostkeeper.macos-pkg"
        ]
    );
    let second = recorded(&rig).expect("the digest of the second send");
    assert_eq!(second, changed().settings_sha256());
    assert_ne!(second, first);
    assert!(
        rig.rig
            .record(UDID)
            .config_profile
            .expect("sent")
            .acknowledged_unix()
            .is_none(),
        "not delivered until acknowledged"
    );

    for now in [2_001, 2_100, 5_000, 90_000] {
        look(&rig, now).await;
        event(&rig, now + 1).await;
    }
    assert_eq!(config_sends(&rig), 2, "unanswered: not sent again");

    answer_last(&rig, "Acknowledged", 90_010).await;
    for now in [90_100, 90_200] {
        look(&rig, now).await;
        event(&rig, now + 1).await;
    }
    assert_eq!(config_sends(&rig), 2, "acknowledged: nothing more");
    assert_eq!(rig.profile_commands().len(), 2);
    assert_eq!(recorded(&rig), Some(second));

    // A restart with the same flags changes nothing.
    rig.reconfigure(changed(), counting_from(150));
    look(&rig, 91_000).await;
    assert_eq!(config_sends(&rig), 2);
}

/// Catches: an error answer to the changed profile retried at once or in a loop, or never; a
/// retry before [`CONFIG_RESEND_SECS`] after the answer; the digest of an errored send counted
/// as delivered (the settings then never reach the Mac).
#[tokio::test]
async fn a_changed_profile_answered_with_an_error_is_retried_only_after_the_backoff() {
    let mut rig = acknowledged("resend-error", false).await;
    rig.reconfigure(changed(), counting_from(100));
    look(&rig, 2_000).await;
    assert_eq!(config_sends(&rig), 2);

    answer_last(&rig, "Error", 2_100).await;
    for now in [2_101, 2_102] {
        look(&rig, now).await;
        event(&rig, now).await;
    }
    look(&rig, 2_100 + CONFIG_RESEND_SECS - 1).await;
    assert_eq!(config_sends(&rig), 2, "inside the backoff");
    look(&rig, 2_100 + CONFIG_RESEND_SECS).await;
    assert_eq!(config_sends(&rig), 3);
    look(&rig, 2_100 + CONFIG_RESEND_SECS + 1).await;
    assert_eq!(config_sends(&rig), 3, "unanswered again: not sent again");

    answer_last(&rig, "Acknowledged", 9_000).await;
    look(&rig, 9_000 + CONFIG_RESEND_SECS * 3).await;
    assert_eq!(config_sends(&rig), 3, "delivered");
}

/// Catches: an approved Mac not sent the profile whose settings changed (a signer rotation or a
/// new domain never reaching it), or sent it when they did not; anything else sent to it with
/// it (a nonce, the profile twice); a nonce profile left on it for the profile's send.
#[tokio::test]
async fn an_approved_mac_gets_the_changed_profile_and_nothing_else() {
    let mut rig = acknowledged("resend-approved", true).await;
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    look(&rig, 1_020).await;
    let nonces = rig.nonces_pushed().len();
    assert_eq!(nonces, 1);
    rig.fake.approve(SERIAL, 1_030);
    rig.reconfigure(changed(), counting_from(100));

    look(&rig, 1_040).await;
    assert_eq!(
        rig.removed(),
        [format!("{IDENTIFIER}.nonce")],
        "the removal first"
    );
    assert_eq!(config_sends(&rig), 1);
    look(&rig, 1_041).await;
    assert_eq!(config_sends(&rig), 2, "then the changed profile");
    for now in [1_042, 1_100, 50_000] {
        look(&rig, now).await;
        event(&rig, now + 1).await;
    }
    assert_eq!(config_sends(&rig), 2);
    assert_eq!(rig.nonces_pushed().len(), nonces);
    assert_eq!(rig.fake.issued().len(), 1);
    assert_eq!(rig.removed().len(), 1);

    answer_last(&rig, "Acknowledged", 50_010).await;
    look(&rig, 60_000).await;
    assert_eq!(config_sends(&rig), 2);
    assert_eq!(
        rig.profile_commands().len(),
        4,
        "config, nonce, its removal, config"
    );
}

/// Catches: an approved Mac's changed profile that the Mac answered with an error never sent
/// again, or in a loop; one sent sooner than [`CONFIG_RESEND_SECS`] after the answer.
#[tokio::test]
async fn an_approved_mac_s_changed_profile_is_retried_after_an_error_answer() {
    let mut rig = acknowledged("resend-approved-error", false).await;
    rig.fake.approve(SERIAL, 1_030);
    rig.reconfigure(changed(), counting_from(100));
    look(&rig, 1_040).await;
    assert_eq!(config_sends(&rig), 2);
    answer_last(&rig, "Error", 1_050).await;
    look(&rig, 1_050 + CONFIG_RESEND_SECS - 1).await;
    assert_eq!(config_sends(&rig), 2);
    look(&rig, 1_050 + CONFIG_RESEND_SECS).await;
    assert_eq!(config_sends(&rig), 3);
}

/// Catches: a Mac approved before the gate sent it anything in this enrollment sent the config
/// profile because it is "different" from no command at all.
#[tokio::test]
async fn an_approved_mac_with_no_config_profile_is_sent_none() {
    let rig = EnrollRig::new("resend-approved-none").await;
    rig.fake.allow(SERIAL);
    rig.fake.approve(SERIAL, 5_000);
    rig.enroll(SERIAL, false).await;
    look(&rig, 6_000).await;
    assert!(rig.profile_commands().is_empty());
    assert_eq!(rig.rig.record(UDID).config_profile, None);
}

/// Catches: a record written before the digest (an acknowledged config profile with none) not
/// sent the profile once, so a Mac enrolled before the new domains never gets them; sent again
/// and again because the digest stays unknown; sent again after the acknowledgement.
#[tokio::test]
async fn a_record_with_no_digest_is_sent_the_profile_once() {
    let mut rig = acknowledged("resend-no-digest", false).await;
    let mut record = rig.rig.record(UDID);
    record
        .config_profile
        .as_mut()
        .expect("a config profile")
        .settings_sha256 = None;
    let store = FileDeviceStore::open(std::sync::Arc::new(RealFs), &rig.rig.dir()).expect("store");
    store.save(&record).expect("saved");
    rig.reconfigure(config(), counting_from(100));
    assert_eq!(config_sends(&rig), 1);

    look(&rig, 2_000).await;
    assert_eq!(config_sends(&rig), 2);
    assert_eq!(recorded(&rig), Some(config().settings_sha256()));
    look(&rig, 2_001).await;
    assert_eq!(config_sends(&rig), 2);
    answer_last(&rig, "Acknowledged", 2_010).await;
    look(&rig, 99_999).await;
    assert_eq!(config_sends(&rig), 2);
}
