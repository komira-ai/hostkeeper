//! The contract of [`Records`], as a suite every implementation's tests run: [`run`] holds a
//! [`Subject`] to each [`Clause`] and returns the clauses it breaks, with why. An
//! implementation conforms when the list is empty.
//!
//! The suite writes records of every kind directly, not through the store: the store's own
//! rules are the store's tests'. It asks no more of a failed commit than that one which fails
//! before it writes anything leaves the records as they were, which both a commit that writes
//! all or nothing and the files' commit (one record after the other) keep.

use std::fmt;
use std::sync::Arc;

use super::{Record, Records, pb};
use crate::enroll::mac_nonce::MacNonceHash;
use crate::enroll::{FIRST_SERIAL, StoreError};
use crate::names::{HostId, KeyHash};

/// What the suite is run against: one set of durable records, empty when the subject is made.
pub trait Subject {
    /// The records, opened: every call opens the same durable records again, as a restart does.
    fn open(&self) -> Arc<dyn Records>;
    /// Makes the next commit of the records opened last fail before it writes anything.
    fn fail_next_commit(&self);
}

/// One promise of the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Clause {
    /// Empty records load nothing and list nothing.
    Empty,
    /// A record of every kind, committed together, loads back field for field after the records
    /// are opened again.
    RoundTrip,
    /// A commit of a record whose key is kept already replaces it.
    Overwrite,
    /// `requests` lists every request with its id, in the order of the ids, whatever its state,
    /// one this release does not know included.
    RequestsInOrder,
    /// `host_ids` lists every host, sorted, and no record of another kind.
    HostIdsSorted,
    /// Serials start at [`FIRST_SERIAL`], go up by one, survive a reopen, and a reserving take
    /// writes the request with the serial taken and nothing else changed.
    Serial,
    /// The first heartbeat, the first serving heartbeat and the duplicate mark change that
    /// field of the host only.
    HostMarks,
    /// A commit that fails before it writes leaves the records as they were.
    FailedCommit,
    /// A commit holding a record whose key does not parse is refused and writes nothing; a
    /// serial reserved in such a request is refused and not taken.
    Unkeyed,
    /// The lock can be taken, and taken again once released.
    Lock,
}

/// A clause broken, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub clause: Clause,
    pub why: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.clause, self.why)
    }
}

/// The check of one clause: `Err(why)` when the subject breaks it.
type Check = fn(&dyn Subject) -> Result<(), String>;

/// Runs every clause, each against a subject of its own from `fresh`, and returns the ones
/// broken, in the order of [`Clause`].
pub fn run(fresh: &dyn Fn() -> Box<dyn Subject>) -> Vec<Failure> {
    let clauses: [(Clause, Check); 10] = [
        (Clause::Empty, empty),
        (Clause::RoundTrip, round_trip),
        (Clause::Overwrite, overwrite),
        (Clause::RequestsInOrder, requests_in_order),
        (Clause::HostIdsSorted, host_ids_sorted),
        (Clause::Serial, serial),
        (Clause::HostMarks, host_marks),
        (Clause::FailedCommit, failed_commit),
        (Clause::Unkeyed, unkeyed),
        (Clause::Lock, lock),
    ];
    clauses
        .into_iter()
        .filter_map(|(clause, check)| check(&*fresh()).err().map(|why| Failure { clause, why }))
        .collect()
}

/// `Err(why)` unless `holds`.
fn expect(holds: bool, why: String) -> Result<(), String> {
    if holds { Ok(()) } else { Err(why) }
}

/// A store error as a clause's failure.
fn failed(what: &str) -> impl FnOnce(StoreError) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// The DER of a P-256 SubjectPublicKeyInfo before its point.
const SPKI_PREFIX: &str = "3059301306072a8648ce3d020106082a8648ce3d030107034200";

/// Nine P-256 public keys (uncompressed points), so the suite's requests carry real keys whose
/// SHA-256 is their id, as the store checks every request it reads.
const POINTS: [&str; 9] = [
    "04234d2f154c381dcf7e5b013de103fb3d2a9dda67248a278cdfdb89ddd462c04e22c75de3660f763fa0436e300938649156c9fbe3d621efa362231424898413eb",
    "04fa93f7b81ecee483785d6e6cc8307f081c347ba12da883d950c360f124cf62097deaae582443038cd64428f74b5faf696a2573d0e000b88703d4ca9dc60eee33",
    "049822d192787a454ace630e22928c9904301a5103d36f579d01a84b8199bdd51a89fcf85b6e5267bc8a0a6af0c28bdc0a141f2e6e829421435bb7a941b783353c",
    "04a8e8c8bf6398ab0b6af769b56c51dfc561559abb0f645a9887619dc9dab14d8578f26dedd8026763e3a45e303a763c02e1426fd3c4519d37202987c745cc44d5",
    "049e44a43fdae505f33c159df5fd34c66e1819a6c6cfbde580543b06d5fd0c248e9abb8fd1f89bc3edb163660737b64f3c1091c49dd17c1259137f6e48b8ca3d97",
    "04a3e46bbf65af534d4b680c782b41863eefb7d4e14786e131a8defcfdf1d4aeb62f202a642fe3cf85cc4c3939eef6f2e5632806594ed6e4d4dbe4b95167e8d91a",
    "04e2a1a499c4e81c53e4d18038427e920d04721cbb484e92cc9536f170d975d49a5edc61fc5092991cded5c6aa2b0b848d09825cd83882d97b4aa97d2aa302baa6",
    "04f64d3685d163cb89e2c1493fb6402434ea7656afa311cd9aa5d0391c9d0eb51bb723f480d3ea553b45030415e5ff986b21047d7fd466e936d934238e5bfe4e64",
    "04ac78ceaea6f08d3dcb17c704307ebf21c532cc61fa12eb1eadd81e427c20e6e7dd62d7a7d670b5e6df2756fa1dcdca87389da50a383b52ca68f88167a044d76f",
];

/// The SubjectPublicKeyInfo of the suite's key `n`, 1 to 9.
fn spki(n: u8) -> Vec<u8> {
    let point = POINTS[usize::from(n.clamp(1, 9) - 1)];
    hex::decode(format!("{SPKI_PREFIX}{point}")).unwrap_or_default()
}

fn key(n: u8) -> KeyHash {
    KeyHash::of_spki(&spki(n))
}

fn host_id(text: &str) -> Result<HostId, String> {
    HostId::parse(text).map_err(|e| format!("a host id of the suite: {e}"))
}

/// The request of the suite's key `n` in the stored state `state`, with every field set.
fn request(n: u8, state: pb::RequestState) -> pb::Request {
    let id = key(n);
    pb::Request {
        schema: 1,
        request_id: id.as_bytes().to_vec(),
        csr_der: vec![0x30, 1, 2],
        claims: Some(pb::Claims {
            machine_id: format!("m-{id}"),
            dmi_serial: "dmi".to_owned(),
            dmi_product_uuid: "uuid".to_owned(),
            addresses: vec!["aa:bb:cc:dd:ee:01".to_owned(), "192.0.2.1".to_owned()],
            mac_serial: "C02".to_owned(),
            mac_platform_uuid: "platform".to_owned(),
            bmc_mac: "aa:bb:cc:dd:ee:02".to_owned(),
        }),
        image: "image-1".to_owned(),
        posted_unix: 1_800_000_000,
        renewal_of: "web-0".to_owned(),
        state: state as i32,
        approved: Some(pb::Approval {
            host_id: "web-1".to_owned(),
            role: "web".to_owned(),
            services: vec!["api".to_owned(), "db".to_owned()],
            replace: true,
            approver: "ops".to_owned(),
            approved_unix: 1_800_000_100,
            reason: "racked".to_owned(),
            mac_nonce_sha256: vec![7; 32],
        }),
        spki_der: spki(n),
        round: 2,
        serial: 0,
        mac_nonce_presented: true,
        expired_unix: 1_800_000_200,
        expired_after_secs: 86_400,
    }
}

fn host(name: &str, key: &KeyHash) -> pb::Host {
    pb::Host {
        schema: 1,
        host_id: name.to_owned(),
        key_hash: key.as_bytes().to_vec(),
        machine_id: format!("m-{name}"),
        role: "web".to_owned(),
        services: vec!["api".to_owned()],
        duplicate: false,
        first_heartbeat_unix: None,
        first_serving_unix: None,
        running: None,
    }
}

/// What a daemon said it runs, with one service of `version`.
fn running(version: &str, at_unix: u64) -> pb::Running {
    pb::Running {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "0.2.0".to_owned(),
        services: vec![pb::RunningService {
            service: "api".to_owned(),
            version: version.to_owned(),
            installer: "deb".to_owned(),
        }],
        at_unix,
    }
}

/// One record of every kind.
fn every_kind() -> Vec<Record> {
    let id = key(1);
    vec![
        Record::Deny(pb::Deny {
            schema: 1,
            host_id: "web-1".to_owned(),
            state: pb::DenyState::Cleared as i32,
            cleared_by: id.as_bytes().to_vec(),
        }),
        Record::Host(pb::Host {
            duplicate: true,
            first_heartbeat_unix: Some(1_800_000_300),
            first_serving_unix: Some(1_800_000_360),
            running: Some(running("1.0", 1_800_000_380)),
            ..host("web-1", &id)
        }),
        Record::Request(pb::Request {
            serial: 9,
            ..request(1, pb::RequestState::Issued)
        }),
        Record::Issued(pb::Issued {
            schema: 1,
            request_id: id.as_bytes().to_vec(),
            round: 2,
            serial: 9,
            certificate_der: vec![0x30, 4, 5],
            host_id: "web-1".to_owned(),
            not_after: 1_800_604_800,
        }),
        Record::MacNonce(pb::MacNonce {
            schema: 1,
            nonce_sha256: vec![7; 32],
            serial: "C02".to_owned(),
            issued_unix: 1_800_000_050,
            consumed_by: id.as_bytes().to_vec(),
            request_id: id.as_bytes().to_vec(),
        }),
    ]
}

/// What `records` holds for the key of `record`, as a record of the same kind.
fn load_like(records: &dyn Records, record: &Record) -> Result<Option<Record>, String> {
    let read = failed("a load");
    Ok(match record {
        Record::Request(m) => records
            .request(&key_of(&m.request_id)?)
            .map_err(read)?
            .map(Record::Request),
        Record::Issued(m) => records
            .issued(&key_of(&m.request_id)?)
            .map_err(read)?
            .map(Record::Issued),
        Record::Host(m) => records
            .host(&host_id(&m.host_id)?)
            .map_err(read)?
            .map(Record::Host),
        Record::Deny(m) => records
            .deny(&host_id(&m.host_id)?)
            .map_err(read)?
            .map(Record::Deny),
        Record::MacNonce(m) => records
            .mac_nonce(&hash_of(&m.nonce_sha256)?)
            .map_err(read)?
            .map(Record::MacNonce),
    })
}

fn hash_of(bytes: &[u8]) -> Result<MacNonceHash, String> {
    MacNonceHash::from_bytes(bytes).ok_or_else(|| "a nonce of the suite is not 32 bytes".to_owned())
}

fn key_of(bytes: &[u8]) -> Result<KeyHash, String> {
    KeyHash::from_bytes(bytes).ok_or_else(|| "a key of the suite is not 32 bytes".to_owned())
}

/// Every record of `wanted` loads back from `records` as it is.
fn holds_all(records: &dyn Records, wanted: &[Record]) -> Result<(), String> {
    for record in wanted {
        let loaded = load_like(records, record)?;
        expect(
            loaded.as_ref() == Some(record),
            format!("committed {record:?}, loaded {loaded:?}"),
        )?;
    }
    Ok(())
}

/// No record of `absent` loads from `records`.
fn holds_none(records: &dyn Records, absent: &[Record]) -> Result<(), String> {
    for record in absent {
        let loaded = load_like(records, record)?;
        expect(
            loaded.is_none(),
            format!("never committed, yet loaded {loaded:?}"),
        )?;
    }
    Ok(())
}

fn empty(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    holds_none(&*records, &every_kind())?;
    let all = records.requests().map_err(failed("a list"))?;
    let hosts = records.host_ids().map_err(failed("a list"))?;
    expect(all.is_empty(), format!("empty records list {all:?}"))?;
    expect(hosts.is_empty(), format!("empty records list {hosts:?}"))
}

fn round_trip(subject: &dyn Subject) -> Result<(), String> {
    let wanted = every_kind();
    subject
        .open()
        .commit(&wanted)
        .map_err(failed("the commit"))?;
    holds_all(&*subject.open(), &wanted)
}

fn overwrite(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    let first = every_kind();
    records.commit(&first).map_err(failed("the commit"))?;
    let second = vec![
        Record::Request(request(1, pb::RequestState::Denied)),
        Record::Host(host("web-1", &key(2))),
        Record::Deny(pb::Deny {
            schema: 1,
            host_id: "web-1".to_owned(),
            state: pb::DenyState::Retired as i32,
            cleared_by: Vec::new(),
        }),
    ];
    records
        .commit(&second)
        .map_err(failed("the second commit"))?;
    holds_all(&*subject.open(), &second)
}

fn requests_in_order(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    let states = [
        (9, pb::RequestState::Approved as i32),
        (3, pb::RequestState::Pending as i32),
        (7, pb::RequestState::Issued as i32),
        (1, pb::RequestState::DuplicateClaim as i32),
        (5, pb::RequestState::Denied as i32),
        (4, 9),
    ];
    let mut all: Vec<(KeyHash, pb::Request)> = states
        .iter()
        .map(|(n, state)| {
            let request = pb::Request {
                state: *state,
                ..request(*n, pb::RequestState::Pending)
            };
            (key(*n), request)
        })
        .collect();
    // One commit per request, in an order that is not the ids'.
    for (_, request) in &all {
        records
            .commit(&[Record::Request(request.clone())])
            .map_err(failed("a commit"))?;
    }
    all.sort_by_key(|(id, _)| *id);
    // Listed by the records that wrote them: the files refuse to open on a state this release
    // does not know, as the store refuses to start on one.
    let listed = records.requests().map_err(failed("a list"))?;
    expect(listed == all, format!("listed {listed:?}, not {all:?}"))
}

fn host_ids_sorted(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    // Records of other kinds are no hosts.
    let others: Vec<Record> = every_kind()
        .into_iter()
        .filter(|record| !matches!(record, Record::Host(_)))
        .collect();
    records.commit(&others).map_err(failed("a commit"))?;
    let mut wanted = Vec::new();
    for name in ["web-10", "db-1", "web-1-a", "web-1"] {
        records
            .commit(&[Record::Host(host(name, &key(1)))])
            .map_err(failed("a commit"))?;
        wanted.push(host_id(name)?);
    }
    wanted.sort();
    let listed = subject.open().host_ids().map_err(failed("a list"))?;
    expect(
        listed == wanted,
        format!("listed {listed:?}, not {wanted:?}"),
    )
}

fn serial(subject: &dyn Subject) -> Result<(), String> {
    let take = |records: &dyn Records, reserving: Option<&pb::Request>| {
        records.take_serial(reserving).map_err(failed("a serial"))
    };
    let first = take(&*subject.open(), None)?;
    let second = take(&*subject.open(), None)?;
    let pending = request(3, pb::RequestState::Approved);
    let records = subject.open();
    records
        .commit(&[Record::Request(pending.clone())])
        .map_err(failed("a commit"))?;
    let third = take(&*records, Some(&pending))?;
    let fourth = take(&*subject.open(), None)?;
    let taken = [first, second, third, fourth];
    let wanted = [
        FIRST_SERIAL,
        FIRST_SERIAL + 1,
        FIRST_SERIAL + 2,
        FIRST_SERIAL + 3,
    ];
    expect(taken == wanted, format!("took {taken:?}, not {wanted:?}"))?;
    holds_all(
        &*subject.open(),
        &[Record::Request(pb::Request {
            serial: third,
            ..pending
        })],
    )
}

fn host_marks(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    let plain = pb::Host {
        services: vec!["api".to_owned(), "db".to_owned()],
        ..host("web-1", &key(1))
    };
    records
        .commit(&[Record::Host(plain.clone())])
        .map_err(failed("a commit"))?;
    records
        .set_first_heartbeat(&plain, 1_800_000_400)
        .map_err(failed("the first heartbeat"))?;
    let heard = pb::Host {
        first_heartbeat_unix: Some(1_800_000_400),
        ..plain.clone()
    };
    holds_all(&*subject.open(), &[Record::Host(heard.clone())])?;
    subject
        .open()
        .set_first_serving(&heard, 1_800_000_460)
        .map_err(failed("the first serving heartbeat"))?;
    let serving = pb::Host {
        first_serving_unix: Some(1_800_000_460),
        ..heard
    };
    holds_all(&*subject.open(), &[Record::Host(serving.clone())])?;
    subject
        .open()
        .set_running(&serving, running("1.0", 1_800_000_480))
        .map_err(failed("what the host runs"))?;
    let runs = pb::Host {
        running: Some(running("1.0", 1_800_000_480)),
        ..serving
    };
    holds_all(&*subject.open(), &[Record::Host(runs.clone())])?;
    subject
        .open()
        .set_running(&runs, running("1.1", 1_800_000_500))
        .map_err(failed("what the host runs, again"))?;
    let newer = pb::Host {
        running: Some(running("1.1", 1_800_000_500)),
        ..runs
    };
    holds_all(&*subject.open(), &[Record::Host(newer.clone())])?;
    subject
        .open()
        .set_duplicate(&newer)
        .map_err(failed("the duplicate mark"))?;
    holds_all(
        &*subject.open(),
        &[Record::Host(pb::Host {
            duplicate: true,
            ..newer
        })],
    )
}

fn failed_commit(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    let before = vec![Record::Host(host("web-1", &key(1)))];
    records.commit(&before).map_err(failed("a commit"))?;
    subject.fail_next_commit();
    let refused = vec![
        Record::Host(host("web-1", &key(2))),
        Record::Request(request(2, pb::RequestState::Pending)),
    ];
    expect(
        records.commit(&refused).is_err(),
        "the commit told to fail succeeded".to_owned(),
    )?;
    let reopened = subject.open();
    holds_all(&*reopened, &before)?;
    holds_none(&*reopened, &refused[1..])
}

fn unkeyed(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    let good = Record::Host(host("web-1", &key(1)));
    let bad = [
        Record::Request(pb::Request {
            request_id: vec![1, 2],
            ..request(1, pb::RequestState::Pending)
        }),
        Record::Issued(pb::Issued {
            request_id: vec![1],
            ..pb::Issued::default()
        }),
        Record::Host(host("Not_A_Host", &key(1))),
        Record::Deny(pb::Deny {
            host_id: "../web-1".to_owned(),
            ..pb::Deny::default()
        }),
        Record::MacNonce(pb::MacNonce {
            nonce_sha256: vec![1; 31],
            ..pb::MacNonce::default()
        }),
    ];
    for record in bad {
        let refused = records.commit(&[good.clone(), record.clone()]);
        expect(
            matches!(refused, Err(StoreError::Unkeyed(_))),
            format!("a commit of {record:?} answered {refused:?}"),
        )?;
    }
    holds_none(&*subject.open(), &[good])?;
    let unkeyed = pb::Request {
        request_id: vec![3],
        ..request(3, pb::RequestState::Approved)
    };
    let refused = records.take_serial(Some(&unkeyed));
    expect(
        matches!(refused, Err(StoreError::Unkeyed(_))),
        format!("a serial reserved in an unkeyed request answered {refused:?}"),
    )?;
    let first = records.take_serial(None).map_err(failed("a serial"))?;
    expect(
        first == FIRST_SERIAL,
        format!("a refused reservation took a serial: the next is {first}"),
    )
}

fn lock(subject: &dyn Subject) -> Result<(), String> {
    let records = subject.open();
    drop(records.lock().map_err(failed("the lock"))?);
    drop(records.lock().map_err(failed("the lock again"))?);
    Ok(())
}
