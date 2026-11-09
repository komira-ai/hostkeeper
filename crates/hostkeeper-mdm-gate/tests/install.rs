//! The gate's install of the hostkeeper pkg against a fake NanoHUB and a fake of the
//! coordinator's enrollment, as design section 10 names its tests (task M14): the re-send of
//! 5.4 an hour after the Mac's last answer, none while a send is unanswered, none once a
//! candidate request appears or the Mac is approved, only an allowlisted digest whose file
//! holds it, and the inline manifest of 7.1. Time is the rig's clock; progress is the gate's own
//! look ([`Gate::tick`]) and the webhook events the test posts.

mod support;

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use hostkeeper_mdm_gate::install::{ACTION_INSTALL_SENT, INSTALL_RESEND_SECS};
use hostkeeper_mdm_gate::{Command, CommandUuid, PkgDigest, PkgEntry, pkg};
use plist::Dictionary;
use support::enrollment::{EnrollRig, HOST, request};
use support::{
    Answer, OTHER_UDID, SERIAL, UDID, authenticate, check_out, device_information, result,
    token_update,
};

/// The bytes of the pkg the tests list.
const PKG: &[u8] = b"the bytes of a hostkeeper pkg, version 0.1.66";

/// The bytes of another pkg.
const OTHER_PKG: &[u8] = b"the bytes of another hostkeeper pkg";

/// The directory of the pkg files of `rig`.
fn pkg_dir(rig: &EnrollRig) -> PathBuf {
    let dir = rig.rig.scratch.path().join("pkg");
    std::fs::create_dir_all(&dir).expect("the pkg directory");
    dir
}

/// Writes `bytes` as the pkg file named for `digest` in `rig`'s pkg directory; its path.
fn put(rig: &EnrollRig, digest: &PkgDigest, bytes: &[u8]) -> PathBuf {
    let path = pkg_dir(rig).join(digest.file_name());
    std::fs::write(&path, bytes).expect("the pkg file");
    path
}

/// One `[[pkg]]` entry of the allowlist.
fn entry(digest: &PkgDigest, version: &str, path: &std::path::Path) -> String {
    format!(
        "[[pkg]]\nsha256 = \"{digest}\"\nversion = \"{version}\"\npath = \"{}\"\n",
        path.display()
    )
}

/// A rig whose gate installs [`PKG`] (version 0.1.66, its file in place), its device [`UDID`]
/// enrolled at 1000 with [`SERIAL`] on the allowlist, `supervised`; the config profile is sent
/// and not yet answered.
async fn rig(name: &str, supervised: bool) -> EnrollRig {
    let mut rig = EnrollRig::new(name).await;
    let digest = PkgDigest::of(PKG);
    let path = put(&rig, &digest, PKG);
    rig.install(&entry(&digest, "0.1.66", &path));
    rig.fake.allow(SERIAL);
    rig.enroll(SERIAL, supervised).await;
    rig
}

/// The device's answer `status` to the last command of `kind`, at `now`.
async fn answer_last(rig: &EnrollRig, kind: &str, status: &str, now: u64) {
    rig.clock().set(now);
    let uuid = rig.last_uuid(kind);
    rig.rig.post(&result(UDID, Some(&uuid), status, &[])).await;
}

/// One look at `now`.
async fn look(rig: &EnrollRig, now: u64) {
    rig.clock().set(now);
    rig.gate().tick().await;
}

const INSTALL: &str = "InstallEnterpriseApplication";

/// The audit lines of the pkg's sends.
fn sends(rig: &EnrollRig) -> Vec<String> {
    rig.fake
        .audit_lines()
        .into_iter()
        .filter(|(action, _)| action == ACTION_INSTALL_SENT)
        .map(|(_, reason)| reason)
        .collect()
}

/// The audit line of send `n` of [`PKG`].
fn send_line(n: u32) -> String {
    format!(
        "serial {SERIAL}; sha256 {}; version 0.1.66; send {n}",
        PkgDigest::of(PKG)
    )
}

/// The digest the manifest of an `InstallEnterpriseApplication` names, and its URL.
fn asset(command: &Dictionary) -> (String, String) {
    let items = command["Manifest"].as_dictionary().expect("a Manifest")["items"]
        .as_array()
        .expect("items");
    let assets = items[0].as_dictionary().expect("an item")["assets"]
        .as_array()
        .expect("assets");
    let asset = assets[0].as_dictionary().expect("an asset");
    let text = |key: &str| asset[key].as_string().expect("text").to_owned();
    (text("sha256"), text("url"))
}

/// Catches: a pkg sent again before the hour after the Mac's last answer (at 3599 seconds),
/// or not sent again at the hour (3600 seconds), after an `Acknowledged` and after an `Error`;
/// a send not counted, or audited without its count; an `Error` answer not kept as the
/// device's last error.
#[tokio::test]
async fn the_pkg_is_resent_an_hour_after_its_last_answer() {
    let rig = rig("install-resend", true).await;
    assert!(
        rig.installs().is_empty(),
        "nothing before the config profile"
    );
    answer_last(&rig, "InstallProfile", "Acknowledged", 1_005).await;
    assert_eq!(rig.installs().len(), 1);
    assert_eq!(sends(&rig), [send_line(1)]);

    answer_last(&rig, INSTALL, "Acknowledged", 1_010).await;
    look(&rig, 1_010 + INSTALL_RESEND_SECS - 1).await;
    assert_eq!(rig.installs().len(), 1);
    look(&rig, 1_010 + INSTALL_RESEND_SECS).await;
    assert_eq!(rig.installs().len(), 2);
    assert_eq!(sends(&rig), [send_line(1), send_line(2)]);

    let answered = 1_010 + INSTALL_RESEND_SECS + 5;
    answer_last(&rig, INSTALL, "Error", answered).await;
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some("the pkg install: the device answered an error")
    );
    look(&rig, answered + INSTALL_RESEND_SECS - 1).await;
    assert_eq!(rig.installs().len(), 2);
    look(&rig, answered + INSTALL_RESEND_SECS).await;
    assert_eq!(rig.installs().len(), 3);
    assert_eq!(sends(&rig), [send_line(1), send_line(2), send_line(3)]);

    let install = rig.rig.record(UDID).install.expect("the sends");
    assert_eq!(install.sends, 3);
    assert_eq!(install.sha256, PkgDigest::of(PKG));
    assert_eq!(install.last.sent_unix, answered + INSTALL_RESEND_SECS);
    assert_eq!(install.last.answer, None);
}

/// Catches: a pkg sent again while the last send is unanswered (a Mac that is off), at a tick
/// past the hour or after a `NotNow`, which piles commands up in NanoHUB's queue; the count
/// surviving a restart wrong.
#[tokio::test]
async fn no_resend_while_the_previous_install_is_unanswered() {
    let mut rig = rig("install-unanswered", true).await;
    answer_last(&rig, "InstallProfile", "Acknowledged", 1_005).await;
    assert_eq!(rig.installs().len(), 1);
    for now in [
        1_005 + INSTALL_RESEND_SECS,
        1_005 + 2 * INSTALL_RESEND_SECS,
        1_005 + 100 * INSTALL_RESEND_SECS,
    ] {
        look(&rig, now).await;
    }
    answer_last(&rig, INSTALL, "NotNow", 1_005 + 101 * INSTALL_RESEND_SECS).await;
    look(&rig, 1_005 + 200 * INSTALL_RESEND_SECS).await;
    assert_eq!(rig.installs().len(), 1);

    // After a restart the send is still unanswered, and still not sent again; its answer is
    // matched, and the hour counts from it.
    rig.restart().expect("the gate opens");
    look(&rig, 1_005 + 300 * INSTALL_RESEND_SECS).await;
    assert_eq!(rig.installs().len(), 1);
    let answered = 1_005 + 301 * INSTALL_RESEND_SECS;
    answer_last(&rig, INSTALL, "Acknowledged", answered).await;
    look(&rig, answered + INSTALL_RESEND_SECS).await;
    assert_eq!(rig.installs().len(), 2);
    assert_eq!(sends(&rig), [send_line(1), send_line(2)]);
}

/// Catches: a pkg sent again once a candidate request is pending (the daemon runs), or once a
/// request claiming the serial was approved in this enrollment; a request that is no
/// candidate (another platform UUID, posted before the enrollment, a duplicate claim)
/// stopping the re-send; and a new enrollment that does not start the count again.
#[tokio::test]
async fn an_approved_mac_is_not_sent_the_pkg_again() {
    let rig = rig("install-approved", true).await;
    answer_last(&rig, "InstallProfile", "Acknowledged", 1_005).await;
    answer_last(&rig, INSTALL, "Acknowledged", 1_010).await;

    // Requests that are no candidate do not stop the re-send.
    rig.fake.wait(SERIAL, request(1, OTHER_UDID, 1_020));
    rig.fake.wait(SERIAL, request(2, UDID, 999));
    let mut duplicate = request(3, UDID, 1_030);
    duplicate.duplicate_claim = true;
    rig.fake.wait(SERIAL, duplicate);
    let resent = 1_010 + INSTALL_RESEND_SECS;
    look(&rig, resent).await;
    assert_eq!(rig.installs().len(), 2);
    assert!(rig.fake.issued().is_empty());

    // The daemon's request: a candidate. The pkg is not sent again; the nonce is pushed.
    answer_last(&rig, INSTALL, "Acknowledged", resent + 1).await;
    rig.fake.wait(SERIAL, request(4, UDID, resent + 2));
    look(&rig, resent + 1 + INSTALL_RESEND_SECS).await;
    assert_eq!(rig.installs().len(), 2);
    assert_eq!(rig.fake.issued().len(), 1);

    // Approved in this enrollment: nothing more, at any hour.
    let approved = resent + 2 + INSTALL_RESEND_SECS;
    rig.fake.approve(SERIAL, approved);
    for hours in 1..=3 {
        look(&rig, approved + hours * INSTALL_RESEND_SECS).await;
    }
    assert_eq!(rig.installs().len(), 2);
    assert_eq!(sends(&rig), [send_line(1), send_line(2)]);

    // Erased and enrolled anew: the approval is before this enrollment, its requests are
    // stale, and the count starts again at 1 once the new config profile is acknowledged.
    let again = approved + 10 * INSTALL_RESEND_SECS;
    rig.clock().set(again);
    rig.rig.post(&check_out(UDID)).await;
    rig.rig.post(&authenticate(UDID, Some(SERIAL))).await;
    rig.rig.post(&token_update(UDID, 1)).await;
    assert_eq!(rig.rig.record(UDID).install, None);
    let uuid = rig.rig.sent(UDID, "DeviceInformation");
    rig.rig
        .post(&device_information(UDID, &uuid, Some(SERIAL), Some(true)))
        .await;
    answer_last(&rig, "InstallProfile", "Acknowledged", again + 1).await;
    assert_eq!(rig.installs().len(), 3);
    assert_eq!(sends(&rig), [send_line(1), send_line(2), send_line(1)]);
}

/// Catches: a pkg sent whose file's bytes are not its listed digest (the Mac would refuse it,
/// or worse, the list would not bound what is sent), or whose file is missing or unreadable;
/// the reason not kept as the device's last error, or the record written again at each look
/// for the same reason; a digest sent that is not the newest listed (an older entry, or a pkg
/// file in the directory that no entry lists); and a fixed file not sent at the next look.
#[tokio::test]
async fn only_an_allowlisted_digest_is_sent() {
    let mut rig = EnrollRig::new("install-allowlisted").await;
    let listed = PkgDigest::of(PKG);
    let older = PkgDigest::of(OTHER_PKG);
    let path = put(&rig, &listed, b"bytes that are not the listed pkg");
    let older_path = put(&rig, &older, OTHER_PKG);
    // A pkg in the directory that no entry lists.
    let unlisted = PkgDigest::of(b"an unlisted pkg");
    put(&rig, &unlisted, b"an unlisted pkg");
    let text = format!(
        "{}{}",
        entry(&older, "0.1.9", &older_path),
        entry(&listed, "0.1.66", &path)
    );
    rig.install(&text);
    rig.fake.allow(SERIAL);
    rig.enroll(SERIAL, true).await;
    answer_last(&rig, "InstallProfile", "Acknowledged", 1_005).await;
    assert!(rig.installs().is_empty());
    let shown = path.display();
    let mismatch = format!(
        "the pkg {shown}'s bytes are {}, not its listed sha256; it is not sent",
        PkgDigest::of(b"bytes that are not the listed pkg")
    );
    assert_eq!(
        rig.rig.record(UDID).last_error.as_deref(),
        Some(mismatch.as_str())
    );
    let record = rig.rig.record(UDID);
    look(&rig, 1_006).await;
    assert_eq!(
        rig.rig.record(UDID),
        record,
        "the same reason is not written again"
    );

    std::fs::remove_file(&path).expect("removed");
    look(&rig, 1_007).await;
    assert_eq!(
        rig.rig.record(UDID).last_error,
        Some(format!("the pkg {shown} is missing"))
    );
    std::fs::create_dir(&path).expect("a directory in its place");
    look(&rig, 1_008).await;
    let error = rig.rig.record(UDID).last_error.expect("an error");
    assert!(
        error.starts_with(&format!("the pkg {shown} cannot be read: ")),
        "{error}"
    );
    assert!(rig.installs().is_empty());
    assert!(sends(&rig).is_empty());

    std::fs::remove_dir(&path).expect("removed");
    put(&rig, &listed, PKG);
    look(&rig, 1_009).await;
    let installs = rig.installs();
    assert_eq!(installs.len(), 1);
    assert_eq!(
        asset(&installs[0]),
        (
            listed.to_string(),
            format!("https://{HOST}/pkg/{listed}.pkg")
        )
    );
    assert_eq!(sends(&rig), [send_line(1)]);
}

/// Catches: a manifest the Mac cannot check or would fetch elsewhere: a `ManifestURL` in
/// place of the inline manifest, a `sha256` that is not the allowlisted digest, a URL that
/// does not name it, chunked hashes, metadata a pkg install lacks, or any other key; and a
/// command type off the gate's list.
#[test]
fn the_manifest_carries_the_digest_the_mac_checks() {
    let digest = PkgDigest::of(PKG);
    let entry = PkgEntry {
        sha256: digest.clone(),
        version: hostkeeper_wire::version::Version::parse("0.1.66").expect("a version"),
        path: PathBuf::from(format!("/var/lib/hostkeeper-mdm-pkg/{digest}.pkg")),
    };
    let host = hostkeeper_mdm_gate::MdmHost::parse("MDM.Example.com").expect("a host");
    let uuid = CommandUuid::from_random([0xAB; 16]);
    let command = Command::install_application(uuid, pkg::manifest(&entry, &host));
    assert!(command.is_allowed());
    assert_eq!(command.request_type(), INSTALL);
    let text = String::from_utf8(command.to_plist()).expect("UTF-8");
    let golden = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Command</key>
	<dict>
		<key>Manifest</key>
		<dict>
			<key>items</key>
			<array>
				<dict>
					<key>assets</key>
					<array>
						<dict>
							<key>kind</key>
							<string>software-package</string>
							<key>sha256</key>
							<string>{digest}</string>
							<key>url</key>
							<string>https://mdm.example.com/pkg/{digest}.pkg</string>
						</dict>
					</array>
					<key>metadata</key>
					<dict>
						<key>bundle-identifier</key>
						<string>dev.hostkeeper.hostkeeper</string>
						<key>bundle-version</key>
						<string>0.1.66</string>
						<key>kind</key>
						<string>software</string>
						<key>title</key>
						<string>hostkeeper 0.1.66</string>
					</dict>
				</dict>
			</array>
		</dict>
		<key>RequestType</key>
		<string>InstallEnterpriseApplication</string>
	</dict>
	<key>CommandUUID</key>
	<string>ABABABAB-ABAB-4BAB-ABAB-ABABABABABAB</string>
</dict>
</plist>"#
    );
    assert_eq!(text, golden);
    // The digest is the whole file's SHA-256 in lower case, as `shasum -a 256` prints it.
    assert_eq!(
        PkgDigest::of(b"").as_str(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

/// Catches: a pkg sent before the Mac acknowledged its config profile (the daemon's start
/// would find no managed config), or after an error answer to it; a pkg held
/// back from an unsupervised Mac (it needs none, and is approved by an operator); a gate
/// without the pkg install sending one.
#[tokio::test]
async fn the_pkg_waits_for_the_config_profile_and_needs_no_supervision() {
    let rig = rig("install-config-first", false).await;
    look(&rig, 1_002).await;
    assert!(rig.installs().is_empty());
    answer_last(&rig, "InstallProfile", "Error", 1_003).await;
    look(&rig, 1_004).await;
    assert!(rig.installs().is_empty());
    // The config profile is sent again an hour after its error; once acknowledged, the pkg.
    look(&rig, 1_003 + 3_600).await;
    answer_last(&rig, "InstallProfile", "Acknowledged", 1_003 + 3_601).await;
    assert_eq!(rig.installs().len(), 1);
    assert!(rig.fake.issued().is_empty());

    let without = EnrollRig::new("install-none").await;
    without.fake.allow(SERIAL);
    without.enroll(SERIAL, true).await;
    answer_last(&without, "InstallProfile", "Acknowledged", 1_005).await;
    look(&without, 1_005 + 10 * INSTALL_RESEND_SECS).await;
    assert!(without.installs().is_empty());
}

/// Catches: a send NanoHUB did not queue counted or audited as sent, or not sent again at
/// the next look (it is in no queue); the enrollment's failure to list requests read as no
/// candidate (a pkg sent to a Mac whose daemon may run); no random bytes for a UUID sending
/// a command with none.
#[tokio::test]
async fn a_send_nanohub_did_not_queue_is_sent_at_the_next_look() {
    let rig = rig("install-unqueued", true).await;
    rig.hub().answer(Answer::Status(500, "down".to_owned()));
    answer_last(&rig, "InstallProfile", "Acknowledged", 1_005).await;
    assert_eq!(rig.installs().len(), 1, "asked once");
    assert!(sends(&rig).is_empty());
    let install = rig.rig.record(UDID).install.expect("the try");
    assert_eq!((install.sends, install.last.answer), (0, None));
    assert!(rig.rig.record(UDID).sent.is_empty());

    rig.hub().answer(Answer::Queued);
    rig.fake.fail_waiting(true);
    look(&rig, 1_006).await;
    assert_eq!(rig.installs().len(), 1, "no candidate list, no send");
    rig.fake.fail_waiting(false);
    look(&rig, 1_007).await;
    assert_eq!(rig.installs().len(), 2);
    assert_eq!(sends(&rig), [send_line(1)]);

    let mut starved = EnrollRig::new("install-no-random").await;
    let (random, budget) = support::budgeted();
    let digest = PkgDigest::of(PKG);
    let path = put(&starved, &digest, PKG);
    starved.install_with(&entry(&digest, "0.1.66", &path), random);
    starved.fake.allow(SERIAL);
    starved.enroll(SERIAL, true).await;
    budget.store(0, Ordering::SeqCst);
    answer_last(&starved, "InstallProfile", "Acknowledged", 1_005).await;
    assert!(starved.installs().is_empty());
    assert_eq!(
        starved.rig.record(UDID).last_error.as_deref(),
        Some("no random bytes for a profile command")
    );
    budget.store(-1, Ordering::SeqCst);
    look(&starved, 1_006).await;
    assert_eq!(starved.installs().len(), 1);
}
