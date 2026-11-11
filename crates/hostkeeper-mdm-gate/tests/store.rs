//! The device records on disk: the round trip of every field, the files the store refuses and
//! the ones it ignores, and what a failing file system does to the gate.

mod support;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::fs::{FaultFs, Fs, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_mdm_gate::{
    CommandUuid, DeviceRecord, DeviceStore, FileDeviceStore, Gate, Handled, InstallRecord,
    NonceRecord, Phase, PkgDigest, ProfileAnswer, ProfileCommand, SCHEMA, Sent, StoreError,
};
use prost::Message as _;
use sha2::{Digest as _, Sha256};
use support::{Clock, FakeNanoHub, OTHER_UDID, TestFs, UDID, counting, id, scratch, token_update};

// The shapes of the record file's messages, as `store.rs` writes them.
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
}

#[derive(Clone, PartialEq, prost::Message)]
struct RecordPb {
    #[prost(string, tag = "1")]
    enrollment_id: String,
    #[prost(string, tag = "2")]
    udid: String,
    #[prost(string, optional, tag = "3")]
    authenticate_serial: Option<String>,
    #[prost(string, optional, tag = "4")]
    serial: Option<String>,
    #[prost(uint32, tag = "5")]
    phase: u32,
    #[prost(uint32, tag = "6")]
    enrollment: u32,
    #[prost(bool, optional, tag = "7")]
    supervised: Option<bool>,
    #[prost(bool, optional, tag = "8")]
    enrolled_via_dep: Option<bool>,
    #[prost(bool, optional, tag = "9")]
    user_approved: Option<bool>,
    #[prost(uint64, optional, tag = "10")]
    enrolled_since_unix: Option<u64>,
    #[prost(uint64, tag = "11")]
    last_checkin_unix: u64,
    #[prost(uint64, optional, tag = "12")]
    checked_out_unix: Option<u64>,
    #[prost(uint64, optional, tag = "13")]
    device_information_unix: Option<u64>,
    #[prost(uint64, optional, tag = "14")]
    security_info_unix: Option<u64>,
    #[prost(message, repeated, tag = "15")]
    sent: Vec<SentPb>,
    #[prost(string, repeated, tag = "16")]
    refused: Vec<String>,
    #[prost(string, optional, tag = "17")]
    last_error: Option<String>,
}

const GOOD_UUID: &str = "0A1B2C3D-AAAA-4BBB-8CCC-0123456789AB";

fn good(id: &str) -> RecordPb {
    RecordPb {
        enrollment_id: id.to_owned(),
        udid: id.to_owned(),
        phase: 2,
        enrollment: 1,
        last_checkin_unix: 5,
        ..RecordPb::default()
    }
}

/// The bytes of a record file holding `record`, with the digest the store writes.
fn file_of(record: &[u8]) -> Vec<u8> {
    file_with(SCHEMA, record, Sha256::digest(record).to_vec())
}

fn file_with(schema: u32, record: &[u8], sha256: Vec<u8>) -> Vec<u8> {
    FilePb {
        schema,
        record: record.to_vec(),
        sha256,
    }
    .encode_to_vec()
}

fn store_in(dir: &Path) -> FileDeviceStore {
    FileDeviceStore::open(Arc::new(RealFs), dir).expect("the store opens")
}

fn corrupt(path: &Path, reason: &str) -> String {
    format!(
        "{}: {reason}; the gate does not guess what a device's record said, so fix or remove the file to start",
        path.display()
    )
}

/// Writes `bytes` as the file `name` of a new store directory and loads it.
fn load_file(
    test: &str,
    name: &str,
    bytes: &[u8],
) -> (PathBuf, Result<Vec<DeviceRecord>, StoreError>) {
    let scratch = scratch(test);
    let dir = scratch.path().join("devices");
    let store = store_in(&dir);
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write");
    let loaded = store.load();
    // Keep the scratch alive for the load only.
    drop(scratch);
    (path, loaded)
}

/// Catches: a record file that is not a whole record of this release read as a device, or
/// refused with the wrong reason, or without the file's path and the way out.
#[test]
fn a_record_file_that_is_not_whole_is_refused_with_its_reason() {
    let bad_phase = |phase| {
        RecordPb {
            phase,
            ..good(UDID)
        }
        .encode_to_vec()
    };
    let bad_sent = |uuid: &str, kind: &str| {
        let sent = SentPb {
            uuid: uuid.to_owned(),
            request_type: kind.to_owned(),
            sent_unix: 1,
        };
        RecordPb {
            sent: vec![sent],
            ..good(UDID)
        }
        .encode_to_vec()
    };
    let name = format!("{UDID}.pb");
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "garbage",
            vec![0xff, 0xff, 0xff],
            "the file is not a record",
        ),
        ("empty", Vec::new(), "the file has no schema"),
        (
            "no schema",
            file_with(
                0,
                &good(UDID).encode_to_vec(),
                Sha256::digest(good(UDID).encode_to_vec()).to_vec(),
            ),
            "the file has no schema",
        ),
        (
            "bad digest",
            file_with(SCHEMA, &good(UDID).encode_to_vec(), vec![0; 32]),
            "the record does not match its digest",
        ),
        (
            "short digest",
            file_with(SCHEMA, &good(UDID).encode_to_vec(), Vec::new()),
            "the record does not match its digest",
        ),
        (
            "undecodable record",
            file_of(&[0xff, 0xff]),
            "the record does not decode",
        ),
        (
            "other id",
            file_of(&good(OTHER_UDID).encode_to_vec()),
            "the record's enrollment id is not its file's name",
        ),
        (
            "malformed id",
            file_of(&good("not an id").encode_to_vec()),
            "the record's enrollment id is not its file's name",
        ),
        (
            "empty id",
            file_of(&good("").encode_to_vec()),
            "the record's enrollment id is not its file's name",
        ),
        (
            "phase 0",
            file_of(&bad_phase(0)),
            "the record's phase is unknown",
        ),
        (
            "phase 4",
            file_of(&bad_phase(4)),
            "the record's phase is unknown",
        ),
        (
            "short uuid",
            file_of(&bad_sent("0A1B", "SecurityInfo")),
            "a sent command's UUID is malformed",
        ),
        (
            "uuid with a non-hex byte",
            file_of(&bad_sent(&GOOD_UUID.replace('A', "Z"), "SecurityInfo")),
            "a sent command's UUID is malformed",
        ),
        (
            "type off the list",
            file_of(&bad_sent(GOOD_UUID, "EraseDevice")),
            "a sent command's type is not one the gate sends",
        ),
    ];
    for (what, bytes, reason) in cases {
        let (path, loaded) = load_file("store-bad", &name, &bytes);
        let error = loaded.expect_err(what);
        assert!(
            matches!(error, StoreError::Corrupt { .. }),
            "{what}: {error}"
        );
        assert_eq!(error.to_string(), corrupt(&path, reason), "{what}");
    }
}

/// Catches: a record of a newer release read (and rewritten without the fields this release does
/// not know), or the refusal naming the wrong numbers.
#[test]
fn a_newer_schema_is_refused() {
    let name = format!("{UDID}.pb");
    let bytes = file_with(
        SCHEMA + 1,
        &good(UDID).encode_to_vec(),
        Sha256::digest(good(UDID).encode_to_vec()).to_vec(),
    );
    let (path, loaded) = load_file("store-newer", &name, &bytes);
    let error = loaded.expect_err("newer");
    assert!(
        matches!(&error, StoreError::NewerSchema { schema: 2, .. }),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "{}: schema 2 is newer than this release reads (1)",
            path.display()
        )
    );
    let bytes = file_with(u32::MAX, &[], Vec::new());
    let (path, loaded) = load_file("store-newest", &name, &bytes);
    assert_eq!(
        loaded.expect_err("newer").to_string(),
        format!(
            "{}: schema 4294967295 is newer than this release reads (1)",
            path.display()
        )
    );
}

/// Catches: a field a later release adds (unknown to this one) making its record unreadable, and
/// a record with the least that is valid refused.
#[test]
fn an_unknown_field_is_skipped_and_a_minimal_record_is_read() {
    let name = format!("{UDID}.pb");
    // Field 99, a varint, after the record's own fields.
    let mut record = good(UDID).encode_to_vec();
    record.extend_from_slice(&[0x98, 0x06, 0x01]);
    let (_, loaded) = load_file("store-unknown", &name, &file_of(&record));
    let records = loaded.expect("read");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].enrollment_id.as_str(), UDID);
    assert_eq!(records[0].phase, Phase::Enrolled);
    assert!(records[0].sent.is_empty() && records[0].refused.is_empty());

    for (number, phase) in [
        (1, Phase::Authenticated),
        (2, Phase::Enrolled),
        (3, Phase::CheckedOut),
    ] {
        let bytes = file_of(
            &RecordPb {
                phase: number,
                ..good(UDID)
            }
            .encode_to_vec(),
        );
        let (_, loaded) = load_file("store-phase", &name, &bytes);
        assert_eq!(loaded.expect("read")[0].phase, phase);
    }
}

/// Catches: a temporary file a crash left, or a file that is not a record, read as a device (or
/// refused); two devices' records mixed; the directory's own order mattering.
#[test]
fn only_files_named_dot_pb_are_records() {
    let scratch = scratch("store-names");
    let dir = scratch.path().join("devices");
    let store = store_in(&dir);
    let name = format!("{UDID}.pb");
    std::fs::write(dir.join(&name), file_of(&good(UDID).encode_to_vec())).expect("write");
    std::fs::write(
        dir.join(format!("{OTHER_UDID}.pb")),
        file_of(&good(OTHER_UDID).encode_to_vec()),
    )
    .expect("write");
    std::fs::write(dir.join(format!("{name}.tmp")), b"torn").expect("write");
    std::fs::write(dir.join("notes.txt"), b"hello").expect("write");
    std::fs::write(dir.join("pb"), b"hello").expect("write");
    std::fs::create_dir(dir.join("sub")).expect("a directory");
    let mut ids: Vec<String> = store
        .load()
        .expect("loads")
        .iter()
        .map(|record| record.enrollment_id.to_string())
        .collect();
    ids.sort();
    let mut wanted = [UDID.to_owned(), OTHER_UDID.to_owned()];
    wanted.sort();
    assert_eq!(ids, wanted);
}

/// Catches: a missing directory not made (the coordinator would not start on a new machine), made
/// with a mode others can enter, or an existing one disturbed.
#[test]
fn a_missing_directory_is_made_private_and_an_existing_one_is_kept() {
    let scratch = scratch("store-mkdir");
    let dir = scratch.path().join("a").join("b").join("devices");
    let store = store_in(&dir);
    assert!(store.load().expect("empty").is_empty());
    let mode = std::fs::metadata(&dir).expect("made").permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    let record = DeviceRecord::new(id(UDID), 1);
    store.save(&record).expect("saved");
    let again = store_in(&dir);
    assert_eq!(again.load().expect("loads"), [record]);
    assert_eq!(
        format!("{again:?}"),
        format!("FileDeviceStore {{ dir: {:?}, .. }}", dir)
    );
}

/// Catches: a directory under a regular file reported as a success, or the error without the
/// directory's path.
#[test]
fn a_directory_that_cannot_be_made_is_an_io_error() {
    let scratch = scratch("store-blocked");
    let file = scratch.path().join("file");
    std::fs::write(&file, b"x").expect("write");
    let dir = file.join("devices");
    let error = FileDeviceStore::open(Arc::new(RealFs), &dir).expect_err("blocked");
    assert!(
        matches!(&error, StoreError::Io { path, .. } if *path == dir),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .starts_with(&format!("{}: ", dir.display())),
        "{error}"
    );
}

/// A record with every optional field set, a sent and a refused entry, and an error.
fn full(id_text: &str) -> DeviceRecord {
    let mut record = DeviceRecord::new(id(id_text), 10);
    record.udid = id_text.to_owned();
    record.authenticate_serial = Some("C02AUTH".to_owned());
    record.serial = Some("C02SERIAL".to_owned());
    record.phase = Phase::CheckedOut;
    record.enrollment = 7;
    record.supervised = Some(false);
    record.enrolled_via_dep = Some(true);
    record.user_approved = Some(false);
    record.enrolled_since_unix = Some(11);
    record.last_checkin_unix = 12;
    record.checked_out_unix = Some(13);
    record.device_information_unix = Some(14);
    record.security_info_unix = Some(15);
    record.sent = vec![
        Sent {
            uuid: CommandUuid::from_random([1; 16]),
            request_type: "InstallProfile".to_owned(),
            sent_unix: 16,
            queued: true,
        },
        Sent {
            uuid: CommandUuid::from_random([2; 16]),
            request_type: "SecurityInfo".to_owned(),
            sent_unix: 17,
            queued: true,
        },
    ];
    record.refused = vec!["DeviceInformation".to_owned(), "SecurityInfo".to_owned()];
    record.last_error = Some("DeviceInformation: the device answered Error".to_owned());
    record.config_profile = Some(ProfileCommand {
        uuid: CommandUuid::from_random([3; 16]),
        sent_unix: 18,
        answer: Some(ProfileAnswer {
            acknowledged: true,
            at_unix: 19,
        }),
        settings_sha256: Some("d".repeat(64)),
    });
    record.nonce = Some(NonceRecord {
        serial: "C02SERIAL".to_owned(),
        request_id: KeyHash::from_bytes(&[9; 32]).expect("32 bytes"),
        issued_unix: 20,
        enrollment: 6,
        pushes: 2,
        push: Some(ProfileCommand {
            uuid: CommandUuid::from_random([4; 16]),
            sent_unix: 21,
            answer: Some(ProfileAnswer {
                acknowledged: false,
                at_unix: 22,
            }),
            settings_sha256: None,
        }),
        removal: Some(ProfileCommand::new(CommandUuid::from_random([5; 16]), 23)),
    });
    record.install = Some(InstallRecord {
        sha256: PkgDigest::parse(&"c".repeat(64)).expect("a digest"),
        sends: 4,
        last: ProfileCommand {
            uuid: CommandUuid::from_random([6; 16]),
            sent_unix: 24,
            answer: Some(ProfileAnswer {
                acknowledged: true,
                at_unix: 25,
            }),
            settings_sha256: None,
        },
    });
    record
}

/// The enrollment side's fields (18 and 19) of a record, in the shapes `store.rs` writes, to
/// append to a record of [`RecordPb`]: protobuf merges a message's fields across its bytes.
#[derive(Clone, PartialEq, prost::Message)]
struct ProfilePb {
    #[prost(string, tag = "1")]
    uuid: String,
    #[prost(uint64, tag = "2")]
    sent_unix: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
struct NoncePb {
    #[prost(string, tag = "1")]
    serial: String,
    #[prost(bytes = "vec", tag = "2")]
    request_id: Vec<u8>,
    #[prost(message, optional, tag = "6")]
    push: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct EnrollFieldsPb {
    #[prost(message, optional, tag = "18")]
    config_profile: Option<ProfilePb>,
    #[prost(message, optional, tag = "19")]
    nonce: Option<NoncePb>,
}

/// Catches: the enrollment side's fields made a change a release before them cannot read (a
/// reader of fields 1 to 17 reads a record that has them, and every field it knows is the
/// same), or a record without them (written before them) not read as one with no profile sent
/// and no nonce; a malformed UUID or request id in them read as a device, not refused.
#[test]
fn the_enrollment_fields_are_read_by_the_release_before_them_and_the_other_way() {
    let scratch = scratch("store-enroll-fields");
    let dir = scratch.path().join("devices");
    let record = full(UDID);
    store_in(&dir).save(&record).expect("saved");
    let bytes = std::fs::read(dir.join(format!("{UDID}.pb"))).expect("the file");
    let file = FilePb::decode(bytes.as_slice()).expect("a file");
    let before = RecordPb::decode(file.record.as_slice()).expect("the release before reads it");
    assert_eq!(before.enrollment_id, UDID);
    assert_eq!(before.serial.as_deref(), Some("C02SERIAL"));
    assert_eq!((before.phase, before.enrollment), (3, 7));
    assert_eq!(before.sent.len(), 2);
    assert_eq!(before.last_error, record.last_error);

    let name = format!("{UDID}.pb");
    let (_, loaded) = load_file(
        "store-before-fields",
        &name,
        &file_of(&good(UDID).encode_to_vec()),
    );
    let loaded = loaded.expect("read");
    assert_eq!(
        (&loaded[0].config_profile, &loaded[0].nonce),
        (&None, &None)
    );

    let with = |extra: &EnrollFieldsPb| {
        let mut bytes = good(UDID).encode_to_vec();
        bytes.extend(extra.encode_to_vec());
        file_of(&bytes)
    };
    let profile = |uuid: &str| ProfilePb {
        uuid: uuid.to_owned(),
        sent_unix: 1,
    };
    let cases = [
        (
            EnrollFieldsPb {
                config_profile: Some(profile("not-a-uuid")),
                nonce: None,
            },
            "the config profile's UUID is malformed",
        ),
        (
            EnrollFieldsPb {
                config_profile: None,
                nonce: Some(NoncePb {
                    serial: "C02SERIAL".to_owned(),
                    request_id: vec![9; 31],
                    push: None,
                }),
            },
            "the nonce's request id or a UUID is malformed",
        ),
        (
            EnrollFieldsPb {
                config_profile: None,
                nonce: Some(NoncePb {
                    serial: "C02SERIAL".to_owned(),
                    request_id: vec![9; 32],
                    push: Some(profile("not-a-uuid")),
                }),
            },
            "the nonce's request id or a UUID is malformed",
        ),
    ];
    for (extra, reason) in cases {
        let (path, loaded) = load_file("store-enroll-corrupt", &name, &with(&extra));
        assert_eq!(
            loaded.expect_err("refused").to_string(),
            corrupt(&path, reason)
        );
    }
    let fine = EnrollFieldsPb {
        config_profile: Some(profile(GOOD_UUID)),
        nonce: Some(NoncePb {
            serial: "C02SERIAL".to_owned(),
            request_id: vec![9; 32],
            push: None,
        }),
    };
    let (_, loaded) = load_file("store-enroll-fine", &name, &with(&fine));
    let loaded = loaded.expect("read").remove(0);
    let nonce = loaded.nonce.expect("a nonce");
    assert_eq!((nonce.push, nonce.removal, nonce.pushes), (None, None, 0));
    let config = loaded.config_profile.expect("a config profile");
    assert_eq!((config.uuid.as_str(), config.answer), (GOOD_UUID, None));
}

/// The pkg install's field (20) of a record, as `store.rs` writes it.
#[derive(Clone, PartialEq, prost::Message)]
struct InstallPb {
    #[prost(string, tag = "1")]
    sha256: String,
    #[prost(uint32, tag = "2")]
    sends: u32,
    #[prost(message, optional, tag = "3")]
    last: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct InstallFieldPb {
    #[prost(message, optional, tag = "20")]
    install: Option<InstallPb>,
}

/// Catches: the pkg install's field made a change the release before it cannot read (a reader
/// of fields 1 to 19 reads a record that has it, and every field it knows is the same), or a
/// record without it (written before it) not read as one with no pkg sent; a malformed digest,
/// UUID or a missing command in it read as a device, not refused.
#[test]
fn the_install_field_is_read_by_the_release_before_it_and_the_other_way() {
    let scratch = scratch("store-install-field");
    let dir = scratch.path().join("devices");
    let record = full(UDID);
    store_in(&dir).save(&record).expect("saved");
    let bytes = std::fs::read(dir.join(format!("{UDID}.pb"))).expect("the file");
    let file = FilePb::decode(bytes.as_slice()).expect("a file");
    let before =
        EnrollFieldsPb::decode(file.record.as_slice()).expect("the release before reads it");
    assert_eq!(
        before.config_profile.map(|profile| profile.sent_unix),
        Some(18)
    );
    let before = RecordPb::decode(file.record.as_slice()).expect("the release before reads it");
    assert_eq!((before.phase, before.enrollment), (3, 7));

    let name = format!("{UDID}.pb");
    let (_, loaded) = load_file(
        "store-before-install",
        &name,
        &file_of(&good(UDID).encode_to_vec()),
    );
    assert_eq!(loaded.expect("read")[0].install, None);

    let with = |install: InstallPb| {
        let mut bytes = good(UDID).encode_to_vec();
        bytes.extend(
            InstallFieldPb {
                install: Some(install),
            }
            .encode_to_vec(),
        );
        file_of(&bytes)
    };
    let last = |uuid: &str| {
        Some(ProfilePb {
            uuid: uuid.to_owned(),
            sent_unix: 30,
        })
    };
    let reason = "the pkg install's digest or UUID is malformed";
    let cases = [
        InstallPb {
            sha256: "C".repeat(64),
            sends: 1,
            last: last(GOOD_UUID),
        },
        InstallPb {
            sha256: "c".repeat(64),
            sends: 1,
            last: last("not-a-uuid"),
        },
        InstallPb {
            sha256: "c".repeat(64),
            sends: 1,
            last: None,
        },
    ];
    for install in cases {
        let (path, loaded) = load_file("store-install-corrupt", &name, &with(install));
        assert_eq!(
            loaded.expect_err("refused").to_string(),
            corrupt(&path, reason)
        );
    }
    let (_, loaded) = load_file(
        "store-install-fine",
        &name,
        &with(InstallPb {
            sha256: "c".repeat(64),
            sends: 2,
            last: last(GOOD_UUID),
        }),
    );
    let install = loaded.expect("read").remove(0).install.expect("an install");
    assert_eq!(
        (
            install.sha256.as_str(),
            install.sends,
            install.last.uuid.as_str()
        ),
        ("c".repeat(64).as_str(), 2, GOOD_UUID)
    );
    assert_eq!((install.last.sent_unix, install.last.answer), (30, None));
}

/// Catches: the config profile's settings digest (field 5 of its message) made a change the
/// release before it cannot read (a reader of the profile's first two fields reads a record that
/// has it, with the same command), a record written without it not read as one with no digest
/// (which counts as different settings), or the digest lost or changed in the file.
#[test]
fn the_settings_digest_is_read_by_the_release_before_it_and_the_other_way() {
    let scratch = scratch("store-settings-digest");
    let dir = scratch.path().join("devices");
    let record = full(UDID);
    store_in(&dir).save(&record).expect("saved");
    let bytes = std::fs::read(dir.join(format!("{UDID}.pb"))).expect("the file");
    let file = FilePb::decode(bytes.as_slice()).expect("a file");
    let before = EnrollFieldsPb::decode(file.record.as_slice()).expect("the release before");
    let command = before.config_profile.expect("a config profile");
    assert_eq!(
        (command.uuid.as_str(), command.sent_unix),
        (CommandUuid::from_random([3; 16]).as_str(), 18)
    );
    let loaded = store_in(&dir).load().expect("loads").remove(0);
    let digest = loaded.config_profile.and_then(|c| c.settings_sha256);
    assert_eq!(digest, Some("d".repeat(64)));

    // A record the release before wrote: a config profile of two fields.
    let mut old = good(UDID).encode_to_vec();
    old.extend(
        EnrollFieldsPb {
            config_profile: Some(ProfilePb {
                uuid: GOOD_UUID.to_owned(),
                sent_unix: 40,
            }),
            nonce: None,
        }
        .encode_to_vec(),
    );
    let (_, loaded) = load_file("store-no-digest", &format!("{UDID}.pb"), &file_of(&old));
    let command = loaded
        .expect("read")
        .remove(0)
        .config_profile
        .expect("sent");
    assert_eq!((command.sent_unix, command.settings_sha256), (40, None));
}

/// Catches: a field lost in the file (any of the twenty, and those of the profile commands), an unset optional read back as set
/// or the other way round, a phase changed; a rewrite that leaves two files or a mode wider
/// than 0600.
#[test]
fn a_record_round_trips_every_field() {
    let scratch = scratch("store-round-trip");
    let dir = scratch.path().join("devices");
    let store = store_in(&dir);
    let empty = DeviceRecord::new(id(OTHER_UDID), 3);
    let mut one_phase = full(&"b".repeat(64));
    one_phase.phase = Phase::Authenticated;
    let mut other_phase = full("c-1");
    other_phase.phase = Phase::Enrolled;
    let records = [full(UDID), empty, one_phase, other_phase];
    for record in &records {
        store.save(record).expect("saved");
    }
    let mut loaded = store_in(&dir).load().expect("loads");
    loaded.sort_by(|a, b| a.enrollment_id.cmp(&b.enrollment_id));
    let mut wanted = records.to_vec();
    wanted.sort_by(|a, b| a.enrollment_id.cmp(&b.enrollment_id));
    assert_eq!(loaded, wanted);

    // Saving replaces the record.
    let mut changed = full(UDID);
    changed.sent.clear();
    changed.last_error = None;
    store.save(&changed).expect("saved");
    let mut names = RealFs.list(&dir).expect("listed");
    names.sort();
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(names.iter().all(|name| name.ends_with(".pb")));
    let after = store_in(&dir).load().expect("loads");
    assert!(after.contains(&changed));
    let mode = std::fs::metadata(dir.join(format!("{UDID}.pb")))
        .expect("file")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// Catches: a listing or a read that fails reported as an empty store (every device forgotten),
/// or with the wrong path.
#[test]
fn a_list_or_a_read_that_fails_is_an_io_error() {
    let scratch = scratch("store-io");
    let dir = scratch.path().join("devices");
    let fs = Arc::new(FaultFs::new());
    let store = FileDeviceStore::open(fs.clone(), &dir).expect("opens");
    store.save(&DeviceRecord::new(id(UDID), 1)).expect("saved");
    let before = fs.count();

    fs.fail_at(before + 1);
    let error = store.load().expect_err("list fails");
    assert!(
        matches!(&error, StoreError::Io { path, .. } if *path == dir),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "{}: injected failure at operation {}",
            dir.display(),
            before + 1
        )
    );

    fs.fail_at(before + 3);
    let error = store.load().expect_err("read fails");
    let file = dir.join(format!("{UDID}.pb"));
    assert!(
        matches!(&error, StoreError::Io { path, .. } if *path == file),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "{}: injected failure at operation {}",
            file.display(),
            before + 3
        )
    );
}

/// Catches: a record file listed and removed a moment later (by hand) failing the start, or
/// kept as a device.
#[test]
fn a_file_listed_and_then_gone_is_skipped() {
    let scratch = scratch("store-gone");
    let dir = scratch.path().join("devices");
    let fs = Arc::new(TestFs::default());
    let store = FileDeviceStore::open(fs.clone(), &dir).expect("opens");
    store.save(&DeviceRecord::new(id(UDID), 1)).expect("saved");
    store
        .save(&DeviceRecord::new(id(OTHER_UDID), 1))
        .expect("saved");
    fs.hide(&format!("{UDID}.pb"));
    let loaded = store.load().expect("loads");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].enrollment_id.as_str(), OTHER_UDID);
}

/// Catches: a record that cannot be written taken into memory anyway (the gate would say a
/// device is enrolled that no file says), or sending commands for it; the error without the
/// file's path.
#[tokio::test]
async fn a_record_that_cannot_be_written_is_not_taken() {
    // Each operation of a write fails in turn.
    for step in 1..=4u64 {
        let scratch = scratch("store-save-fails");
        let dir = scratch.path().join("devices");
        let hub = FakeNanoHub::start().await;
        let fs = Arc::new(FaultFs::new());
        let clock = Clock::at(1_000);
        let store = Box::new(FileDeviceStore::open(fs.clone(), &dir).expect("opens"));
        let gate = Gate::open(store, hub.client(), Arc::new(clock), counting()).expect("opens");
        support::handle(&gate, &support::authenticate(OTHER_UDID, None)).await;
        let before = gate.records();
        let heard = hub.heard().len();

        let n = fs.count() + step;
        fs.fail_at(n);
        let error = gate
            .handle(support::event_of(&token_update(UDID, 1)))
            .await
            .expect_err("not taken");
        assert!(matches!(error, StoreError::Io { .. }), "{error}");
        assert_eq!(
            error.to_string(),
            format!(
                "{}: injected failure at operation {n}",
                dir.join(format!("{UDID}.pb")).display()
            )
        );
        assert_eq!(gate.records(), before, "step {step}");
        assert_eq!(
            hub.heard().len(),
            heard,
            "no command is sent for a record not kept"
        );

        // The next event is taken.
        assert_eq!(
            support::handle(&gate, &token_update(UDID, 1)).await,
            Handled::Recorded
        );
        assert_eq!(gate.records().len(), 2);
    }
}

/// Catches: a gate that opens over a store whose records cannot be loaded, or the key shown in
/// the gate's `Debug`.
#[tokio::test]
async fn a_gate_does_not_open_over_a_record_it_cannot_read_and_shows_no_key() {
    let scratch = scratch("store-gate-open");
    let dir = scratch.path().join("devices");
    let hub = FakeNanoHub::start().await;
    let clock = Clock::at(1);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join(format!("{UDID}.pb")), b"\xff\xff").expect("write");
    let error = support::open(&dir, &hub, &clock).expect_err("refused");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error}");
    std::fs::remove_file(dir.join(format!("{UDID}.pb"))).expect("remove");
    let gate = support::open(&dir, &hub, &clock).expect("opens");
    let shown = format!("{gate:?}");
    assert!(
        shown.starts_with("Gate { queue: NanoHub { url: ApiUrl {"),
        "{shown}"
    );
    assert!(
        shown.contains("ApiKey(..)") && !shown.contains(support::KEY),
        "{shown}"
    );
}
