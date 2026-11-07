//! The store read whole and written back (`FileRecords::read_snapshot` and `write_snapshot`),
//! what `hostkeeper-coordinator import` and `export` move between the files and the database:
//! every record comes back as it was written, the read writes nothing, and it refuses what the
//! store refuses when it reads a record.

mod support;

use std::io;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::ca::check_request;
use hostkeeper_enrollment::enroll::records::{FileRecords, Snapshot, pb};
use hostkeeper_enrollment::enroll::{EnrollmentStore, MacNonceHash, RequestState, StoreError};
use hostkeeper_enrollment::fs::{FaultFs, FileLock, FileStat, Fs, Op, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use prost::Message;
use support::scratch;

const REQUEST: &[u8] = include_bytes!("fixtures/v1/request.pb");
const ISSUED: &[u8] = include_bytes!("fixtures/v1/issued.pb");
const HOST: &[u8] = include_bytes!("fixtures/v1/host.pb");
const DENY: &[u8] = include_bytes!("fixtures/v1/deny.pb");
const SERIAL: &[u8] = include_bytes!("fixtures/v1/serial.pb");
const CSR: &[u8] = include_bytes!("fixtures/v1/p256-csr.der");

/// The request id of the fixtures: the SHA-256 of the fixture CSR's key.
fn fixture_id() -> KeyHash {
    check_request(CSR)
        .expect("the fixture CSR")
        .key()
        .key_hash()
}

/// Writes `bytes` at `dir/path`, its directory made first.
fn put(dir: &Path, path: &str, bytes: &[u8]) {
    let at = dir.join(path);
    std::fs::create_dir_all(at.parent().expect("a parent")).expect("make the directory");
    std::fs::write(at, bytes).expect("write a record");
}

/// The nonce whose SHA-256 names a nonce file of the tests.
fn nonce_hash() -> MacNonceHash {
    MacNonceHash::of(b"a nonce of the tests, sixteen bytes or more")
}

/// The fixture request, approved with the nonce of [`nonce_hash`].
fn approved_with_nonce() -> Vec<u8> {
    let mut request = pb::Request::decode(REQUEST).expect("the fixture request");
    let approval = request.approved.get_or_insert_with(pb::Approval::default);
    approval.mac_nonce_sha256 = nonce_hash().as_bytes().to_vec();
    request.encode_to_vec()
}

/// The nonce of [`nonce_hash`], not marked consumed.
fn unmarked_nonce() -> Vec<u8> {
    pb::MacNonce {
        schema: 1,
        nonce_sha256: nonce_hash().as_bytes().to_vec(),
        serial: "C02TESTSERIAL".to_owned(),
        issued_unix: 1_800_000_000,
        consumed_by: Vec::new(),
        request_id: fixture_id().as_bytes().to_vec(),
    }
    .encode_to_vec()
}

/// A store directory holding every kind of record: the v1 fixtures, a request approved with a
/// Mac nonce and that nonce's file, not yet marked consumed.
fn every_record(dir: &Path) {
    let id = fixture_id();
    put(dir, &format!("requests/{id}.pb"), &approved_with_nonce());
    put(dir, &format!("issued/{id}.pb"), ISSUED);
    put(dir, "hosts/web-1.pb", HOST);
    put(dir, "deny/old-1.pb", DENY);
    put(
        dir,
        &format!("mac-nonces/{}.pb", hex(nonce_hash().as_bytes())),
        &unmarked_nonce(),
    );
    put(dir, "serial.pb", SERIAL);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read(dir: &Path) -> Result<Snapshot, StoreError> {
    FileRecords::read_snapshot(Arc::new(RealFs), dir)
}

/// Catches a record that is not read, a field lost on the way back, a nonce an approval consumed
/// read as unused, or a serial counter that is not written back at its value.
#[test]
fn a_snapshot_reads_every_record_and_writes_them_back() {
    let from = scratch("snapshot-from");
    every_record(from.path());
    let snapshot = read(from.path()).expect("read the snapshot");
    assert_eq!(
        (
            snapshot.requests.len(),
            snapshot.issued.len(),
            snapshot.hosts.len(),
            snapshot.deny.len(),
            snapshot.mac_nonces.len(),
        ),
        (1, 1, 1, 1, 1)
    );
    assert_eq!(
        snapshot.mac_nonces[0].consumed_by,
        fixture_id().as_bytes().to_vec(),
        "the approval consumed the nonce, as the store's open marks it"
    );
    let serial = pb::SerialCounter::decode(SERIAL).expect("the fixture serial");
    assert_eq!(snapshot.serial, Some(serial.next));
    assert!(!snapshot.is_empty());

    let into = scratch("snapshot-into");
    let target = into.path().join("enrollment");
    FileRecords::write_snapshot(Arc::new(RealFs), &target, &snapshot).expect("write it back");
    assert_eq!(read(&target).expect("read it again"), snapshot);
    let store = EnrollmentStore::open(Arc::new(RealFs), &target).expect("the store opens it");
    let view = store.request(&fixture_id()).expect("read").expect("held");
    assert_eq!(view.state, RequestState::Issued);
    assert_eq!(
        store.allocate_serial().expect("take a serial"),
        serial.next,
        "the counter goes on from where it stood"
    );
}

/// Catches a read that makes a directory, takes the store's lock or marks a nonce's file: the
/// import reads files it must leave as it found them. A store with no subdirectories reads as
/// one that holds nothing.
#[test]
fn a_snapshot_writes_nothing() {
    let from = scratch("snapshot-quiet");
    every_record(from.path());
    let fs = Arc::new(FaultFs::new());
    read_with(&fs, from.path()).expect("read the snapshot");
    let written: Vec<Op> = fs
        .ops()
        .into_iter()
        .filter(|op| !matches!(op, Op::Read(_) | Op::List(_)))
        .collect();
    assert_eq!(written, Vec::new());
    let nonce = std::fs::read(
        from.path()
            .join(format!("mac-nonces/{}.pb", hex(nonce_hash().as_bytes()))),
    )
    .expect("the nonce file");
    assert_eq!(nonce, unmarked_nonce(), "the nonce's file is not marked");

    let bare = scratch("snapshot-bare");
    let empty = read_with(&fs, bare.path()).expect("an empty directory reads");
    assert!(empty.is_empty());
    assert_eq!(std::fs::read_dir(bare.path()).expect("list").count(), 0);
}

fn read_with(fs: &Arc<FaultFs>, dir: &Path) -> Result<Snapshot, StoreError> {
    FileRecords::read_snapshot(fs.clone(), dir)
}

/// Catches a stray file (a temporary one a crash left, a name that is no key) read as a record:
/// the store never reads it.
#[test]
fn a_snapshot_passes_over_names_that_are_no_record() {
    let from = scratch("snapshot-stray");
    every_record(from.path());
    for stray in [
        "issued/left.pb.tmp",
        "issued/not-hex.pb",
        "hosts/Not_A_Host.pb",
        "deny/Not_A_Host.pb",
        "deny/old-1.pb.tmp",
        "mac-nonces/zz.pb",
        "mac-nonces/abcd.pb",
    ] {
        put(from.path(), stray, b"no record");
    }
    let snapshot = read(from.path()).expect("read the snapshot");
    let clean = scratch("snapshot-clean");
    every_record(clean.path());
    assert_eq!(snapshot, read(clean.path()).expect("read the clean one"));
}

/// Catches a record read under another key than its own: the database keys every record by its
/// fields, so a host file named for one host and holding another would be imported as that
/// other host.
#[test]
fn a_snapshot_refuses_a_record_not_kept_under_its_own_key() {
    for (path, bytes) in [
        ("hosts/web-2.pb", HOST),
        ("deny/old-2.pb", DENY),
        (
            "issued/0000000000000000000000000000000000000000000000000000000000000000.pb",
            ISSUED,
        ),
    ] {
        let from = scratch("snapshot-misnamed");
        every_record(from.path());
        put(from.path(), path, bytes);
        match read(from.path()) {
            Err(StoreError::Corrupt { path: at, reason }) => {
                assert_eq!(at, from.path().join(path));
                assert_eq!(
                    reason,
                    "the record's own key is not the one its file is named by"
                );
            }
            other => panic!("{path}: {other:?}"),
        }
    }
    let from = scratch("snapshot-misnamed-nonce");
    every_record(from.path());
    let other = MacNonceHash::of(b"another nonce of the tests, sixteen bytes");
    let path = format!("mac-nonces/{}.pb", hex(other.as_bytes()));
    put(from.path(), &path, &unmarked_nonce());
    assert!(matches!(
        read(from.path()),
        Err(StoreError::Corrupt { path: at, .. }) if at == from.path().join(&path)
    ));
}

/// Catches a snapshot that takes a record the store would refuse when it reads it: a deny row in
/// a state this release does not know, a request whose host ids do not parse, a host whose key
/// hash is not 32 bytes, a record of a newer schema.
#[test]
fn a_snapshot_refuses_what_the_store_refuses() {
    let id = fixture_id();
    let mut deny = pb::Deny::decode(DENY).expect("the fixture deny row");
    deny.state = 9;
    let mut renewal = pb::Request::decode(REQUEST).expect("the fixture request");
    renewal.renewal_of = "Not A Host".to_owned();
    let mut approval = pb::Request::decode(REQUEST).expect("the fixture request");
    approval
        .approved
        .get_or_insert_with(pb::Approval::default)
        .host_id = "Not A Host".to_owned();
    let mut host = pb::Host::decode(HOST).expect("the fixture host");
    host.key_hash = vec![1, 2, 3];
    let mut newer = pb::Host::decode(HOST).expect("the fixture host");
    newer.schema = 2;
    let mut serial = pb::SerialCounter::decode(SERIAL).expect("the fixture serial");
    serial.schema = 0;
    let request = format!("requests/{id}.pb");
    let cases: [(&str, Vec<u8>, &str); 6] = [
        ("deny/old-1.pb", deny.encode_to_vec(), "state"),
        (&request, renewal.encode_to_vec(), "corrupt"),
        (&request, approval.encode_to_vec(), "corrupt"),
        ("hosts/web-1.pb", host.encode_to_vec(), "corrupt"),
        ("hosts/web-1.pb", newer.encode_to_vec(), "newer"),
        ("serial.pb", serial.encode_to_vec(), "corrupt"),
    ];
    for (path, bytes, kind) in cases {
        let from = scratch("snapshot-refused");
        every_record(from.path());
        put(from.path(), path, &bytes);
        let refused = read(from.path());
        let named = match (&refused, kind) {
            (Err(StoreError::UnknownState { path, .. }), "state")
            | (Err(StoreError::Corrupt { path, .. }), "corrupt")
            | (Err(StoreError::NewerSchema { path, .. }), "newer") => path.clone(),
            _ => panic!("{path}: {refused:?}"),
        };
        assert_eq!(named, from.path().join(path));
    }
}

/// [`RealFs`], with one name more in the listing of the subdirectory it names than it holds.
struct Phantom(&'static str);

impl Fs for Phantom {
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        RealFs.read(path)
    }
    fn read_private(&self, path: &Path) -> io::Result<Option<(Vec<u8>, FileStat)>> {
        RealFs.read_private(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        RealFs.write_new(path, bytes, mode)
    }
    fn sync_file(&self, path: &Path) -> io::Result<()> {
        RealFs.sync_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        RealFs.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        RealFs.remove(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        RealFs.sync_dir(dir)
    }
    fn create_dir(&self, dir: &Path) -> io::Result<()> {
        RealFs.create_dir(dir)
    }
    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = RealFs.list(dir)?;
        if dir.file_name().and_then(|name| name.to_str()) == Some(self.0) {
            names.push(match self.0 {
                "hosts" | "deny" => "gone-1.pb".to_owned(),
                _ => format!("{}.pb", "ab".repeat(32)),
            });
        }
        Ok(names)
    }
    fn lock(&self, path: &Path) -> io::Result<FileLock> {
        RealFs.lock(path)
    }
}

/// Catches a record listed and then not read taken as one that never was: the store changed
/// while it was read, which the import refuses rather than move part of it.
#[test]
fn a_snapshot_refuses_a_record_that_is_gone_after_it_was_listed() {
    for sub in ["issued", "hosts", "deny", "mac-nonces"] {
        let from = scratch("snapshot-gone");
        every_record(from.path());
        let refused = FileRecords::read_snapshot(Arc::new(Phantom(sub)), from.path());
        assert!(
            matches!(
                &refused,
                Err(StoreError::Corrupt { reason, .. })
                    if reason == "the file was listed and is gone: the store changed while it was read"
            ),
            "{sub}: {refused:?}"
        );
    }
}
