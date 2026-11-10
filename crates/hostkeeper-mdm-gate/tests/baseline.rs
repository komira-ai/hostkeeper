//! The enrollment baseline (docs/design/hostkeeper-mdm.md section 6.4) against a fake NanoHUB and
//! a fake of the coordinator's enrollment: what is sent after the config profile is
//! acknowledged and to whom, what is kept by the `ProfileList` check, what a failure does and
//! shows, the file and its refusals, and the record's compatibility with the release before.
//! Time is the rig's clock; progress is the gate's own look ([`Gate::tick`]) and the webhook
//! events the test posts.

mod support;

use std::path::{Path, PathBuf};

use hostkeeper_mdm_gate::{
    ACTION_BASELINE, Baseline, BaselineError, BaselineProfile, BaselineRecord, BaselineState,
    BaselineStatus, CommandUuid, DEFAULT_CHECK_SECS, DeviceRecord, Item, PkgDigest, ProfileAnswer,
    ProfileCommand, ProfileIdentifier, Sent, StoreError, decode_record, encode_record,
};
use plist::Value;
use prost::Message as _;
use sha2::{Digest as _, Sha256};
use support::enrollment::{EnrollRig, IDENTIFIER, StatFs, command_dict, profile_dict, request};
use support::{
    Answer, OTHER_UDID, SERIAL, UDID, budgeted, check_out, dict, idle, plist, result, token_update,
};

const GROUP: &str = "farm-macs";
const PROFILE_ID: &str = "com.example.fskit";

/// The check interval of the rigs, in seconds.
const CHECK: u64 = 600;

const DESKTOP: &str = "[[group.entry]]\nremote_desktop = true\n";

/// An unsigned `Configuration` profile `identifier`; `name` tells two versions apart.
fn profile_bytes(identifier: &str, name: &str) -> Vec<u8> {
    plist(dict(&[
        ("PayloadType", Value::from("Configuration")),
        ("PayloadIdentifier", Value::from(identifier)),
        (
            "PayloadUUID",
            Value::from("6C1F6A52-2B2E-4F55-9D6B-4B7B3D2F9A10"),
        ),
        ("PayloadVersion", Value::Integer(1.into())),
        ("PayloadDisplayName", Value::from(name)),
        ("PayloadContent", Value::Array(Vec::new())),
    ]))
}

fn identifier() -> ProfileIdentifier {
    ProfileIdentifier::parse(IDENTIFIER).expect("an identifier")
}

/// The baseline of `text`, its files read as root's.
fn parse(text: &str) -> Result<Baseline, BaselineError> {
    Baseline::parse(text, &StatFs::root(), &identifier())
}

/// A group `group` with the entries `entries`.
fn toml(group: &str, entries: &str) -> String {
    format!("[[group]]\nhost_group = \"{group}\"\n{entries}")
}

fn profile_entry(path: &Path) -> String {
    format!("[[group.entry]]\nprofile = \"{}\"\n", path.display())
}

/// Writes the profile file `name` of `rig`'s baseline directory.
fn write_profile(rig: &EnrollRig, name: &str, bytes: &[u8]) -> PathBuf {
    let dir = rig.rig.scratch.path().join("baseline");
    std::fs::create_dir_all(&dir).expect("the baseline directory");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("the profile file");
    path
}

/// The baseline of [`GROUP`]: Screen Sharing, then the profile `bytes` of [`PROFILE_ID`].
fn baseline_of(rig: &EnrollRig, bytes: &[u8]) -> Baseline {
    let path = write_profile(rig, "fskit.mobileconfig", bytes);
    parse(&toml(GROUP, &format!("{DESKTOP}{}", profile_entry(&path))))
        .expect("parses")
        .with_check_secs(CHECK)
}

/// A rig whose gate has the baseline of [`GROUP`], whose device [`UDID`] is enrolled at 1_000
/// with [`SERIAL`] on the allowlist in that group, `supervised`; the config profile is sent and
/// not yet answered.
async fn farm(name: &str, supervised: bool) -> EnrollRig {
    let mut rig = EnrollRig::new(name).await;
    rig.baseline(Some(baseline_of(&rig, &profile_bytes(PROFILE_ID, "v1"))));
    rig.fake.allow(SERIAL);
    rig.fake.group(SERIAL, GROUP);
    rig.enroll(SERIAL, supervised).await;
    rig
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

/// The device's answer `status` to the `nth` command of `kind` NanoHUB heard, at `now`, whose
/// result holds `keys`.
async fn answer(
    rig: &EnrollRig,
    kind: &str,
    nth: usize,
    status: &str,
    now: u64,
    keys: &[(&str, Value)],
) {
    rig.clock().set(now);
    let uuid = rig
        .hub()
        .commands()
        .into_iter()
        .filter(|(heard, _)| heard == kind)
        .map(|(_, uuid)| uuid)
        .nth(nth)
        .expect("a command of that kind");
    rig.rig.post(&result(UDID, Some(&uuid), status, keys)).await;
}

async fn ack(rig: &EnrollRig, kind: &str, nth: usize, now: u64) {
    answer(rig, kind, nth, "Acknowledged", now, &[]).await;
}

/// A `ProfileList` result naming `identifiers`.
fn listing(identifiers: &[&str]) -> Vec<(&'static str, Value)> {
    let items = identifiers
        .iter()
        .map(|id| Value::Dictionary(dict(&[("PayloadIdentifier", Value::from(*id))])))
        .collect();
    vec![("ProfileList", Value::Array(items))]
}

/// One look at `now`.
async fn look(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.gate().tick().await;
}

fn status(rig: &EnrollRig) -> Option<BaselineStatus> {
    rig.gate().baseline_status(&rig.rig.record(UDID))
}

fn pending() -> Option<BaselineStatus> {
    Some(BaselineStatus {
        state: BaselineState::Pending,
        reason: String::new(),
    })
}

fn applied() -> Option<BaselineStatus> {
    Some(BaselineStatus {
        state: BaselineState::Applied,
        reason: String::new(),
    })
}

fn failed(reason: &str) -> Option<BaselineStatus> {
    Some(BaselineStatus {
        state: BaselineState::Failed,
        reason: reason.to_owned(),
    })
}

/// The `Payload` of each `InstallProfile` of the baseline profile [`PROFILE_ID`] NanoHUB heard.
fn profile_installs(rig: &EnrollRig) -> Vec<Vec<u8>> {
    rig.hub()
        .heard()
        .iter()
        .filter(|heard| heard.command().0 == "InstallProfile")
        .filter(|heard| profile_dict(heard)["PayloadIdentifier"].as_string() == Some(PROFILE_ID))
        .map(|heard| {
            command_dict(heard)["Payload"]
                .as_data()
                .expect("a Payload")
                .to_vec()
        })
        .collect()
}

fn baseline_lines(rig: &EnrollRig) -> Vec<String> {
    rig.fake
        .audit_lines()
        .into_iter()
        .filter(|(action, _)| action == ACTION_BASELINE)
        .map(|(_, reason)| reason)
        .collect()
}

const DESKTOP_LINE: &str = "serial C02MT66KFLHH; group farm-macs; remote_desktop";
const PROFILE_LINE: &str = "serial C02MT66KFLHH; group farm-macs; profile com.example.fskit";

/// Catches: a baseline sent before the Mac acknowledged its config profile, or not after it;
/// a profile sent with other bytes than its file's; `EnableRemoteDesktop` with keys Apple's
/// schema does not have; a send not audited, or audited without the serial, the group and the
/// entry.
#[tokio::test]
async fn the_baseline_follows_the_config_profile_ack() {
    let rig = farm("baseline-order", true).await;
    assert_eq!(kinds(&rig), ["InstallProfile"], "the config profile alone");
    assert_eq!(
        status(&rig),
        pending(),
        "no config profile acknowledged yet"
    );
    assert!(baseline_lines(&rig).is_empty());

    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(
        kinds(&rig),
        ["InstallProfile", "EnableRemoteDesktop", "InstallProfile"]
    );
    assert_eq!(
        profile_installs(&rig),
        [profile_bytes(PROFILE_ID, "v1")],
        "the file's bytes, as they are"
    );
    let heard = rig.hub().heard();
    let desktop = command_dict(&heard[3]);
    assert_eq!(
        desktop.keys().map(String::as_str).collect::<Vec<_>>(),
        ["RequestType"]
    );
    assert_eq!(baseline_lines(&rig), [DESKTOP_LINE, PROFILE_LINE]);
    assert_eq!(status(&rig), pending());
    assert_eq!(
        rig.gate().baseline_groups().into_iter().collect::<Vec<_>>(),
        [GROUP]
    );
}

/// Catches: a baseline sent to a Mac whose host group the file does not list, or that has no
/// host group, or whose serial is not on the allowlist; a status shown for it.
#[tokio::test]
async fn the_baseline_is_picked_by_the_group_of_the_serial() {
    let rig = farm("baseline-groups", true).await;
    let third = "5E6F7A8B-1C2D-4E3F-8A9B-0C1D2E3F4A5B";
    rig.fake.allow("C02OTHER0001");
    rig.fake.group("C02OTHER0001", "lab");
    rig.fake.allow("C02NOGROUP01");
    rig.rig.enroll(OTHER_UDID, "C02OTHER0001", true).await;
    rig.rig.enroll(third, "C02NOGROUP01", true).await;
    for udid in [OTHER_UDID, third] {
        let uuid = rig.rig.sent(udid, "InstallProfile");
        rig.rig
            .post(&result(udid, Some(&uuid), "Acknowledged", &[]))
            .await;
        look_all(&rig).await;
        let record = rig.rig.record(udid);
        assert!(record.baseline.is_none(), "{udid}");
        assert_eq!(rig.gate().baseline_status(&record), None, "{udid}");
    }
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 0);
    assert_eq!(count(&rig, "InstallProfile"), 3, "three config profiles");
    assert!(baseline_lines(&rig).is_empty());
}

async fn look_all(rig: &EnrollRig) {
    rig.gate().tick().await;
}

/// Catches: `EnableRemoteDesktop` sent to a Mac that is not supervised (Apple's schema says
/// `supervised: true`), now or at a later check; its profile held back with it; the reason not
/// shown.
#[tokio::test]
async fn remote_desktop_is_sent_only_to_a_supervised_mac() {
    let rig = farm("baseline-unsupervised", false).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(kinds(&rig), ["InstallProfile", "InstallProfile"]);
    let reason =
        "remote_desktop: the Mac is not supervised, and EnableRemoteDesktop needs a supervised Mac";
    assert_eq!(status(&rig), failed(reason));
    ack(&rig, "InstallProfile", 1, 1_020).await;
    look(&rig, 1_020 + 10 * CHECK).await;
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 0);
    assert_eq!(
        status(&rig),
        failed(reason),
        "the profile is applied; this is not"
    );
}

/// Catches: a profile that went missing from the Mac not sent again after a `ProfileList`
/// names the others; the list sent before the profile is acknowledged, or before the check is
/// due; `EnableRemoteDesktop` and the list sent again a check interval after their last answer,
/// at the interval and not a second before.
#[tokio::test]
async fn a_missing_profile_is_sent_again_on_a_profile_list_check() {
    let rig = farm("baseline-list", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    assert_eq!(count(&rig, "ProfileList"), 0, "the profile is unanswered");
    assert_eq!(status(&rig), pending());
    ack(&rig, "InstallProfile", 1, 1_030).await;
    assert_eq!(status(&rig), applied());
    assert_eq!(
        count(&rig, "ProfileList"),
        1,
        "checked once it is acknowledged"
    );

    answer(
        &rig,
        "ProfileList",
        0,
        "Acknowledged",
        1_040,
        &listing(&["com.example.other"]),
    )
    .await;
    assert_eq!(profile_installs(&rig).len(), 2, "sent again, being missing");
    assert_eq!(
        count(&rig, "ProfileList"),
        1,
        "not checked with the install"
    );
    assert_eq!(status(&rig), pending());
    ack(&rig, "InstallProfile", 2, 1_050).await;
    assert_eq!(status(&rig), applied());
    assert_eq!(profile_installs(&rig).len(), 2);

    let before = kinds(&rig).len();
    look(&rig, 1_020 + CHECK - 1).await;
    assert_eq!(kinds(&rig).len(), before, "no check is due");
    look(&rig, 1_020 + CHECK).await;
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 2);
    assert_eq!(count(&rig, "ProfileList"), 1);
    assert_eq!(status(&rig), applied(), "a refresh is not a regression");
    look(&rig, 1_040 + CHECK - 1).await;
    assert_eq!(count(&rig, "ProfileList"), 1);
    look(&rig, 1_040 + CHECK).await;
    assert_eq!(count(&rig, "ProfileList"), 2);
    assert_eq!(
        count(&rig, "EnableRemoteDesktop"),
        2,
        "its answer is awaited"
    );
    assert_eq!(profile_installs(&rig).len(), 2, "nothing is missing");
}

/// Catches: a Mac's `TokenUpdate` that does not make the check due again, or an idle check-in
/// that does.
#[tokio::test]
async fn a_token_update_makes_the_check_due() {
    let rig = farm("baseline-token", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    ack(&rig, "InstallProfile", 1, 1_030).await;
    answer(
        &rig,
        "ProfileList",
        0,
        "Acknowledged",
        1_040,
        &listing(&[PROFILE_ID]),
    )
    .await;
    assert_eq!(count(&rig, "ProfileList"), 1);
    rig.clock().set(1_050);
    rig.rig.post(&idle(UDID)).await;
    assert_eq!(count(&rig, "ProfileList"), 1);
    rig.clock().set(1_060);
    rig.rig.post(&token_update(UDID, 2)).await;
    assert_eq!(count(&rig, "ProfileList"), 2);
    assert_eq!(status(&rig), applied());
}

/// Catches: a baseline error answer failing the enrollment (the pkg or the nonce held back), or
/// the failure not shown with its reason; an errored profile sent again before the check
/// interval, or never.
#[tokio::test]
async fn a_baseline_failure_does_not_fail_enrollment() {
    let mut rig = EnrollRig::new("baseline-failure").await;
    let digest = PkgDigest::of(b"a pkg");
    let dir = rig.rig.scratch.path().join("pkg");
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join(digest.file_name());
    std::fs::write(&path, b"a pkg").expect("pkg");
    rig.install(&format!(
        "[[pkg]]\nsha256 = \"{digest}\"\nversion = \"0.1.66\"\npath = \"{}\"\n",
        path.display()
    ));
    rig.baseline(Some(baseline_of(&rig, &profile_bytes(PROFILE_ID, "v1"))));
    rig.fake.allow(SERIAL);
    rig.fake.group(SERIAL, GROUP);
    rig.enroll(SERIAL, true).await;

    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(
        count(&rig, "InstallEnterpriseApplication"),
        1,
        "the pkg too"
    );
    answer(&rig, "InstallProfile", 1, "Error", 1_020, &[]).await;
    assert_eq!(
        status(&rig),
        failed("profile com.example.fskit: the device answered an error")
    );
    answer(
        &rig,
        "EnableRemoteDesktop",
        0,
        "CommandFormatError",
        1_021,
        &[],
    )
    .await;
    assert_eq!(
        status(&rig),
        failed("remote_desktop: the device answered an error"),
        "the first failed entry of the file"
    );
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some("the baseline's remote desktop: the device answered an error")
    );

    rig.fake.wait(SERIAL, request(1, UDID, 1_030));
    look(&rig, 1_030).await;
    assert_eq!(rig.nonces_pushed().len(), 1, "the nonce is pushed");

    look(&rig, 1_020 + CHECK - 1).await;
    assert_eq!(profile_installs(&rig).len(), 1);
    look(&rig, 1_020 + CHECK).await;
    assert_eq!(
        profile_installs(&rig).len(),
        2,
        "sent again at the interval"
    );
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 1, "its error is 1_021");
    look(&rig, 1_021 + CHECK).await;
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 2);
    ack(&rig, "InstallProfile", 3, 1_700).await;
    assert_eq!(
        status(&rig),
        pending(),
        "the retry of remote_desktop is awaited"
    );
    answer(&rig, "EnableRemoteDesktop", 1, "Error", 1_710, &[]).await;
    assert_eq!(
        status(&rig),
        failed("remote_desktop: the device answered an error"),
        "the profile is applied; the other entry failed again"
    );
}

/// Catches: a profile file changed on disk (same identifier) never reaching Macs that
/// acknowledged the old one; the new one sent again and again.
#[tokio::test]
async fn a_changed_profile_file_is_sent_again_once() {
    let mut rig = farm("baseline-changed", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    ack(&rig, "InstallProfile", 1, 1_030).await;
    answer(
        &rig,
        "ProfileList",
        0,
        "Acknowledged",
        1_040,
        &listing(&[PROFILE_ID]),
    )
    .await;
    rig.baseline(Some(baseline_of(&rig, &profile_bytes(PROFILE_ID, "v2"))));
    look(&rig, 2_000).await;
    assert_eq!(
        profile_installs(&rig),
        [
            profile_bytes(PROFILE_ID, "v1"),
            profile_bytes(PROFILE_ID, "v2")
        ]
    );
    look(&rig, 2_001).await;
    assert_eq!(
        profile_installs(&rig).len(),
        2,
        "unanswered: not sent again"
    );
    ack(&rig, "InstallProfile", 2, 2_010).await;
    look(&rig, 2_011).await;
    assert_eq!(profile_installs(&rig).len(), 2);
    assert_eq!(status(&rig), applied());
}

/// Catches: a Mac whose request is approved losing its baseline, or an approval sending the
/// nonce flow into the baseline.
#[tokio::test]
async fn an_approved_mac_is_still_sent_its_baseline() {
    let rig = farm("baseline-approved", true).await;
    rig.fake.approve(SERIAL, 1_005);
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(
        kinds(&rig),
        ["InstallProfile", "EnableRemoteDesktop", "InstallProfile"]
    );
    assert!(rig.nonces_pushed().is_empty());
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    ack(&rig, "InstallProfile", 1, 1_030).await;
    assert_eq!(count(&rig, "ProfileList"), 1);
    assert_eq!(status(&rig), applied());
}

/// Catches: a command NanoHUB did not queue counted as sent (never sent again, or audited), or
/// sent again at every look once it is queued.
#[tokio::test]
async fn a_command_nanohub_did_not_queue_is_sent_at_the_next_look() {
    let rig = farm("baseline-unqueued", true).await;
    rig.hub().answer(Answer::Status(500, "no".to_owned()));
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 1);
    assert!(baseline_lines(&rig).is_empty(), "nothing was queued");
    assert_eq!(status(&rig), pending());
    rig.hub().answer(Answer::Queued);
    look(&rig, 1_020).await;
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 2);
    assert_eq!(profile_installs(&rig).len(), 2);
    assert_eq!(baseline_lines(&rig), [DESKTOP_LINE, PROFILE_LINE]);
    look(&rig, 1_021).await;
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 2);
}

/// Catches: the baseline's state kept in memory only: after a restart a command sent again
/// that was unanswered, or its answer not matched.
#[tokio::test]
async fn the_baseline_survives_a_restart() {
    let mut rig = farm("baseline-restart", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    rig.restart().expect("the gate opens again");
    look(&rig, 1_015).await;
    assert_eq!(
        kinds(&rig),
        ["InstallProfile", "EnableRemoteDesktop", "InstallProfile"]
    );
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    ack(&rig, "InstallProfile", 1, 1_030).await;
    assert_eq!(status(&rig), applied());
    rig.restart().expect("the gate opens again");
    look(&rig, 1_035).await;
    assert_eq!(count(&rig, "ProfileList"), 1, "its answer is awaited");
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 1);
}

/// Catches: a `ProfileList` answer that is no list taken as a list (every profile then
/// "installed"), or the check repeated at every look after an error.
#[tokio::test]
async fn a_profile_list_that_is_no_list_is_an_error_answer() {
    let rig = farm("baseline-bad-list", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    ack(&rig, "InstallProfile", 1, 1_030).await;
    answer(&rig, "ProfileList", 0, "Acknowledged", 1_040, &[]).await;
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some("the baseline's profile list: the device answered an error")
    );
    assert_eq!(
        status(&rig),
        applied(),
        "an error of the check is no failure"
    );
    look(&rig, 1_040 + CHECK - 1).await;
    assert_eq!(count(&rig, "ProfileList"), 1);
    look(&rig, 1_040 + CHECK).await;
    assert_eq!(count(&rig, "ProfileList"), 2);
    answer(&rig, "ProfileList", 1, "Error", 1_700, &[]).await;
    look(&rig, 1_700 + CHECK - 1).await;
    assert_eq!(count(&rig, "ProfileList"), 2);
    look(&rig, 1_700 + CHECK).await;
    assert_eq!(count(&rig, "ProfileList"), 3);
}

/// Catches: an item of a `ProfileList` with no identifier, or that is no dictionary, read as an
/// installed profile; a `ProfileList` key that is no array read as a list.
#[tokio::test]
async fn a_profile_list_names_only_the_identifiers_it_has() {
    let rig = farm("baseline-list-items", true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    ack(&rig, "InstallProfile", 1, 1_030).await;
    let items = Value::Array(vec![
        Value::Dictionary(dict(&[("PayloadIdentifier", Value::from(PROFILE_ID))])),
        Value::Dictionary(dict(&[("PayloadDisplayName", Value::from("nameless"))])),
        Value::from("not a dictionary"),
    ]);
    answer(
        &rig,
        "ProfileList",
        0,
        "Acknowledged",
        1_040,
        &[("ProfileList", items)],
    )
    .await;
    let installed = rig.rig.record(UDID).baseline.expect("a baseline").installed;
    assert_eq!(installed, [PROFILE_ID]);
    assert_eq!(status(&rig), applied());

    look(&rig, 1_040 + CHECK).await;
    answer(
        &rig,
        "ProfileList",
        1,
        "Acknowledged",
        1_700,
        &[("ProfileList", Value::from("x"))],
    )
    .await;
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some("the baseline's profile list: the device answered an error")
    );
}

/// Catches: a `ProfileList` sent for a group that lists no profile (there is nothing to find
/// missing), or `EnableRemoteDesktop` not refreshed at the interval when it is the only entry.
#[tokio::test]
async fn a_group_of_remote_desktop_alone_sends_no_profile_list() {
    let mut rig = EnrollRig::new("baseline-desktop-only").await;
    rig.baseline(Some(
        parse(&toml(GROUP, DESKTOP))
            .expect("parses")
            .with_check_secs(CHECK),
    ));
    rig.fake.allow(SERIAL);
    rig.fake.group(SERIAL, GROUP);
    rig.enroll(SERIAL, true).await;
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(kinds(&rig), ["InstallProfile", "EnableRemoteDesktop"]);
    ack(&rig, "EnableRemoteDesktop", 0, 1_020).await;
    assert_eq!(status(&rig), applied());
    look(&rig, 1_020 + CHECK).await;
    assert_eq!(count(&rig, "EnableRemoteDesktop"), 2);
    assert_eq!(count(&rig, "ProfileList"), 0);
}

/// Catches: a baseline status shown for a gate with no baseline, or for a Mac that is not
/// enrolled.
#[tokio::test]
async fn there_is_no_status_without_a_baseline_or_an_enrolled_mac() {
    let rig = EnrollRig::new("baseline-none").await;
    rig.fake.allow(SERIAL);
    rig.fake.group(SERIAL, GROUP);
    rig.enroll(SERIAL, true).await;
    assert_eq!(status(&rig), None, "the gate has no baseline");
    assert!(rig.gate().baseline_groups().is_empty());
    assert_eq!(kinds(&rig), ["InstallProfile"]);

    let mut rig = farm("baseline-checked-out", true).await;
    rig.rig.post(&check_out(UDID)).await;
    assert_eq!(status(&rig), None);
    rig.baseline(None);
    assert_eq!(status(&rig), None);
}

/// Catches: a baseline command sent with a UUID that could not be drawn, or the failure not
/// kept.
#[tokio::test]
async fn no_random_bytes_send_nothing() {
    let (random, budget) = budgeted();
    let mut rig = EnrollRig::new("baseline-no-random").await;
    rig.baseline(Some(baseline_of(&rig, &profile_bytes(PROFILE_ID, "v1"))));
    rig.reconfigure(support::enrollment::config(), random);
    rig.fake.allow(SERIAL);
    rig.fake.group(SERIAL, GROUP);
    rig.enroll(SERIAL, true).await;
    budget.store(0, std::sync::atomic::Ordering::SeqCst);
    ack(&rig, "InstallProfile", 0, 1_010).await;
    assert_eq!(kinds(&rig), ["InstallProfile"]);
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some("no random bytes for a profile command")
    );
}

/// Catches: a baseline file that breaks a rule read as a baseline: each refusal with its exact
/// text.
#[test]
fn the_baseline_file_refuses_what_breaks_a_rule() {
    let dir = support::scratch("baseline-file");
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).expect("written");
        path
    };
    let good = profile_entry(&write(
        "good.mobileconfig",
        &profile_bytes(PROFILE_ID, "v1"),
    ));
    let other = profile_entry(&write(
        "other.mobileconfig",
        &profile_bytes("com.example.b", "b"),
    ));
    let twice = profile_entry(&write(
        "twice.mobileconfig",
        &profile_bytes(PROFILE_ID, "c"),
    ));
    let own = profile_entry(&write(
        "own.mobileconfig",
        &profile_bytes(&format!("{IDENTIFIER}.nonce"), "n"),
    ));
    let own_config = profile_entry(&write(
        "own-config.mobileconfig",
        &profile_bytes(&format!("{IDENTIFIER}.config"), "c"),
    ));
    let plain = write("plain.mobileconfig", b"not a property list");
    let kind = write(
        "kind.mobileconfig",
        &plist(dict(&[
            ("PayloadType", Value::from("com.apple.wifi.managed")),
            ("PayloadIdentifier", Value::from("com.example.c")),
        ])),
    );
    let nameless = write(
        "nameless.mobileconfig",
        &plist(dict(&[("PayloadType", Value::from("Configuration"))])),
    );
    let blank = write(
        "blank.mobileconfig",
        &plist(dict(&[
            ("PayloadType", Value::from("Configuration")),
            ("PayloadIdentifier", Value::from("")),
        ])),
    );
    let invalid = |why: &str| format!("--mdm-baseline-file: {why}");
    let cases: Vec<(&str, String, String)> = vec![
        (
            "no group",
            String::new(),
            invalid("the file lists no group ([[group]])"),
        ),
        (
            "not toml",
            "group = 1".to_owned(),
            invalid("invalid type: integer `1`, expected a sequence"),
        ),
        (
            "unknown key",
            toml(GROUP, &format!("colour = 1\n{DESKTOP}")),
            invalid("unknown field `colour`, expected `host_group` or `entry`"),
        ),
        (
            "no entry",
            toml(GROUP, ""),
            invalid("group farm-macs has no entry"),
        ),
        (
            "group twice",
            format!("{}{}", toml(GROUP, DESKTOP), toml(GROUP, DESKTOP)),
            invalid("group farm-macs is listed twice"),
        ),
        (
            "remote desktop false",
            toml(GROUP, "[[group.entry]]\nremote_desktop = false\n"),
            invalid(
                "group farm-macs: an entry is exactly one of remote_desktop = true and profile = \"<path>\"",
            ),
        ),
        (
            "empty entry",
            toml(GROUP, "[[group.entry]]\n"),
            invalid(
                "group farm-macs: an entry is exactly one of remote_desktop = true and profile = \"<path>\"",
            ),
        ),
        (
            "both",
            toml(
                GROUP,
                "[[group.entry]]\nremote_desktop = true\nprofile = \"/x\"\n",
            ),
            invalid(
                "group farm-macs: an entry is exactly one of remote_desktop = true and profile = \"<path>\"",
            ),
        ),
        (
            "remote desktop twice",
            toml(GROUP, &format!("{DESKTOP}{DESKTOP}")),
            invalid("group farm-macs: remote_desktop is listed twice"),
        ),
        (
            "identifier twice",
            toml(GROUP, &format!("{good}{twice}")),
            invalid("group farm-macs: the identifier com.example.fskit is listed twice"),
        ),
        (
            "gate's nonce profile",
            toml(GROUP, &own),
            invalid(&format!(
                "group farm-macs: profile {} has the identifier {IDENTIFIER}.nonce, which is the gate's own",
                dir.path().join("own.mobileconfig").display()
            )),
        ),
        (
            "gate's config profile",
            toml(GROUP, &own_config),
            invalid(&format!(
                "group farm-macs: profile {} has the identifier {IDENTIFIER}.config, which is the gate's own",
                dir.path().join("own-config.mobileconfig").display()
            )),
        ),
    ];
    for (what, text, why) in cases {
        let error = parse(&text).expect_err(what);
        assert!(matches!(error, BaselineError::Invalid(_)), "{what}");
        assert_eq!(error.to_string(), why, "{what}");
    }
    for (what, path, why) in [
        (
            "plain",
            &plain,
            "it is not an unsigned property list of a profile",
        ),
        ("kind", &kind, "its PayloadType is not Configuration"),
        ("nameless", &nameless, "it has no PayloadIdentifier"),
        ("blank", &blank, "it has no PayloadIdentifier"),
    ] {
        let error = parse(&toml(GROUP, &profile_entry(path))).expect_err(what);
        assert_eq!(
            error.to_string(),
            format!("--mdm-baseline-file: profile {}: {why}", path.display()),
            "{what}"
        );
    }

    // Two groups, a profile identifier repeated across them.
    let both = format!(
        "{}{}",
        toml(GROUP, &format!("{DESKTOP}{good}")),
        toml("lab", &format!("{good}{other}"))
    );
    let baseline = parse(&both).expect("parses").with_check_secs(9);
    assert_eq!(baseline.groups().collect::<Vec<_>>(), ["farm-macs", "lab"]);
    assert_eq!(baseline.group("lab").map(<[Item]>::len), Some(2));
    assert_eq!(baseline.group("none"), None);
    assert!(matches!(
        baseline.group(GROUP).expect("a group")[0],
        Item::RemoteDesktop
    ));
    let Item::Profile(profile) = &baseline.group(GROUP).expect("a group")[1] else {
        panic!("a profile");
    };
    assert_eq!(profile.identifier, PROFILE_ID);
    let digest: String = Sha256::digest(profile_bytes(PROFILE_ID, "v1"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        format!("{profile:?}"),
        format!("ProfileItem {{ identifier: \"{PROFILE_ID}\", sha256: \"{digest}\", .. }}")
    );
    assert_eq!(DEFAULT_CHECK_SECS, 21_600);
}

/// Catches: a baseline file or profile that someone but root could have written, or that is
/// missing, or not UTF-8, taken as what every Mac is configured with.
#[test]
fn the_baseline_file_and_its_profiles_must_be_roots_alone() {
    let dir = support::scratch("baseline-trust");
    let profile = dir.path().join("p.mobileconfig");
    std::fs::write(&profile, profile_bytes(PROFILE_ID, "v1")).expect("written");
    let file = dir.path().join("baseline.toml");
    std::fs::write(
        &file,
        toml(GROUP, &format!("{DESKTOP}{}", profile_entry(&profile))),
    )
    .expect("written");
    let untrusted = |path: &Path, why: &str| {
        format!(
            "--mdm-baseline-file {}: {why}; the baseline and its profiles must be files of root's that no one else can write",
            path.display()
        )
    };

    let loaded = Baseline::load(&StatFs::root(), &file, &identifier()).expect("root's");
    assert_eq!(loaded.groups().collect::<Vec<_>>(), [GROUP]);
    for (fs, why) in [
        (StatFs::new(501, 0o644, true), "it is not owned by root"),
        (
            StatFs::new(0, 0o664, true),
            "its group or others can write it",
        ),
        (
            StatFs::new(0, 0o602, true),
            "its group or others can write it",
        ),
        (StatFs::new(0, 0o644, false), "it is not a regular file"),
    ] {
        let error = Baseline::load(&fs, &file, &identifier()).expect_err("refused");
        assert!(matches!(error, BaselineError::Untrusted { .. }));
        assert_eq!(error.to_string(), untrusted(&file, why));
        // The same of a profile the file names.
        let text = toml(GROUP, &profile_entry(&profile));
        let error = Baseline::parse(&text, &fs, &identifier()).expect_err("refused");
        assert_eq!(error.to_string(), untrusted(&profile, why));
    }

    let missing = dir.path().join("missing.toml");
    let error = Baseline::load(&StatFs::root(), &missing, &identifier()).expect_err("missing");
    assert!(matches!(error, BaselineError::Read { .. }));
    assert!(
        error
            .to_string()
            .starts_with(&format!("--mdm-baseline-file {}: ", missing.display())),
        "{error}"
    );
    let binary = dir.path().join("binary.toml");
    std::fs::write(&binary, [0xff, 0xfe]).expect("written");
    let error = Baseline::load(&StatFs::root(), &binary, &identifier()).expect_err("not UTF-8");
    assert_eq!(
        error.to_string(),
        "--mdm-baseline-file: the file is not UTF-8"
    );
    // The real owner is the test's user, not root.
    let error = Baseline::load(&hostkeeper_enrollment::fs::RealFs, &file, &identifier())
        .expect_err("not root's");
    assert_eq!(
        error.to_string(),
        untrusted(&file, "it is not owned by root")
    );
}

// The shapes of the record file's messages as the release before the baseline wrote and read
// them: fields 1 to 20 only, a sent command of a type outside its five refused.
#[derive(Clone, PartialEq, prost::Message)]
struct FilePb {
    #[prost(uint32, tag = "1")]
    schema: u32,
    #[prost(bytes = "vec", tag = "2")]
    record: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    sha256: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct SentPb {
    #[prost(string, tag = "1")]
    uuid: String,
    #[prost(string, tag = "2")]
    request_type: String,
    #[prost(uint64, tag = "3")]
    sent_unix: u64,
    #[prost(bool, tag = "4")]
    queued: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ProfilePb {
    #[prost(string, tag = "1")]
    uuid: String,
    #[prost(uint64, tag = "2")]
    sent_unix: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
struct BaselineProfilePb {
    #[prost(string, tag = "1")]
    identifier: String,
    #[prost(message, optional, tag = "2")]
    command: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct BaselinePb {
    #[prost(message, optional, tag = "1")]
    remote_desktop: Option<ProfilePb>,
    #[prost(message, repeated, tag = "2")]
    profiles: Vec<BaselineProfilePb>,
}

/// A record as the release before reads it (`sent` only) and as this release also writes it.
#[derive(Clone, PartialEq, prost::Message)]
struct RecordPb {
    #[prost(string, tag = "1")]
    enrollment_id: String,
    #[prost(string, tag = "2")]
    udid: String,
    #[prost(uint32, tag = "5")]
    phase: u32,
    #[prost(uint32, tag = "6")]
    enrollment: u32,
    #[prost(uint64, tag = "11")]
    last_checkin_unix: u64,
    #[prost(message, repeated, tag = "15")]
    sent: Vec<SentPb>,
    #[prost(message, optional, tag = "21")]
    baseline: Option<BaselinePb>,
    #[prost(message, repeated, tag = "22")]
    baseline_sent: Vec<SentPb>,
}

const UUID_A: &str = "0A1B2C3D-AAAA-4BBB-8CCC-0123456789AB";
const UUID_B: &str = "0A1B2C3D-AAAA-4BBB-8CCC-0123456789AC";
const UUID_C: &str = "0A1B2C3D-AAAA-4BBB-8CCC-0123456789AD";
const UUID_D: &str = "0A1B2C3D-AAAA-4BBB-8CCC-0123456789AE";

fn uuid(text: &str) -> CommandUuid {
    CommandUuid::parse(text).expect("a UUID")
}

fn unanswered(text: &str, kind: &str) -> Sent {
    Sent {
        uuid: uuid(text),
        request_type: kind.to_owned(),
        sent_unix: 7,
        queued: true,
    }
}

fn command(text: &str) -> ProfileCommand {
    ProfileCommand::new(uuid(text), 7)
}

/// The record file's fields from its bytes.
fn read_file(bytes: &[u8]) -> (FilePb, RecordPb) {
    let file = FilePb::decode(bytes).expect("a file");
    let record = RecordPb::decode(file.record.as_slice()).expect("a record");
    (file, record)
}

fn file_of(record: &RecordPb) -> Vec<u8> {
    let bytes = record.encode_to_vec();
    FilePb {
        schema: 1,
        sha256: Sha256::digest(&bytes).to_vec(),
        record: bytes,
    }
    .encode_to_vec()
}

/// Catches: a record this release writes that the release before refuses or misreads (B8): its
/// unanswered `EnableRemoteDesktop` and `ProfileList` are in a field that release skips, and
/// every other field it reads is as it was; a record the release before wrote (no baseline)
/// read as one with none; a baseline whose command UUID is malformed taken as whole.
#[test]
fn the_release_before_the_baseline_reads_every_record_this_one_writes() {
    let mut record = DeviceRecord::new(
        hostkeeper_mdm_gate::EnrollmentId::parse(UDID).expect("an id"),
        5,
    );
    record.sent = vec![
        unanswered(UUID_A, "DeviceInformation"),
        unanswered(UUID_B, "InstallProfile"),
        unanswered(UUID_C, "EnableRemoteDesktop"),
        unanswered(UUID_D, "ProfileList"),
    ];
    let answered = ProfileCommand {
        answer: Some(ProfileAnswer {
            acknowledged: true,
            at_unix: 9,
        }),
        ..command(UUID_B)
    };
    record.baseline = Some(BaselineRecord {
        remote_desktop: Some(command(UUID_C)),
        profiles: vec![BaselineProfile {
            identifier: PROFILE_ID.to_owned(),
            command: answered,
        }],
        list: Some(command(UUID_D)),
        installed: vec![PROFILE_ID.to_owned()],
        checked_unix: Some(11),
    });
    let bytes = encode_record(&record);
    let place = Path::new("a.pb");
    assert_eq!(decode_record(place, UDID, &bytes).expect("reads"), record);

    // The release before: field 15 holds the types it knows, its digest holds, and fields 21
    // and 22 are unknown to it.
    let (file, old) = read_file(&bytes);
    assert_eq!(Sha256::digest(&file.record).to_vec(), file.sha256);
    let types: Vec<&str> = old
        .sent
        .iter()
        .map(|sent| sent.request_type.as_str())
        .collect();
    assert_eq!(types, ["DeviceInformation", "InstallProfile"]);
    assert_eq!(old.baseline_sent.len(), 2);

    // A record the release before wrote.
    let before = RecordPb {
        enrollment_id: UDID.to_owned(),
        udid: UDID.to_owned(),
        phase: 2,
        enrollment: 1,
        last_checkin_unix: 5,
        sent: vec![SentPb {
            uuid: UUID_A.to_owned(),
            request_type: "SecurityInfo".to_owned(),
            sent_unix: 1,
            queued: true,
        }],
        ..RecordPb::default()
    };
    let read = decode_record(place, UDID, &file_of(&before)).expect("reads");
    assert_eq!(read.baseline, None);
    assert_eq!(read.sent.len(), 1);

    let corrupt = |record: &RecordPb| match decode_record(place, UDID, &file_of(record))
        .expect_err("refused")
    {
        StoreError::Corrupt { reason, .. } => reason,
        other => panic!("{other}"),
    };
    let bad = ProfilePb {
        uuid: "not a uuid".to_owned(),
        sent_unix: 1,
    };
    let with = |baseline: BaselinePb| RecordPb {
        baseline: Some(baseline),
        ..before.clone()
    };
    assert_eq!(
        corrupt(&with(BaselinePb {
            remote_desktop: Some(bad.clone()),
            ..BaselinePb::default()
        })),
        "a baseline command's UUID is malformed"
    );
    assert_eq!(
        corrupt(&with(BaselinePb {
            profiles: vec![BaselineProfilePb {
                identifier: "x".to_owned(),
                command: Some(bad),
            }],
            ..BaselinePb::default()
        })),
        "a baseline command's UUID is malformed"
    );
    assert_eq!(
        corrupt(&with(BaselinePb {
            profiles: vec![BaselineProfilePb {
                identifier: "x".to_owned(),
                command: None,
            }],
            ..BaselinePb::default()
        })),
        "a baseline command's UUID is malformed"
    );
    let sent = |uuid: &str, kind: &str| SentPb {
        uuid: uuid.to_owned(),
        request_type: kind.to_owned(),
        sent_unix: 1,
        queued: true,
    };
    assert_eq!(
        corrupt(&RecordPb {
            baseline_sent: vec![sent("short", "ProfileList")],
            ..before.clone()
        }),
        "a sent command's UUID is malformed"
    );
    assert_eq!(
        corrupt(&RecordPb {
            baseline_sent: vec![sent(UUID_A, "EraseDevice")],
            ..before
        }),
        "a sent command's type is not one the gate sends"
    );
}
