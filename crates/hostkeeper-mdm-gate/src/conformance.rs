//! The cases every [`DeviceStore`] passes, one list for every implementation: the files of
//! [`crate::FileDeviceStore`] and the coordinator's table `mdm_devices` alike. A store's own tests
//! run [`run`] over a [`Subject`] of theirs and expect no [`Failure`].
//!
//! The cases are what the gate relies on of a store: a new store holds nothing; a saved record
//! comes back after a restart exactly as saved, every field and every phase; a save replaces the
//! record of its enrollment id; a save that fails is an error and leaves the record as it was; a
//! record kept that is not whole, or is of a newer schema, or is of another enrollment id than
//! its key, stops the load; and a field a later release added is skipped.

use std::fmt;

use hostkeeper_enrollment::names::KeyHash;
use prost::Message as _;
use sha2::{Digest as _, Sha256};

use crate::ids::{CommandUuid, EnrollmentId};
use crate::pkg::PkgDigest;
use crate::record::{
    DeviceRecord, InstallRecord, NonceRecord, Phase, ProfileAnswer, ProfileCommand, Sent,
};
use crate::store::{DeviceStore, SCHEMA, StoreError, encode_record};

/// One store under test, as its own tests make it: a place of its own that no other subject
/// shares.
pub trait Subject {
    /// The store over what the subject keeps, opened again as a restart opens it.
    fn open(&self) -> Box<dyn DeviceStore>;
    /// Makes the next save fail, writing nothing.
    fn fail_next_save(&self);
    /// Keeps `bytes` as the record of the enrollment id `key`, as a store that wrote them
    /// would have kept them. `key` is 1 to 64 ASCII letters, digits and `-`.
    fn put_bytes(&self, key: &str, bytes: &[u8]);
}

/// A case of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clause {
    /// A new store holds no record.
    Empty,
    /// A saved record is loaded after a restart exactly as saved: every field, every phase, the
    /// longest enrollment id, a record with no optional field.
    RoundTrip,
    /// A save replaces the record of its enrollment id, and leaves the others.
    Replace,
    /// A save that fails is an error, and the record is the one before it; the next save lands.
    FailedSave,
    /// A record that is not whole stops the load, with the reason the file store gives.
    NotWhole,
    /// A record of a newer schema stops the load.
    NewerSchema,
    /// A record of another enrollment id than its key stops the load.
    OtherKey,
    /// A field the reader does not know is skipped.
    UnknownField,
}

/// A case a store did not pass, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The case.
    pub clause: Clause,
    /// What the store did.
    pub why: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.clause, self.why)
    }
}

type Check = fn(&dyn Fn() -> Box<dyn Subject>) -> Result<(), String>;

/// Every case, each on subjects of its own that `fresh` makes, and the cases the store failed.
pub fn run(fresh: &dyn Fn() -> Box<dyn Subject>) -> Vec<Failure> {
    let checks: [(Clause, Check); 8] = [
        (Clause::Empty, empty),
        (Clause::RoundTrip, round_trip),
        (Clause::Replace, replace),
        (Clause::FailedSave, failed_save),
        (Clause::NotWhole, not_whole),
        (Clause::NewerSchema, newer_schema),
        (Clause::OtherKey, other_key),
        (Clause::UnknownField, unknown_field),
    ];
    checks
        .into_iter()
        .filter_map(|(clause, check)| check(fresh).err().map(|why| Failure { clause, why }))
        .collect()
}

fn id(text: &str) -> Result<EnrollmentId, String> {
    EnrollmentId::parse(text).ok_or_else(|| format!("{text} of the list is not an enrollment id"))
}

fn uuid(byte: u8) -> CommandUuid {
    CommandUuid::from_random([byte; 16])
}

fn answered(byte: u8, at_unix: u64, acknowledged: bool) -> ProfileCommand {
    ProfileCommand {
        uuid: uuid(byte),
        sent_unix: at_unix - 1,
        answer: Some(ProfileAnswer {
            acknowledged,
            at_unix,
        }),
    }
}

/// The record of `key` with every optional field set, two commands sent, two refused and an
/// error, in `phase`.
fn full(key: &str, phase: Phase) -> Result<DeviceRecord, String> {
    let mut record = DeviceRecord::new(id(key)?, 10);
    record.udid = format!("udid-{key}");
    record.authenticate_serial = Some("C02AUTH".to_owned());
    record.serial = Some("C02SERIAL".to_owned());
    record.phase = phase;
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
            uuid: uuid(1),
            request_type: "InstallProfile".to_owned(),
            sent_unix: 16,
            queued: true,
        },
        Sent {
            uuid: uuid(2),
            request_type: "SecurityInfo".to_owned(),
            sent_unix: 17,
            queued: false,
        },
    ];
    record.refused = vec!["DeviceInformation".to_owned(), "SecurityInfo".to_owned()];
    record.last_error = Some("DeviceInformation: the device answered Error".to_owned());
    record.config_profile = Some(answered(3, 19, true));
    record.nonce = Some(NonceRecord {
        serial: "C02SERIAL".to_owned(),
        request_id: KeyHash::from_bytes(&[9; 32]).ok_or("a key hash of the list is not one")?,
        issued_unix: 20,
        enrollment: 6,
        pushes: 2,
        push: Some(answered(4, 22, false)),
        removal: Some(ProfileCommand::new(uuid(5), 23)),
    });
    record.install = Some(InstallRecord {
        sha256: PkgDigest::parse(&"c".repeat(64)).ok_or("a digest of the list is not one")?,
        sends: 4,
        last: answered(6, 25, true),
    });
    Ok(record)
}

/// The records of `store`, in the order of their enrollment ids.
fn loaded(store: &dyn DeviceStore) -> Result<Vec<DeviceRecord>, String> {
    let mut records = store.load().map_err(|e| format!("the load failed: {e}"))?;
    records.sort_by(|a, b| a.enrollment_id.as_str().cmp(b.enrollment_id.as_str()));
    Ok(records)
}

fn saved(store: &dyn DeviceStore, record: &DeviceRecord) -> Result<(), String> {
    store
        .save(record)
        .map_err(|e| format!("the save of {} failed: {e}", record.enrollment_id))
}

fn expect(holds: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    if holds { Ok(()) } else { Err(why()) }
}

fn empty(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    let records = loaded(&*fresh().open())?;
    expect(records.is_empty(), || {
        format!("a new store holds {} records", records.len())
    })
}

fn round_trip(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    let subject = fresh();
    let records = vec![
        full("a-1", Phase::Authenticated)?,
        full("b-2", Phase::Enrolled)?,
        full("c-3", Phase::CheckedOut)?,
        DeviceRecord::new(id("d-4")?, 3),
        full(&"e".repeat(64), Phase::Enrolled)?,
    ];
    let store = subject.open();
    for record in &records {
        saved(&*store, record)?;
    }
    drop(store);
    let back = loaded(&*subject.open())?;
    expect(back == records, || {
        format!("saved {records:?}, loaded after a restart {back:?}")
    })
}

fn replace(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    let subject = fresh();
    let store = subject.open();
    let other = full("b-2", Phase::Enrolled)?;
    saved(&*store, &full("a-1", Phase::Enrolled)?)?;
    saved(&*store, &other)?;
    let mut changed = full("a-1", Phase::CheckedOut)?;
    changed.sent.clear();
    changed.last_error = None;
    changed.nonce = None;
    saved(&*store, &changed)?;
    drop(store);
    let back = loaded(&*subject.open())?;
    let wanted = vec![changed, other];
    expect(back == wanted, || {
        format!("after a second save of a-1, loaded {back:?}, not {wanted:?}")
    })
}

fn failed_save(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    let subject = fresh();
    let store = subject.open();
    let before = full("a-1", Phase::Enrolled)?;
    saved(&*store, &before)?;
    let mut refused = before.clone();
    refused.phase = Phase::CheckedOut;
    refused.checked_out_unix = Some(99);
    subject.fail_next_save();
    expect(store.save(&refused).is_err(), || {
        "a save told to fail returned Ok".to_owned()
    })?;
    let back = loaded(&*subject.open())?;
    expect(back == vec![before.clone()], || {
        format!("after a failed save, loaded {back:?}, not the record before it")
    })?;
    let mut after = before;
    after.enrollment = 8;
    saved(&*store, &after)?;
    let back = loaded(&*subject.open())?;
    expect(back == vec![after], || {
        format!("the save after a failed one did not land: loaded {back:?}")
    })
}

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
}

/// The fields of a record a case changes, in the record's own message.
#[derive(Clone, PartialEq, prost::Message)]
struct RecordPb {
    #[prost(string, tag = "1")]
    enrollment_id: String,
    #[prost(uint32, tag = "5")]
    phase: u32,
    #[prost(message, repeated, tag = "15")]
    sent: Vec<SentPb>,
    #[prost(string, optional, tag = "99")]
    later: Option<String>,
}

fn record_pb(key: &str) -> RecordPb {
    RecordPb {
        enrollment_id: key.to_owned(),
        phase: 2,
        sent: Vec::new(),
        later: None,
    }
}

/// The bytes of a record whose message is `record`, of `schema`, with its digest.
fn kept(schema: u32, record: &RecordPb) -> Vec<u8> {
    let record = record.encode_to_vec();
    FilePb {
        schema,
        sha256: Sha256::digest(&record).to_vec(),
        record,
    }
    .encode_to_vec()
}

/// The load of a fresh subject that keeps `bytes` under `key`.
fn load_of(
    fresh: &dyn Fn() -> Box<dyn Subject>,
    key: &str,
    bytes: &[u8],
) -> Result<Vec<DeviceRecord>, StoreError> {
    let subject = fresh();
    subject.put_bytes(key, bytes);
    subject.open().load()
}

/// The reason of `outcome`, which must be [`StoreError::Corrupt`].
fn corrupt_reason(outcome: Result<Vec<DeviceRecord>, StoreError>) -> Result<String, String> {
    match outcome {
        Err(StoreError::Corrupt { reason, .. }) => Ok(reason),
        other => Err(format!("loaded {other:?}, not refused as not whole")),
    }
}

fn not_whole(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    let good = kept(SCHEMA, &record_pb("a-1"));
    let mut torn = good.clone();
    torn.truncate(good.len() - 1);
    let short = {
        let record = record_pb("a-1").encode_to_vec();
        let mut sha256 = Sha256::digest(&record).to_vec();
        sha256.pop();
        FilePb {
            schema: SCHEMA,
            record,
            sha256,
        }
        .encode_to_vec()
    };
    let mut digest = good.clone();
    if let Some(last) = digest.last_mut() {
        *last ^= 1;
    }
    let undecodable = {
        let record = vec![0xff, 0xff];
        FilePb {
            schema: SCHEMA,
            sha256: Sha256::digest(&record).to_vec(),
            record,
        }
        .encode_to_vec()
    };
    let unknown_phase = kept(
        SCHEMA,
        &RecordPb {
            phase: 4,
            ..record_pb("a-1")
        },
    );
    let bad_uuid = kept(
        SCHEMA,
        &RecordPb {
            sent: vec![SentPb {
                uuid: "not a uuid".to_owned(),
                request_type: "SecurityInfo".to_owned(),
            }],
            ..record_pb("a-1")
        },
    );
    let other_type = kept(
        SCHEMA,
        &RecordPb {
            sent: vec![SentPb {
                uuid: uuid(1).as_str().to_owned(),
                request_type: "EraseDevice".to_owned(),
            }],
            ..record_pb("a-1")
        },
    );
    let cases: [(&str, Vec<u8>, &str); 9] = [
        (
            "garbage",
            b"\xff\xff\xff".to_vec(),
            "the file is not a record",
        ),
        (
            "no schema",
            kept(0, &record_pb("a-1")),
            "the file has no schema",
        ),
        ("a cut file", torn, "the file is not a record"),
        (
            "a short digest",
            short,
            "the record does not match its digest",
        ),
        (
            "a changed digest",
            digest,
            "the record does not match its digest",
        ),
        (
            "an undecodable record",
            undecodable,
            "the record does not decode",
        ),
        (
            "an unknown phase",
            unknown_phase,
            "the record's phase is unknown",
        ),
        (
            "a malformed UUID",
            bad_uuid,
            "a sent command's UUID is malformed",
        ),
        (
            "a command the gate does not send",
            other_type,
            "a sent command's type is not one the gate sends",
        ),
    ];
    for (case, bytes, reason) in cases {
        let got =
            corrupt_reason(load_of(fresh, "a-1", &bytes)).map_err(|e| format!("{case}: {e}"))?;
        expect(got == reason, || {
            format!("{case}: refused with {got:?}, not {reason:?}")
        })?;
    }
    Ok(())
}

fn newer_schema(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    for schema in [SCHEMA + 1, u32::MAX] {
        match load_of(fresh, "a-1", &kept(schema, &record_pb("a-1"))) {
            Err(StoreError::NewerSchema { schema: got, .. }) if got == schema => {}
            other => return Err(format!("schema {schema}: loaded {other:?}")),
        }
    }
    Ok(())
}

fn other_key(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    let cases = [
        ("another id", encode_record(&full("b-2", Phase::Enrolled)?)),
        (
            "an id that is not one",
            kept(SCHEMA, &record_pb("not an id")),
        ),
        ("no id", kept(SCHEMA, &record_pb(""))),
    ];
    for (case, bytes) in cases {
        corrupt_reason(load_of(fresh, "a-1", &bytes)).map_err(|e| format!("{case}: {e}"))?;
    }
    Ok(())
}

fn unknown_field(fresh: &dyn Fn() -> Box<dyn Subject>) -> Result<(), String> {
    let later = kept(
        SCHEMA,
        &RecordPb {
            later: Some("a field of a later release".to_owned()),
            ..record_pb("a-1")
        },
    );
    let back = load_of(fresh, "a-1", &later).map_err(|e| format!("the load failed: {e}"))?;
    let without = load_of(fresh, "a-1", &kept(SCHEMA, &record_pb("a-1")))
        .map_err(|e| format!("the load of the record without the field failed: {e}"))?;
    expect(back.len() == 1 && back == without, || {
        format!("a record with a field of a later release loaded as {back:?}, not {without:?}")
    })
}
