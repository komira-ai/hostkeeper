//! The baseline file and the record it adds to (docs/design/hostkeeper-mdm.md section 6.4): each
//! rule of the file with its exact refusal, the files that must be root's, and the record's
//! compatibility with the release before.

mod support;

use std::path::Path;

use hostkeeper_mdm_gate::{
    Baseline, BaselineError, BaselineInstall, BaselineProfile, BaselineRecord, CommandUuid,
    DEFAULT_CHECK_SECS, DeviceRecord, Item, ProfileAnswer, ProfileCommand, Sent, StoreError,
    decode_record, encode_record,
};
use plist::Value;
use prost::Message as _;
use sha2::{Digest as _, Sha256};
use support::baseline::{
    DESKTOP, GROUP, PROFILE_ID, identifier, parse, profile_bytes, profile_entry, toml,
};
use support::enrollment::{IDENTIFIER, StatFs};
use support::{UDID, dict, plist};

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
struct BaselineInstallPb {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, optional, tag = "2")]
    command: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct BaselinePb {
    #[prost(message, optional, tag = "1")]
    remote_desktop: Option<ProfilePb>,
    #[prost(message, repeated, tag = "2")]
    profiles: Vec<BaselineProfilePb>,
    /// Fields 6 and 7 are the device name's `Settings` and the installs.
    #[prost(message, optional, tag = "6")]
    name: Option<ProfilePb>,
    #[prost(message, repeated, tag = "7")]
    installs: Vec<BaselineInstallPb>,
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
const UUID_E: &str = "0A1B2C3D-AAAA-4BBB-8CCC-0123456789AF";
const UUID_F: &str = "0A1B2C3D-AAAA-4BBB-8CCC-0123456789B0";

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
        unanswered(UUID_F, "InstallEnterpriseApplication"),
        unanswered(UUID_C, "EnableRemoteDesktop"),
        unanswered(UUID_D, "ProfileList"),
        unanswered(UUID_E, "Settings"),
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
        name: Some(command(UUID_E).with_settings("a".repeat(64))),
        installs: vec![BaselineInstall {
            name: "tools".to_owned(),
            command: command(UUID_F).with_settings("b".repeat(64)),
        }],
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
    assert_eq!(
        types,
        [
            "DeviceInformation",
            "InstallProfile",
            "InstallEnterpriseApplication"
        ]
    );
    // `EnableRemoteDesktop`, `ProfileList` and `Settings`.
    assert_eq!(old.baseline_sent.len(), 3);

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
                command: Some(bad.clone()),
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
    for baseline in [
        BaselinePb {
            name: Some(bad.clone()),
            ..BaselinePb::default()
        },
        BaselinePb {
            installs: vec![BaselineInstallPb {
                name: "tools".to_owned(),
                command: Some(bad.clone()),
            }],
            ..BaselinePb::default()
        },
        BaselinePb {
            installs: vec![BaselineInstallPb {
                name: "tools".to_owned(),
                command: None,
            }],
            ..BaselinePb::default()
        },
    ] {
        assert_eq!(
            corrupt(&with(baseline)),
            "a baseline command's UUID is malformed"
        );
    }
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
