//! The device name and the named installs in the enrollment baseline (docs/design/coordinator-mdm.md
//! sections 4.7, 4.8 and 5.3) against a fake NanoHUB and a fake of the coordinator's enrollment:
//! what is sent, when it is sent again, what the status says, and that a command delivered twice
//! (delivery is at least once, spike model finding 4) changes nothing the second time.
//! Time is the rig's clock; progress is the gate's own look ([`Gate::tick`]) and the webhook
//! events the test posts.

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;

use hostkeeper_mdm_gate::{
    ACTION_BASELINE, BaselineState, BaselineStatus, COMMAND_TYPES, Handled, Installs, PkgDigest,
};
use plist::Value;
use support::baseline::{DESKTOP, GROUP, PROFILE_ID, parse, profile_bytes, profile_entry, toml};
use support::enrollment::{EnrollRig, command_dict, request};
use support::{
    SERIAL, UDID, authenticate, device_information, dict, handle, result, security_info,
    token_update,
};

/// The check interval of the rigs, in seconds.
const CHECK: u64 = 600;

/// The bytes of the hostkeeper pkg and of the two other pkgs the tests install.
const HOSTKEEPER_PKG: &[u8] = b"the bytes of a hostkeeper pkg, version 0.1.66";
const TOOLS_PKG: &[u8] = b"the bytes of the tools pkg, version 1.0";
const TOOLS_PKG_2: &[u8] = b"the bytes of the tools pkg, version 1.1";

/// The host id the allowlist gives the rig's Mac.
const NAME: &str = "mac-c02mt66kflhh";

fn put(rig: &EnrollRig, bytes: &[u8]) -> (PkgDigest, PathBuf) {
    let digest = PkgDigest::of(bytes);
    let dir = rig.rig.scratch.path().join("pkg");
    std::fs::create_dir_all(&dir).expect("the pkg directory");
    let path = dir.join(digest.file_name());
    std::fs::write(&path, bytes).expect("the pkg file");
    (digest, path)
}

/// The JSON of one install of the setting.
fn install_json(name: &str, digest: &PkgDigest, version: &str, path: &std::path::Path) -> String {
    format!(
        r#"{{"name":"{name}","bundle_id":"com.example.{name}","sha256":"{digest}","version":"{version}","path":"{}","url":"https://origin.example/{name}.pkg"}}"#,
        path.display()
    )
}

/// The installs of the hostkeeper pkg and, if given, `tools` (its bytes and version).
fn installs_of(rig: &EnrollRig, tools: Option<(&[u8], &str)>) -> Installs {
    let (digest, path) = put(rig, HOSTKEEPER_PKG);
    let mut rows = vec![install_json("hostkeeper", &digest, "0.1.66", &path)];
    if let Some((bytes, version)) = tools {
        let (digest, path) = put(rig, bytes);
        rows.push(install_json("tools", &digest, version, &path));
    }
    Installs::parse(&format!("[{}]", rows.join(","))).expect("the installs parse")
}

/// A rig whose gate has the baseline of [`GROUP`] (Screen Sharing and a profile), the pkg
/// install with the hostkeeper pkg, and `NAME` for [`SERIAL`]; the device [`UDID`] is enrolled,
/// `supervised`, with the config profile sent and not yet answered.
async fn farm(name: &str, supervised: bool) -> EnrollRig {
    let mut rig = EnrollRig::new(name).await;
    let dir = rig.rig.scratch.path().join("baseline");
    std::fs::create_dir_all(&dir).expect("the baseline directory");
    let profile = dir.join("fskit.mobileconfig");
    std::fs::write(&profile, profile_bytes(PROFILE_ID, "v1")).expect("the profile file");
    rig.baseline(Some(
        parse(&toml(
            GROUP,
            &format!("{DESKTOP}{}", profile_entry(&profile)),
        ))
        .expect("parses")
        .with_check_secs(CHECK),
    ));
    let (digest, path) = put(&rig, HOSTKEEPER_PKG);
    rig.install(&format!(
        "[[pkg]]\nsha256 = \"{digest}\"\nversion = \"0.1.66\"\npath = \"{}\"\n",
        path.display()
    ));
    rig.fake.allow(SERIAL);
    rig.fake.group(SERIAL, GROUP);
    rig.fake.name(SERIAL, NAME);
    rig.enroll(SERIAL, supervised).await;
    rig
}

/// A rig whose baseline holds a profile alone, so that nothing else in it needs supervision.
async fn profile_only(name: &str, supervised: bool) -> EnrollRig {
    let mut rig = EnrollRig::new(name).await;
    let dir = rig.rig.scratch.path().join("baseline");
    std::fs::create_dir_all(&dir).expect("the baseline directory");
    let profile = dir.join("fskit.mobileconfig");
    std::fs::write(&profile, profile_bytes(PROFILE_ID, "v1")).expect("the profile file");
    rig.baseline(Some(
        parse(&toml(GROUP, &profile_entry(&profile)))
            .expect("parses")
            .with_check_secs(CHECK),
    ));
    let (digest, path) = put(&rig, HOSTKEEPER_PKG);
    rig.install(&format!(
        "[[pkg]]\nsha256 = \"{digest}\"\nversion = \"0.1.66\"\npath = \"{}\"\n",
        path.display()
    ));
    rig.fake.allow(SERIAL);
    rig.fake.group(SERIAL, GROUP);
    rig.fake.name(SERIAL, NAME);
    rig.enroll(SERIAL, supervised).await;
    rig
}

/// One look at `now`.
async fn look(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.gate().tick().await;
}

/// The request types NanoHUB heard besides the two queries, in order.
fn kinds(rig: &EnrollRig) -> Vec<String> {
    rig.hub()
        .commands()
        .into_iter()
        .map(|(kind, _)| kind)
        .filter(|kind| kind != "DeviceInformation" && kind != "SecurityInfo")
        .collect()
}

fn count(rig: &EnrollRig, kind: &str) -> usize {
    kinds(rig).iter().filter(|heard| *heard == kind).count()
}

/// The device's `Acknowledged` to the `nth` command of `kind` NanoHUB heard, at `now`.
async fn ack(rig: &EnrollRig, kind: &str, nth: usize, now: u64) {
    rig.clock().set(now);
    let uuid = rig
        .hub()
        .commands()
        .into_iter()
        .filter(|(heard, _)| heard == kind)
        .map(|(_, uuid)| uuid)
        .nth(nth)
        .expect("a command of that kind");
    rig.rig
        .post(&result(UDID, Some(&uuid), "Acknowledged", &[]))
        .await;
}

/// The device's `Acknowledged` to the `InstallEnterpriseApplication` of the bundle `bundle`
/// that is the `nth` of them NanoHUB heard, at `now`.
async fn ack_install(rig: &EnrollRig, bundle: &str, nth: usize, now: u64) {
    rig.clock().set(now);
    let uuid = rig
        .hub()
        .heard()
        .iter()
        .filter(|heard| heard.command().0 == "InstallEnterpriseApplication")
        .filter(|heard| manifest_of(&command_dict(heard)).2 == bundle)
        .map(|heard| heard.command().1)
        .nth(nth)
        .expect("an install of that bundle");
    rig.rig
        .post(&result(UDID, Some(&uuid), "Acknowledged", &[]))
        .await;
}

fn status(rig: &EnrollRig) -> Option<BaselineStatus> {
    rig.gate().baseline_status(&rig.rig.record(UDID))
}

fn state(state: BaselineState) -> Option<BaselineStatus> {
    Some(BaselineStatus {
        state,
        reason: String::new(),
    })
}

/// The commands of `kind` NanoHUB heard, as their `Command` dictionaries.
fn heard_of(rig: &EnrollRig, kind: &str) -> Vec<plist::Dictionary> {
    rig.hub()
        .heard()
        .iter()
        .filter(|heard| heard.command().0 == kind)
        .map(command_dict)
        .collect()
}

/// The audit lines of the baseline's sends.
fn lines(rig: &EnrollRig) -> Vec<String> {
    rig.fake
        .audit_lines()
        .into_iter()
        .filter(|(action, _)| action == ACTION_BASELINE)
        .map(|(_, reason)| reason)
        .collect()
}

/// The digest and URL of the manifest of an `InstallEnterpriseApplication`, and its bundle
/// identifier and version.
fn manifest_of(command: &plist::Dictionary) -> (String, String, String, String) {
    let item = command["Manifest"].as_dictionary().expect("a Manifest")["items"]
        .as_array()
        .expect("items")[0]
        .as_dictionary()
        .expect("an item")
        .clone();
    let asset = item["assets"].as_array().expect("assets")[0]
        .as_dictionary()
        .expect("an asset");
    let metadata = item["metadata"].as_dictionary().expect("metadata");
    let text =
        |dict: &plist::Dictionary, key: &str| dict[key].as_string().expect("text").to_owned();
    (
        text(asset, "sha256"),
        text(asset, "url"),
        text(metadata, "bundle-identifier"),
        text(metadata, "bundle-version"),
    )
}

/// Catches: the device name sent before the config profile is acknowledged; a `Settings` that
/// does not set both `DeviceName` and `HostName` to the host id, or carries other keys than
/// Apple's schema has; a send not audited with the serial, the group and the name; a status that
/// is not `Pending` until the Mac answers; the name sent again with no change, now or at a later
/// check.
#[tokio::test]
async fn the_device_name_is_the_host_id_and_is_sent_once() {
    let rig = farm("name-once", true).await;
    assert_eq!(count(&rig, "Settings"), 0, "the config profile first");
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(count(&rig, "Settings"), 1);
    let command = &heard_of(&rig, "Settings")[0];
    let mut keys: Vec<&str> = command.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["RequestType", "Settings"]);
    let items = command["Settings"].as_array().expect("Settings");
    let item = |n: usize| items[n].as_dictionary().expect("an item").clone();
    assert_eq!(items.len(), 2);
    assert_eq!(
        item(0),
        dict(&[
            ("Item", Value::from("DeviceName")),
            ("DeviceName", Value::from(NAME))
        ])
    );
    assert_eq!(
        item(1),
        dict(&[
            ("Item", Value::from("HostName")),
            ("HostName", Value::from(NAME))
        ])
    );
    assert!(lines(&rig).contains(&format!(
        "serial {SERIAL}; group {GROUP}; device name {NAME}"
    )));
    assert_eq!(status(&rig), state(BaselineState::Pending));

    ack(&rig, "Settings", 0, 1_020).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_021).await;
    ack(&rig, "InstallProfile", 1, 1_022).await;
    assert_eq!(status(&rig), state(BaselineState::Applied));
    look(&rig, 1_022 + 10 * CHECK).await;
    assert_eq!(
        count(&rig, "Settings"),
        1,
        "the name is not sent again while it is the same"
    );
}

/// Catches: a changed host id (the inventory's `host_id`) that the Mac is not told of, or that
/// is told twice; the status not `Pending` between the change and the answer; a name sent to a
/// Mac the enrollment names nothing for.
#[tokio::test]
async fn a_changed_name_is_sent_again_and_an_unnamed_mac_keeps_its_own() {
    let rig = farm("name-changed", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    ack(&rig, "Settings", 0, 1_020).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_021).await;
    ack(&rig, "InstallProfile", 1, 1_022).await;
    assert_eq!(status(&rig), state(BaselineState::Applied));

    rig.fake.name(SERIAL, "farm-mac-7");
    assert_eq!(
        status(&rig),
        state(BaselineState::Pending),
        "the name differs"
    );
    look(&rig, 1_030).await;
    assert_eq!(count(&rig, "Settings"), 2);
    let latest = heard_of(&rig, "Settings").pop().expect("a command");
    assert_eq!(
        latest["Settings"].as_array().expect("Settings")[0]
            .as_dictionary()
            .expect("an item")["DeviceName"]
            .as_string(),
        Some("farm-mac-7")
    );
    look(&rig, 1_031).await;
    assert_eq!(count(&rig, "Settings"), 2, "not again while unanswered");
    ack(&rig, "Settings", 1, 1_040).await;
    assert_eq!(status(&rig), state(BaselineState::Applied));
    look(&rig, 1_041).await;
    assert_eq!(count(&rig, "Settings"), 2);

    let unnamed = EnrollRig::new("name-none").await;
    unnamed.fake.allow(SERIAL);
    unnamed.fake.group(SERIAL, GROUP);
    unnamed.enroll(SERIAL, true).await;
    assert_eq!(count(&unnamed, "Settings"), 0);
}

/// Catches: `DeviceName` sent to a Mac that is not supervised (Apple: supervised devices only),
/// now or at a later check, or its reason not shown.
#[tokio::test]
async fn the_device_name_is_sent_only_to_a_supervised_mac() {
    let rig = profile_only("name-unsupervised", false).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(count(&rig, "Settings"), 0);
    assert_eq!(
        status(&rig),
        Some(BaselineStatus {
            state: BaselineState::Failed,
            reason: "device name: the Mac is not supervised, and DeviceName needs a supervised Mac"
                .to_owned()
        })
    );
    look(&rig, 1_010 + 10 * CHECK).await;
    assert_eq!(count(&rig, "Settings"), 0);
}

/// Catches: a `Settings` the Mac answered with an error taken as applied, sent again before the
/// check interval, or never; the reason not shown; and a changed name not sent at once after an
/// error on the old one.
#[tokio::test]
async fn a_name_the_mac_refused_is_sent_again_at_the_interval_or_when_it_changes() {
    let rig = profile_only("name-error", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    rig.clock().set(1_020);
    let uuid = rig.last_uuid("Settings");
    rig.rig.post(&result(UDID, Some(&uuid), "Error", &[])).await;
    assert_eq!(
        status(&rig),
        Some(BaselineStatus {
            state: BaselineState::Failed,
            reason: "device name: the device answered an error".to_owned()
        })
    );
    look(&rig, 1_020 + CHECK - 1).await;
    assert_eq!(count(&rig, "Settings"), 1, "not before the interval");
    look(&rig, 1_020 + CHECK).await;
    assert_eq!(count(&rig, "Settings"), 2, "at the interval");

    rig.clock().set(2_000);
    let uuid = rig.last_uuid("Settings");
    rig.rig.post(&result(UDID, Some(&uuid), "Error", &[])).await;
    rig.fake.name(SERIAL, "renamed");
    look(&rig, 2_001).await;
    assert_eq!(count(&rig, "Settings"), 3, "a new name is not held back");
}

/// Catches: a `Settings` answered `Acknowledged` whose items did not all apply (an item with an
/// `Error` status, or none) shown as applied and never sent again; an item list that is all
/// acknowledged, or absent, shown as failed.
#[tokio::test]
async fn an_acknowledged_name_with_a_failed_item_is_failed_and_sent_again() {
    let item = |status: &str| {
        Value::Dictionary(dict(&[
            ("Status", Value::from(status)),
            ("Item", Value::from("HostName")),
        ]))
    };
    let rig = profile_only("name-item", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    let uuid = rig.last_uuid("Settings");
    rig.clock().set(1_020);
    let items = Value::Array(vec![item("Acknowledged"), item("Error")]);
    rig.rig
        .post(&result(
            UDID,
            Some(&uuid),
            "Acknowledged",
            &[("Settings", items)],
        ))
        .await;
    assert_eq!(
        status(&rig),
        Some(BaselineStatus {
            state: BaselineState::Failed,
            reason: "device name: the device answered an error".to_owned()
        })
    );
    look(&rig, 1_020 + CHECK).await;
    assert_eq!(count(&rig, "Settings"), 2, "sent again at the interval");

    // An item with no status is no success; all items acknowledged is.
    let uuid = rig.last_uuid("Settings");
    rig.clock().set(2_000);
    let items = Value::Array(vec![Value::Dictionary(dict(&[]))]);
    rig.rig
        .post(&result(
            UDID,
            Some(&uuid),
            "Acknowledged",
            &[("Settings", items)],
        ))
        .await;
    assert_eq!(status(&rig).map(|s| s.state), Some(BaselineState::Failed));
    look(&rig, 2_000 + CHECK).await;
    let uuid = rig.last_uuid("Settings");
    rig.clock().set(3_000);
    let items = Value::Array(vec![item("Acknowledged"), item("Acknowledged")]);
    rig.rig
        .post(&result(
            UDID,
            Some(&uuid),
            "Acknowledged",
            &[("Settings", items)],
        ))
        .await;
    assert_ne!(status(&rig).map(|s| s.state), Some(BaselineState::Failed));
    look(&rig, 3_000 + 10 * CHECK).await;
    assert_eq!(
        count(&rig, "Settings"),
        3,
        "an applied name is not sent again"
    );
}

/// Catches: a baseline install sent before the config profile is acknowledged, to a Mac of a
/// group with no baseline, or the hostkeeper pkg sent by the baseline as well as by the pkg
/// step; a manifest that does not name the entry's digest, bundle identifier and version or that
/// carries the origin `url` instead of the MDM host's (the Mac fetches from the MDM host); a
/// send not audited; a status that is not `Pending` until the Mac answers.
#[tokio::test]
async fn a_named_install_is_sent_to_a_baseline_mac_by_its_manifest() {
    let rig = farm("install-sent", true).await;
    rig.gate()
        .set_installs(installs_of(&rig, Some((TOOLS_PKG, "1.0"))));
    ack(&rig, "InstallProfile", 0, 1_010).await;
    // The hostkeeper pkg is the pkg step's, `tools` is the baseline's.
    let installs = heard_of(&rig, "InstallEnterpriseApplication");
    assert_eq!(installs.len(), 2);
    let tools: Vec<_> = installs
        .iter()
        .map(manifest_of)
        .filter(|(_, _, bundle, _)| bundle == "com.example.tools")
        .collect();
    let digest = PkgDigest::of(TOOLS_PKG);
    assert_eq!(
        tools,
        [(
            digest.to_string(),
            format!("https://mdm.example.com/pkg/{digest}.pkg"),
            "com.example.tools".to_owned(),
            "1.0".to_owned()
        )]
    );
    assert!(lines(&rig).contains(&format!(
        "serial {SERIAL}; group {GROUP}; install tools 1.0"
    )));
    assert_eq!(status(&rig), state(BaselineState::Pending));
    ack(&rig, "Settings", 0, 1_020).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_021).await;
    ack(&rig, "InstallProfile", 1, 1_022).await;
    assert_eq!(
        status(&rig),
        state(BaselineState::Pending),
        "tools is unanswered"
    );
    ack_install(&rig, "com.example.tools", 0, 1_023).await;
    assert_eq!(status(&rig), state(BaselineState::Applied));
    look(&rig, 1_023 + 10 * CHECK).await;
    assert_eq!(
        count(&rig, "InstallEnterpriseApplication"),
        2,
        "an applied install is not sent again"
    );

    // A Mac of a group the baseline does not list gets no install.
    let other = EnrollRig::new("install-no-group").await;
    other.fake.allow(SERIAL);
    other.enroll(SERIAL, true).await;
    other
        .gate()
        .set_installs(installs_of(&other, Some((TOOLS_PKG, "1.0"))));
    assert_eq!(count(&other, "InstallEnterpriseApplication"), 0);
}

/// Catches: an install entry that changed (a new file digest and version) that the Mac is not
/// sent again, or that is sent while the old one is unanswered, or twice; a change of the origin
/// `url` alone that sends the package again; the status not `Pending` while the new entry is
/// unanswered; an entry removed from the setting that is sent anyway.
#[tokio::test]
async fn a_changed_installs_entry_changes_the_baseline() {
    let rig = farm("install-changed", true).await;
    rig.gate()
        .set_installs(installs_of(&rig, Some((TOOLS_PKG, "1.0"))));
    ack(&rig, "InstallProfile", 0, 1_010).await;
    ack(&rig, "Settings", 0, 1_020).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_021).await;
    ack(&rig, "InstallProfile", 1, 1_022).await;
    ack_install(&rig, "com.example.tools", 0, 1_023).await;
    assert_eq!(status(&rig), state(BaselineState::Applied));

    // Only the origin changes: the same package, so nothing is sent.
    let same = installs_of(&rig, Some((TOOLS_PKG, "1.0")));
    rig.gate().set_installs(
        Installs::parse(&same.to_json().replace("origin.example", "mirror.example"))
            .expect("parses"),
    );
    assert_eq!(status(&rig), state(BaselineState::Applied));
    look(&rig, 1_030).await;
    assert_eq!(count(&rig, "InstallEnterpriseApplication"), 2);

    // A new version of the package.
    rig.gate()
        .set_installs(installs_of(&rig, Some((TOOLS_PKG_2, "1.1"))));
    assert_eq!(
        status(&rig),
        state(BaselineState::Pending),
        "the entry differs from what was sent"
    );
    look(&rig, 1_040).await;
    assert_eq!(count(&rig, "InstallEnterpriseApplication"), 3);
    let (digest, _, bundle, version) = manifest_of(
        heard_of(&rig, "InstallEnterpriseApplication")
            .last()
            .expect("a command"),
    );
    assert_eq!(
        (digest, bundle, version),
        (
            PkgDigest::of(TOOLS_PKG_2).to_string(),
            "com.example.tools".to_owned(),
            "1.1".to_owned()
        )
    );
    look(&rig, 1_041).await;
    assert_eq!(
        count(&rig, "InstallEnterpriseApplication"),
        3,
        "not again while unanswered"
    );
    ack_install(&rig, "com.example.tools", 1, 1_050).await;
    assert_eq!(status(&rig), state(BaselineState::Applied));

    // Removed from the setting: nothing more is sent for it, and the status ignores it.
    rig.gate().set_installs(installs_of(&rig, None));
    look(&rig, 1_060).await;
    assert_eq!(count(&rig, "InstallEnterpriseApplication"), 3);
    assert_eq!(status(&rig), state(BaselineState::Applied));
}

/// Catches: an install whose file is missing or whose bytes are not its listed digest sent
/// anyway (the Mac would refuse it, or fetch other bytes), or sent without a reason kept as the
/// device's last error; the rest of the baseline held back by it.
#[tokio::test]
async fn an_install_whose_file_is_not_its_digest_is_not_sent() {
    let rig = farm("install-bad-file", true).await;
    let installs = installs_of(&rig, Some((TOOLS_PKG, "1.0")));
    let path = installs.get("tools").expect("tools").path.clone();
    std::fs::write(&path, b"other bytes").expect("changed");
    rig.gate().set_installs(installs);
    ack(&rig, "InstallProfile", 0, 1_010).await;
    let tools = |rig: &EnrollRig| {
        heard_of(rig, "InstallEnterpriseApplication")
            .iter()
            .map(manifest_of)
            .filter(|(_, _, bundle, _)| bundle == "com.example.tools")
            .count()
    };
    assert_eq!(tools(&rig), 0);
    assert_eq!(
        count(&rig, "Settings"),
        1,
        "the rest of the baseline is sent"
    );
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 1);
    let error = rig.rig.record(UDID).last_error.expect("a reason");
    assert!(
        error.starts_with("install tools: the pkg ")
            && error.ends_with("not its listed sha256; it is not sent"),
        "{error}"
    );
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some(error.as_str())
    );

    std::fs::remove_file(&path).expect("removed");
    look(&rig, 1_020).await;
    assert_eq!(tools(&rig), 0);
    assert!(
        rig.rig
            .record(UDID)
            .last_error
            .expect("a reason")
            .contains("is missing")
    );

    std::fs::write(&path, TOOLS_PKG).expect("restored");
    look(&rig, 1_030).await;
    assert_eq!(tools(&rig), 1, "sent once the file is as listed");
}

/// Posts `body`, which answers a command, twice: the first is taken and the second is passed
/// over, and nothing else changes: the records, what NanoHUB heard, the audit.
async fn delivered_twice(rig: &EnrollRig, body: &[u8], what: &str) {
    assert_eq!(handle(rig.gate(), body).await, Handled::Recorded, "{what}");
    let records = rig.gate().records();
    let heard = rig.hub().heard().len();
    let audit = rig.fake.audit_lines();
    assert_eq!(
        handle(rig.gate(), body).await,
        Handled::PassedOver,
        "{what}"
    );
    assert_eq!(rig.gate().records(), records, "{what}");
    assert_eq!(rig.hub().heard().len(), heard, "{what}");
    assert_eq!(rig.fake.audit_lines(), audit, "{what}");
}

/// Catches: a command delivered to the Mac twice (the timeout can fire while the Mac still
/// holds it) whose second answer is read again: for each of the eight command types, the
/// repeated result leaves the record, NanoHUB's queue and the audit as the first left them; and
/// a command id enqueued twice, which no look, restart or repeated answer does.
#[tokio::test]
async fn a_pinned_command_delivered_twice_is_harmless() {
    let mut rig = farm("repeat-all", true).await;
    rig.gate()
        .set_installs(installs_of(&rig, Some((TOOLS_PKG, "1.0"))));
    let mut covered: BTreeSet<String> = BTreeSet::new();

    // The two queries were answered by the helper; deliver their answers twice as well by
    // starting over with a device the rig has not answered.
    let other = "0D4F1C2E-7B3A-4C55-9E21-5A6B7C8D9E0F";
    let serial = "C02OTHER0002";
    rig.fake.allow(serial);
    rig.fake.group(serial, GROUP);
    rig.rig.post(&authenticate(other, Some(serial))).await;
    rig.rig.post(&token_update(other, 1)).await;
    let uuid = rig.rig.sent(other, "DeviceInformation");
    delivered_twice(
        &rig,
        &device_information(other, &uuid, Some(serial), Some(true)),
        "DeviceInformation",
    )
    .await;
    covered.insert("DeviceInformation".to_owned());
    let uuid = rig.rig.sent(other, "SecurityInfo");
    delivered_twice(
        &rig,
        &security_info(other, &uuid, true, true),
        "SecurityInfo",
    )
    .await;
    covered.insert("SecurityInfo".to_owned());

    // The first Mac: its config profile, then the baseline, the pkg and the nonce.
    rig.fake.wait(SERIAL, request(1, UDID, 1_005));
    let mut now = 1_010;
    for _ in 0..8 {
        look(&rig, now).await;
        now += 10;
        let unanswered: Vec<_> = rig
            .rig
            .record(UDID)
            .sent
            .iter()
            .map(|sent| (sent.request_type.clone(), sent.uuid.to_string()))
            .collect();
        for (kind, uuid) in unanswered {
            let keys = if kind == "ProfileList" {
                let id = Value::Dictionary(dict(&[("PayloadIdentifier", Value::from(PROFILE_ID))]));
                vec![("ProfileList", Value::Array(vec![id]))]
            } else {
                Vec::new()
            };
            delivered_twice(
                &rig,
                &result(UDID, Some(&uuid), "Acknowledged", &keys),
                &kind,
            )
            .await;
            covered.insert(kind);
        }
        if now == 1_050 {
            // A restart between two looks enqueues nothing again. (The rig's random bytes start
            // over at a restart, so ids are compared up to here.)
            ids_distinct(&rig);
            let heard = rig.hub().heard().len();
            rig.restart().expect("the gate opens");
            look(&rig, now).await;
            now += 10;
            assert_eq!(
                rig.hub().heard().len(),
                heard,
                "a restart sends nothing again"
            );
        }
        if now == 1_070 {
            // The request is approved: the nonce profile is removed.
            rig.fake.approve(SERIAL, now);
        }
    }
    assert_eq!(
        covered,
        COMMAND_TYPES
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect::<BTreeSet<_>>(),
        "every pinned type was delivered twice"
    );
    assert!(rig.hub().commands().len() > 8);
}

/// Every command id NanoHUB heard is different: none was enqueued twice.
fn ids_distinct(rig: &EnrollRig) {
    let uuids: Vec<String> = rig
        .hub()
        .commands()
        .into_iter()
        .map(|(_, uuid)| uuid)
        .collect();
    let distinct: BTreeSet<&String> = uuids.iter().collect();
    assert_eq!(
        distinct.len(),
        uuids.len(),
        "no command id was enqueued twice"
    );
}

/// Catches: a pkg sent by the pkg step when the setting lists no install named `hostkeeper`
/// (there is nothing to send, and the baseline's installs are not the pkg step's), or the baseline
/// held back by that.
#[tokio::test]
async fn without_a_hostkeeper_install_the_pkg_step_sends_nothing() {
    let rig = farm("install-no-hostkeeper", true).await;
    rig.gate().set_installs(Installs::empty());
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(count(&rig, "InstallEnterpriseApplication"), 0);
    assert_eq!(count(&rig, "Settings"), 1, "the baseline goes on");
    assert_eq!(
        rig.gate()
            .installs()
            .map(|installs| installs.entries().len()),
        Some(0)
    );
}
