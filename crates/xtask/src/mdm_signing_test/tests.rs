//! The tests of the kit's description, the commands and `send`.

use std::cell::RefCell;
use std::path::Path;

use plist::Value;
use sha2::{Digest, Sha256};

use super::commands::{MdmHost, ProfileIdentifier, command_uuid, root_profile};
use super::nanohub::{Enqueue, EnrollmentId, Queued};
use super::results::{Answer, Outcome, RESULTS_FILE, Results, record};
use super::{
    Error, KIT_FILE, Kit, KitPkg, PKG_DIR, PKGS, PROFILE_FILE, RootProfile, Step, Target, Verb,
    run, send,
};
use crate::release::fakes::scratch;
use crate::release::ports::{Exec, Ran};
use crate::testdir::TestDir;

/// A NanoHUB that queues every command, or answers with `refuse`, and records each call.
#[derive(Default)]
pub(super) struct FakeHub {
    pub(super) calls: RefCell<Vec<(String, Vec<u8>)>>,
    refuse: Option<Error>,
    push_error: Option<String>,
}

impl Enqueue for FakeHub {
    fn enqueue(&self, id: &EnrollmentId, command: &[u8]) -> Result<Queued, Error> {
        self.calls
            .borrow_mut()
            .push((id.as_str().to_owned(), command.to_vec()));
        match &self.refuse {
            Some(e) => Err(e.clone()),
            None => Ok(Queued {
                push_error: self.push_error.clone(),
            }),
        }
    }
}

/// An `Exec` no test of `send` may reach.
struct NoExec;

impl Exec for NoExec {
    fn spawn(&self, program: &str, _: &[String]) -> std::io::Result<Ran> {
        Err(std::io::Error::other(format!("ran {program}")))
    }
}

const PROFILE: &[u8] = b"<plist>the root's profile</plist>";

/// A kit as `build` leaves it, with pkgs whose bytes are `pkg <n>`.
pub(super) fn kit(name: &str) -> TestDir {
    let dir = scratch(name);
    std::fs::create_dir(dir.join(PKG_DIR)).expect("pkg dir");
    std::fs::write(dir.join(PROFILE_FILE), PROFILE).expect("profile");
    let mut pkgs = Vec::new();
    for spec in &PKGS {
        let bytes = format!("pkg {}", spec.number);
        let sha256 = hex::encode(Sha256::digest(bytes.as_bytes()));
        std::fs::write(dir.join(PKG_DIR).join(format!("{sha256}.pkg")), bytes).expect("pkg");
        pkgs.push(KitPkg {
            step: spec.step,
            identifier: spec.identifier(),
            version: "1".to_owned(),
            sha256,
            build_check_signature: String::new(),
        });
    }
    let kit = Kit {
        profile_identifier: "com.example.hostkeeper.mdm".to_owned(),
        root_profile: RootProfile {
            identifier: "com.example.hostkeeper.mdm.signing-test.root".to_owned(),
            sha256: hex::encode(Sha256::digest(PROFILE)),
            root_sha256: "00".repeat(32),
        },
        pkgs,
    };
    std::fs::write(dir.join(KIT_FILE), toml::to_string(&kit).expect("kit.toml"))
        .expect("write kit.toml");
    dir
}

fn mac() -> EnrollmentId {
    "0000FE00-ABCDEF".parse().expect("an id")
}

fn host() -> MdmHost {
    "mdm.example.net".parse().expect("a host")
}

/// Sends `step` through `hub`.
fn send_to(dir: &Path, step: Step, hub: &FakeHub) -> Result<Vec<String>, Error> {
    let (id, host) = (mac(), host());
    let target = Target {
        enrollment_id: &id,
        mdm_host: &host,
    };
    send(dir, step, &target, Some(hub))
}

/// The `Command` dictionary of the plist `bytes`, and its `CommandUUID`.
fn command_of(bytes: &[u8]) -> (plist::Dictionary, String) {
    let value = Value::from_reader_xml(bytes).expect("an XML plist");
    let top = value.into_dictionary().expect("a dictionary");
    assert_eq!(
        top.keys().collect::<Vec<_>>(),
        ["Command", "CommandUUID"],
        "the plist's keys"
    );
    let uuid = top["CommandUUID"].as_string().expect("a uuid").to_owned();
    (
        top["Command"].as_dictionary().expect("a command").clone(),
        uuid,
    )
}

/// Records `answer` for the last send of `step`.
fn answer(dir: &Path, step: Step, answer: Answer, installed: Option<bool>) {
    record(
        dir,
        step,
        Outcome {
            answer,
            installed,
            trusted: None,
            note: None,
        },
    )
    .expect("recorded");
}

#[test]
fn the_pkg_command_carries_the_digest_the_mac_checks_and_names_it_in_the_url() {
    // Catches: a manifest whose `sha256` is not the pkg's whole-file digest, whose URL names
    // another file, a manifest URL in place of the inline manifest, or metadata a pkg install
    // needs missing (design section 7.1).
    let dir = kit("signing-pkg-command");
    let hub = FakeHub::default();
    send_to(&dir, Step::Pkg2, &hub).expect("sent");
    let calls = hub.calls.borrow();
    let [(id, body)] = calls.as_slice() else {
        panic!("{} calls", calls.len())
    };
    assert_eq!(id, "0000FE00-ABCDEF");
    let (command, _) = command_of(body);
    let sha = hex::encode(Sha256::digest(b"pkg 2"));
    let mut want = plist::Dictionary::new();
    want.insert("RequestType".into(), "InstallEnterpriseApplication".into());
    let asset: plist::Dictionary = [
        ("kind", Value::from("software-package")),
        ("sha256", Value::from(sha.as_str())),
        (
            "url",
            Value::from(format!("https://mdm.example.net/pkg/{sha}.pkg")),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let metadata: plist::Dictionary = [
        ("bundle-identifier", "dev.hostkeeper.signing-test.pkg2"),
        ("bundle-version", "1"),
        ("kind", "software"),
        (
            "title",
            "hostkeeper signing test pkg 2: unsigned distribution pkg",
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), Value::from(v)))
    .collect();
    let item: plist::Dictionary = [
        ("assets".to_owned(), Value::Array(vec![asset.into()])),
        ("metadata".to_owned(), metadata.into()),
    ]
    .into_iter()
    .collect();
    let manifest: plist::Dictionary = [("items".to_owned(), Value::Array(vec![item.into()]))]
        .into_iter()
        .collect();
    want.insert("Manifest".into(), manifest.into());
    assert_eq!(command, want);
}

#[test]
fn the_root_profile_command_carries_the_kit_profile_and_remove_names_it() {
    // Catches: InstallProfile sending other bytes than the profile the kit's digest names, or
    // RemoveProfile naming another identifier than the profile's.
    let dir = kit("signing-profile-command");
    let hub = FakeHub::default();
    send_to(&dir, Step::RootProfile, &hub).expect("sent");
    send_to(&dir, Step::RemoveRootProfile, &hub).expect("sent");
    let calls = hub.calls.borrow();
    let (install, _) = command_of(&calls[0].1);
    assert_eq!(install["RequestType"].as_string(), Some("InstallProfile"));
    assert_eq!(install["Payload"].as_data(), Some(PROFILE));
    assert_eq!(install.len(), 2);
    let (remove, _) = command_of(&calls[1].1);
    assert_eq!(remove["RequestType"].as_string(), Some("RemoveProfile"));
    assert_eq!(
        remove["Identifier"].as_string(),
        Some("com.example.hostkeeper.mdm.signing-test.root")
    );
    assert_eq!(remove.len(), 2);
}

#[test]
fn a_dry_run_prints_the_command_and_sends_and_writes_nothing() {
    // Catches: a send without --apply reaching NanoHUB, reading the API key, or recording a
    // send that did not happen.
    let dir = kit("signing-dry");
    let verb = Verb::Send {
        kit: dir.to_path_buf(),
        step: Step::Pkg1,
        enrollment_id: mac(),
        api_url: "http://127.0.0.1:9004".parse().expect("url"),
        api_key_file: dir.join("no-such-key"),
        mdm_host: host(),
        apply: false,
    };
    let lines = run(verb, &NoExec, |_, _| -> FakeHub { panic!("connected") }).expect("a dry run");
    assert_eq!(lines.len(), 3);
    assert!(
        lines[0].starts_with("pkg-1 to 0000FE00-ABCDEF: command "),
        "{}",
        lines[0]
    );
    let (command, uuid) = command_of(lines[1].as_bytes());
    assert_eq!(
        lines[0],
        format!("pkg-1 to 0000FE00-ABCDEF: command {uuid}")
    );
    assert_eq!(
        command["RequestType"].as_string(),
        Some("InstallEnterpriseApplication")
    );
    assert_eq!(lines[2], "dry run: nothing sent; add --apply to send it");
    assert!(!dir.join(RESULTS_FILE).exists());
}

#[test]
fn an_applied_send_reads_the_key_connects_and_records_the_send() {
    // Catches: --apply not sending, the key file's text not reaching the client, or the send
    // not appended to results.toml with its command UUID.
    use std::os::unix::fs::PermissionsExt;
    let dir = kit("signing-apply");
    let key = dir.join("key");
    std::fs::write(&key, "the-key\n").expect("key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let verb = Verb::Send {
        kit: dir.to_path_buf(),
        step: Step::RootProfile,
        enrollment_id: mac(),
        api_url: "http://127.0.0.1:9004".parse().expect("url"),
        api_key_file: key,
        mdm_host: host(),
        apply: true,
    };
    let seen = RefCell::new(None);
    let lines = run(verb, &NoExec, |url, key| {
        *seen.borrow_mut() = Some((url.addr().to_string(), key));
        FakeHub::default()
    })
    .expect("sent");
    assert_eq!(
        seen.into_inner(),
        Some(("127.0.0.1:9004".to_owned(), "the-key".to_owned()))
    );
    let results = Results::read(&dir).expect("results");
    let [sent] = results.sends.as_slice() else {
        panic!("{:?}", results.sends)
    };
    assert_eq!(sent.step, Step::RootProfile);
    assert_eq!(sent.push_error, None);
    assert_eq!(
        lines,
        [
            format!(
                "root-profile to 0000FE00-ABCDEF: command {}",
                sent.command_uuid
            ),
            "queued and pushed".to_owned(),
            format!(
                "find the answer: the NanoHUB unit's log line with command_uuid={}, then \
                 `mdm-signing-test record --step root-profile`",
                sent.command_uuid
            ),
        ]
    );
}

#[test]
fn a_failed_push_is_recorded_and_said() {
    // Catches: a command queued without its push read as not sent (so sent twice), or the
    // failure hidden from the operator, who must wait for the Mac's next check-in.
    let dir = kit("signing-push");
    let hub = FakeHub {
        push_error: Some("APNs refused the topic".to_owned()),
        ..FakeHub::default()
    };
    let lines = send_to(&dir, Step::Pkg1, &hub).expect("queued");
    assert_eq!(
        lines[1],
        "queued; the push failed (APNs refused the topic): the Mac fetches it at its next check-in"
    );
    let results = Results::read(&dir).expect("results");
    assert_eq!(
        results.sends[0].push_error.as_deref(),
        Some("APNs refused the topic")
    );
}

#[test]
fn a_command_nanohub_refuses_is_not_recorded() {
    // Catches: a send NanoHUB did not queue written to results.toml, so a record or the report
    // would treat it as sent.
    let dir = kit("signing-refused");
    let hub = FakeHub {
        refuse: Some(Error::NanoHub("401".to_owned())),
        ..FakeHub::default()
    };
    assert_eq!(
        send_to(&dir, Step::Pkg1, &hub),
        Err(Error::NanoHub("401".to_owned()))
    );
    assert!(!dir.join(RESULTS_FILE).exists());
}

#[test]
fn each_send_has_its_own_command_uuid() {
    // Catches: a re-send of a step reusing its UUID, which NanoMDM would file over the first
    // and whose log line could not be told from the first's.
    let dir = kit("signing-uuid");
    let hub = FakeHub::default();
    for step in [Step::Pkg1, Step::Pkg1, Step::Pkg2] {
        send_to(&dir, step, &hub).expect("sent");
    }
    let results = Results::read(&dir).expect("results");
    let uuids: Vec<&str> = results
        .sends
        .iter()
        .map(|s| s.command_uuid.as_str())
        .collect();
    let mut unique = uuids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 3, "{uuids:?}");
    for (sent, (_, body)) in results.sends.iter().zip(hub.calls.borrow().iter()) {
        assert_eq!(command_of(body).1, sent.command_uuid);
    }
    let read = Kit::read(&dir).expect("kit");
    assert_eq!(uuids[0], command_uuid(&read, Step::Pkg1, 0));
    assert_eq!(uuids[1], command_uuid(&read, Step::Pkg1, 1));
    let shape = uuids[0].split('-').map(str::len).collect::<Vec<_>>();
    assert_eq!(shape, [8, 4, 4, 4, 12]);
    assert!(
        uuids[0]
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_digit() || b.is_ascii_uppercase())
    );
}

#[test]
fn pkg_3_and_4_wait_for_the_root_profile_acknowledged_and_not_removed() {
    // Catches: pkg 3 sent before the root is on the Mac (a refusal that would be read as the
    // identity's), or after it was removed.
    let dir = kit("signing-order");
    let hub = FakeHub::default();
    let refused = |step: Step| {
        Err(Error::Refused(format!(
            "{} needs the root's profile on the Mac: send root-profile and record it \
             Acknowledged first, with no remove-root-profile after it",
            step.name()
        )))
    };
    assert_eq!(send_to(&dir, Step::Pkg3, &hub), refused(Step::Pkg3));
    send_to(&dir, Step::RootProfile, &hub).expect("sent");
    assert_eq!(send_to(&dir, Step::Pkg4, &hub), refused(Step::Pkg4));
    answer(&dir, Step::RootProfile, Answer::Error, None);
    assert_eq!(send_to(&dir, Step::Pkg3, &hub), refused(Step::Pkg3));
    answer(&dir, Step::RootProfile, Answer::Acknowledged, None);
    send_to(&dir, Step::Pkg3, &hub).expect("pkg 3 after the root");
    send_to(&dir, Step::RemoveRootProfile, &hub).expect("removed");
    assert_eq!(send_to(&dir, Step::Pkg4, &hub), refused(Step::Pkg4));
    send_to(&dir, Step::RootProfile, &hub).expect("sent again");
    answer(&dir, Step::RootProfile, Answer::Acknowledged, None);
    send_to(&dir, Step::Pkg4, &hub).expect("pkg 4 after the root again");
    // Pkg 1 and 2, and the profile steps, have no order.
    let other = kit("signing-order-free");
    for step in [Step::RemoveRootProfile, Step::Pkg2, Step::Pkg1] {
        send_to(&other, step, &hub).expect("no order");
    }
}

#[test]
fn a_pkg_or_profile_whose_bytes_are_not_its_digest_is_refused() {
    // Catches: a file changed after the build (or a copy cut short) sent under the digest the
    // Mac then refuses, read as a refusal of its signature.
    let dir = kit("signing-digest");
    let read = Kit::read(&dir).expect("kit");
    let pkg = &read.pkgs[0];
    let path = dir.join(PKG_DIR).join(format!("{}.pkg", pkg.sha256));
    std::fs::write(&path, "changed").expect("change");
    let hub = FakeHub::default();
    assert_eq!(
        send_to(&dir, Step::Pkg1, &hub),
        Err(Error::Refused(format!(
            "{} has the SHA-256 {}, not {} as kit.toml says",
            path.display(),
            hex::encode(Sha256::digest(b"changed")),
            pkg.sha256
        )))
    );
    std::fs::remove_file(&path).expect("remove");
    assert!(matches!(
        send_to(&dir, Step::Pkg1, &hub),
        Err(Error::Io { .. })
    ));
    std::fs::write(dir.join(PROFILE_FILE), "changed").expect("change");
    assert!(matches!(
        send_to(&dir, Step::RootProfile, &hub),
        Err(Error::Refused(why)) if why.contains("as kit.toml says")
    ));
    assert!(hub.calls.borrow().is_empty());
}

#[test]
fn a_kit_toml_that_is_missing_malformed_or_not_four_pkgs_is_refused() {
    // Catches: a send from a directory that is not a kit, a kit.toml with a field it does not
    // know (from another version), or one whose pkgs are not pkg-1 to pkg-4 in order.
    let dir = scratch("signing-kit-bad");
    assert!(matches!(Kit::read(&dir), Err(Error::Io { what, .. }) if what.contains("made by")));
    std::fs::write(dir.join(KIT_FILE), "profile_identifier = 1\n").expect("write");
    assert!(matches!(Kit::read(&dir), Err(Error::Refused(_))));
    let good = kit("signing-kit-reorder");
    let text = std::fs::read_to_string(good.join(KIT_FILE)).expect("read");
    let mut kit = toml::from_str::<Kit>(&text).expect("parse");
    kit.pkgs.swap(0, 1);
    std::fs::write(good.join(KIT_FILE), toml::to_string(&kit).expect("toml")).expect("write");
    assert_eq!(
        Kit::read(&good),
        Err(Error::Refused(format!(
            "{}: the pkgs are pkg-2, pkg-1, pkg-3, pkg-4, not pkg-1 to pkg-4 in order",
            good.join(KIT_FILE).display()
        )))
    );
    std::fs::write(good.join(KIT_FILE), format!("{text}extra = 1\n")).expect("write");
    assert!(matches!(Kit::read(&good), Err(Error::Refused(why)) if why.contains("extra")));
}

#[test]
fn the_root_profile_installs_one_root_system_wide() {
    // Catches: a profile with another payload type, scoped to the user, without the DER root,
    // or whose UUIDs change between two builds of the same root.
    let der = b"a DER certificate";
    let bytes = root_profile("com.example.x.signing-test.root", der);
    assert_eq!(bytes, root_profile("com.example.x.signing-test.root", der));
    let top = Value::from_reader_xml(bytes.as_slice())
        .expect("plist")
        .into_dictionary()
        .expect("dict");
    assert_eq!(top["PayloadType"].as_string(), Some("Configuration"));
    assert_eq!(top["PayloadScope"].as_string(), Some("System"));
    assert_eq!(
        top["PayloadIdentifier"].as_string(),
        Some("com.example.x.signing-test.root")
    );
    let payloads = top["PayloadContent"].as_array().expect("payloads");
    let [cert] = payloads.as_slice() else {
        panic!("{} payloads", payloads.len())
    };
    let cert = cert.as_dictionary().expect("payload");
    assert_eq!(
        cert["PayloadType"].as_string(),
        Some("com.apple.security.root")
    );
    assert_eq!(cert["PayloadContent"].as_data(), Some(&der[..]));
    assert_eq!(
        cert["PayloadIdentifier"].as_string(),
        Some("com.example.x.signing-test.root.cert")
    );
    assert_ne!(cert["PayloadUUID"], top["PayloadUUID"]);
    let other = root_profile("com.example.x.signing-test.root", b"another root");
    assert_ne!(bytes, other);
}

#[test]
fn the_names_on_the_command_line_are_checked() {
    // Catches: a URL, a port or a path given as the MDM host (a pkg URL the proxy's
    // certificate does not name), or a profile identifier that is not reverse-DNS.
    assert_eq!(
        "MDM.Example.net"
            .parse::<MdmHost>()
            .map(|h| h.pkg_url("ab")),
        Ok("https://mdm.example.net/pkg/ab.pkg".to_owned())
    );
    for bad in [
        "https://mdm.example.net",
        "mdm.example.net:443",
        "mdm",
        "a..b",
        "-a.b",
        "a-.b",
        "",
    ] {
        assert!(bad.parse::<MdmHost>().is_err(), "{bad}");
    }
    assert!(format!("{}.b", "a".repeat(64)).parse::<MdmHost>().is_err());
    assert!(
        format!("{}.b", "a.".repeat(126))
            .parse::<MdmHost>()
            .is_err()
    );
    let id: ProfileIdentifier = "com.example.hostkeeper.mdm".parse().expect("id");
    assert_eq!(
        id.root_profile(),
        "com.example.hostkeeper.mdm.signing-test.root"
    );
    for bad in ["com", "com..x", "com.ex ample", ""] {
        assert!(bad.parse::<ProfileIdentifier>().is_err(), "{bad}");
    }
    assert_eq!(
        "com".parse::<ProfileIdentifier>(),
        Err(
            "\"com\" is not a reverse-DNS identifier (labels of letters, digits and -, joined \
             by dots)"
                .to_owned()
        )
    );
}

#[test]
fn the_steps_name_themselves_as_the_command_line_does() {
    // Catches: a step whose name in the files differs from the flag's value, so a recorded
    // step could not be read back or named.
    use clap::ValueEnum;
    for step in Step::value_variants() {
        let flag = step.to_possible_value().expect("a value");
        assert_eq!(flag.get_name(), step.name());
        let text = toml::to_string(&Results {
            sends: vec![super::results::QueuedSend {
                step: *step,
                command_uuid: "U".to_owned(),
                push_error: None,
                outcome: None,
            }],
        })
        .expect("toml");
        assert!(
            text.contains(&format!("step = \"{}\"", step.name())),
            "{text}"
        );
    }
    assert_eq!(
        PKGS.iter().map(|p| p.step).collect::<Vec<_>>(),
        [Step::Pkg1, Step::Pkg2, Step::Pkg3, Step::Pkg4]
    );
    assert_eq!(Step::RootProfile.pkg(), None);
}

#[test]
fn a_send_queued_but_not_recorded_names_its_command_uuid() {
    // Catches: a command NanoHUB queued while results.toml could not be written reported as a
    // plain write failure, so the operator would not know it is on its way to the Mac.
    let dir = kit("signing-unrecorded");
    std::fs::create_dir(dir.join(format!("{RESULTS_FILE}.partial"))).expect("block the write");
    let hub = FakeHub::default();
    let got = send_to(&dir, Step::Pkg1, &hub);
    let uuid = command_uuid(&Kit::read(&dir).expect("kit"), Step::Pkg1, 0);
    let Err(Error::Refused(why)) = got else {
        panic!("{got:?}")
    };
    assert!(
        why.starts_with(&format!(
            "NanoHUB queued command {uuid} for pkg-1, but it could not be recorded ("
        )),
        "{why}"
    );
    assert!(
        why.ends_with("): record it by hand before the next send of this step"),
        "{why}"
    );
    assert_eq!(hub.calls.borrow().len(), 1);
}
