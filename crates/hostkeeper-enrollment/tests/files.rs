//! The store's files across releases: a file of this release is read by the next (the
//! checked-in v1 fixtures, encoded independently of this crate's encoder), a field the next
//! release adds is ignored, and a file of a newer schema is refused.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::ca::check_request;
use hostkeeper_enrollment::enroll::{
    Claims, EnrollmentStore, IssueError, RequestState, StoreError,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::KeyHash;
use support::{TestCa, host_id, scratch};

const REQUEST: &[u8] = include_bytes!("fixtures/v1/request.pb");
const ISSUED: &[u8] = include_bytes!("fixtures/v1/issued.pb");
const HOST: &[u8] = include_bytes!("fixtures/v1/host.pb");
const DENY: &[u8] = include_bytes!("fixtures/v1/deny.pb");
const SERIAL: &[u8] = include_bytes!("fixtures/v1/serial.pb");
const CSR: &[u8] = include_bytes!("fixtures/v1/p256-csr.der");

/// A field number no message of schema 1 uses (99, varint, value 1).
const UNKNOWN_FIELD: [u8; 3] = [0x98, 0x06, 0x01];

/// The request id of the fixtures: the SHA-256 of the fixture CSR's key.
fn fixture_id() -> KeyHash {
    check_request(CSR)
        .expect("the fixture CSR")
        .key()
        .key_hash()
}

/// A store directory holding the fixtures, each passed through `edit`.
fn store_of(dir: &Path, edit: impl Fn(&[u8]) -> Vec<u8>) -> EnrollmentStore {
    let store = EnrollmentStore::open(Arc::new(RealFs), dir).expect("open");
    let id = fixture_id();
    let files: [(String, &[u8]); 5] = [
        (format!("requests/{id}.pb"), REQUEST),
        (format!("issued/{id}.pb"), ISSUED),
        ("hosts/web-1.pb".to_owned(), HOST),
        ("deny/old-1.pb".to_owned(), DENY),
        ("serial.pb".to_owned(), SERIAL),
    ];
    for (path, bytes) in files {
        std::fs::write(dir.join(path), edit(bytes)).expect("write a fixture");
    }
    store
}

/// The store failure inside an issuance error.
fn store_error(err: IssueError) -> StoreError {
    match err {
        IssueError::Store(err) => err,
        other => panic!("not a store failure: {other}"),
    }
}

/// What each fixture holds, read through the store.
fn assert_fixtures_read(store: &EnrollmentStore, ca: &TestCa) {
    let id = fixture_id();
    let view = store.request(&id).expect("read").expect("held");
    assert_eq!(view.state, RequestState::Issued);
    assert_eq!(
        view.claims,
        Claims {
            machine_id: "fixture-machine".to_owned(),
            dmi_serial: "SN-1".to_owned(),
            dmi_product_uuid: "00000000-0000-4000-8000-000000000001".to_owned(),
            addresses: vec!["192.0.2.10".to_owned()],
            mac_serial: String::new(),
            mac_platform_uuid: String::new(),
            // The release that wrote the fixtures had no BMC claim: it reads as none.
            bmc_mac: String::new(),
        }
    );
    assert_eq!(view.image, "hostkeeper-image-1");
    assert_eq!((view.posted_unix, view.round), (1_800_000_000, 0));
    assert_eq!(
        (view.renewal_of, view.host_id),
        (None, Some(host_id("web-1")))
    );
    let issued = store.issue(&ca.ca, &id).expect("the issued file");
    assert_eq!(issued.certificate_der, b"fixture certificate bytes");
    assert_eq!(
        (issued.serial, issued.not_after, issued.round),
        (2, 1_800_604_800, 0)
    );
    assert_eq!(issued.host_id, host_id("web-1"));
    let web = host_id("web-1");
    assert!(store.is_duplicate(&web).expect("host"));
    assert!(store.renew_allowed(&web, &id).expect("host"));
    let old = host_id("old-1");
    assert!(store.is_denied(&old).expect("deny"));
    assert!(store.is_retiring(&old).expect("deny"));
    assert_eq!(store.allocate_serial().expect("serial"), 3);
    assert!(store.pending().expect("pending").is_empty());
}

/// Catches a change to a store message that the next release cannot read: a file of each
/// message, written as schema 1 and checked in, is read back field by field.
#[test]
fn a_store_file_written_by_this_release_reads_in_the_next() {
    let ca = TestCa::new("fixtures-ca");
    let dir = scratch("fixtures");
    let store = store_of(dir.path(), <[u8]>::to_vec);
    assert_fixtures_read(&store, &ca);
}

/// Catches a reader that reads a file of a newer schema as if it knew it, or that refuses a
/// file only because it carries a field added later.
#[test]
fn the_store_refuses_a_newer_schema_and_ignores_unknown_fields() {
    let ca = TestCa::new("schema-ca");
    let extended = scratch("schema-extended");
    let store = store_of(extended.path(), |bytes| [bytes, &UNKNOWN_FIELD].concat());
    assert_fixtures_read(&store, &ca);

    let newer = scratch("schema-newer");
    // Field 1 (schema) is the first field of every fixture: `08 01` becomes `08 02`.
    let store = store_of(newer.path(), |bytes| {
        let mut bytes = bytes.to_vec();
        assert_eq!(bytes[..2], [0x08, 0x01]);
        bytes[1] = 0x02;
        bytes
    });
    let id = fixture_id();
    let web = host_id("web-1");
    let refusals = [
        store.request(&id).map(|_| ()).expect_err("request"),
        store
            .issue(&ca.ca, &id)
            .map(|_| ())
            .map_err(store_error)
            .expect_err("request"),
        store.is_duplicate(&web).map(|_| ()).expect_err("host"),
        store
            .is_denied(&host_id("old-1"))
            .map(|_| ())
            .expect_err("deny"),
        store.allocate_serial().map(|_| ()).expect_err("serial"),
    ];
    for refused in refusals {
        assert!(
            matches!(refused, StoreError::NewerSchema { schema: 2, .. }),
            "{refused}"
        );
        assert!(
            refused
                .to_string()
                .ends_with(".pb has schema 2; this release reads schema 1"),
            "{refused}"
        );
    }
    // An issued file of a newer schema, beside a request this release reads.
    let issued_only = scratch("schema-issued");
    let store = store_of(issued_only.path(), <[u8]>::to_vec);
    let mut newer_issued = ISSUED.to_vec();
    newer_issued[1] = 0x02;
    std::fs::write(
        issued_only.path().join(format!("issued/{id}.pb")),
        newer_issued,
    )
    .expect("write");
    let refused = store.issue(&ca.ca, &id).expect_err("newer issued file");
    assert!(
        refused
            .to_string()
            .ends_with("has schema 2; this release reads schema 1"),
        "{refused}"
    );
}

/// Catches a host file of the release before (no `first_heartbeat_unix`, field 8) read as a
/// host already heard, or refused; and a first heartbeat written into it that loses what the
/// file held (its key, its duplicate mark) or does not read back. The release before reads the
/// file written here as it reads any file with a field it does not know (the test above).
#[test]
fn a_host_file_of_the_release_before_takes_its_first_heartbeat() {
    let ca = TestCa::new("first-beat-fixture-ca");
    let dir = scratch("first-beat-fixture");
    let store = store_of(dir.path(), <[u8]>::to_vec);
    let web = host_id("web-1");
    let enrolled = |store: &EnrollmentStore| {
        store
            .enrolled_host(&web)
            .expect("read")
            .expect("enrolled")
            .first_heartbeat_unix
    };
    assert_eq!(enrolled(&store), None);
    store.note_heartbeat(&web, 1_800_000_500).expect("noted");
    assert_ne!(
        std::fs::read(dir.path().join("hosts/web-1.pb")).expect("read"),
        HOST
    );
    let reopened = EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("open");
    assert_eq!(enrolled(&reopened), Some(1_800_000_500));
    assert_fixtures_read(&reopened, &ca);
}

/// Catches a reader that accepts a file with no schema (field 1 absent or 0), or a deny row
/// in a state it does not know, instead of refusing it.
#[test]
fn a_file_without_schema_or_with_an_unknown_deny_state_is_refused() {
    let dir = scratch("no-schema");
    let store = store_of(dir.path(), |bytes| {
        let mut bytes = bytes.to_vec();
        bytes[1] = 0x00;
        bytes
    });
    let id = fixture_id();
    let refusals = [
        store.request(&id).map(|_| ()).expect_err("request"),
        store
            .is_duplicate(&host_id("web-1"))
            .map(|_| ())
            .expect_err("host"),
        store
            .is_denied(&host_id("old-1"))
            .map(|_| ())
            .expect_err("deny"),
        store.allocate_serial().map(|_| ()).expect_err("serial"),
    ];
    for refused in refusals {
        assert!(
            refused
                .to_string()
                .ends_with(".pb is not a store file this release reads: the file has no schema"),
            "{refused}"
        );
    }
    let states = scratch("deny-state");
    let store = store_of(states.path(), <[u8]>::to_vec);
    for state in [0x00, 0x04, 0x63] {
        let mut deny = DENY.to_vec();
        deny.extend_from_slice(&[0x18, state]);
        std::fs::write(states.path().join("deny/old-1.pb"), deny).expect("write");
        let refused = store
            .is_denied(&host_id("old-1"))
            .expect_err("unknown state");
        assert!(
            refused.to_string().ends_with(&format!(
                ".pb holds state {state}, which this release does not know"
            )),
            "{refused}"
        );
    }
    let mut not_protobuf = SERIAL.to_vec();
    not_protobuf.extend_from_slice(&[0xff, 0xff]);
    std::fs::write(states.path().join("serial.pb"), not_protobuf).expect("write");
    let refused = store.allocate_serial().expect_err("not protobuf");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
}

/// Catches a request file read under another request's id, a host file with a key hash that
/// is not 32 bytes, and a renewal or host id outside the grammar, being read as valid.
#[test]
fn an_inconsistent_store_file_is_refused() {
    let dir = scratch("inconsistent");
    let store = store_of(dir.path(), <[u8]>::to_vec);
    let elsewhere = KeyHash::of_spki(b"another key");
    std::fs::write(dir.path().join(format!("requests/{elsewhere}.pb")), REQUEST).expect("write");
    let refused = store.request(&elsewhere).expect_err("not its id");
    assert!(
        refused.to_string().ends_with(
            "is not a store file this release reads: the request's key is not the one its id names"
        ),
        "{refused}"
    );
    // The pending list reads every request, so it is refused too.
    assert!(store.pending().is_err());

    let mut short_key = vec![0x08, 0x01, 0x12, 0x05];
    short_key.extend_from_slice(b"web-2");
    short_key.extend_from_slice(&[0x1a, 0x01, 0x00]);
    std::fs::write(dir.path().join("hosts/web-2.pb"), short_key).expect("write");
    let refused = store
        .is_duplicate(&host_id("web-2"))
        .expect_err("short key");
    assert!(
        refused
            .to_string()
            .ends_with("the key hash is not 32 bytes"),
        "{refused}"
    );

    // A later `renewal_of` (field 7) that is not a host id.
    let id = fixture_id();
    let mut bad_renewal = REQUEST.to_vec();
    bad_renewal.extend_from_slice(&[0x3a, 0x06]);
    bad_renewal.extend_from_slice(b"Bad_Id");
    std::fs::write(dir.path().join(format!("requests/{id}.pb")), bad_renewal).expect("write");
    let refused = store.request(&id).expect_err("bad renewal_of");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");

    // A request state this release does not know (field 8 = 9).
    let mut unknown_state = REQUEST.to_vec();
    unknown_state.extend_from_slice(&[0x40, 0x09]);
    std::fs::write(dir.path().join(format!("requests/{id}.pb")), unknown_state).expect("write");
    let refused = store.request(&id).expect_err("unknown state");
    assert!(
        refused
            .to_string()
            .ends_with(".pb holds state 9, which this release does not know"),
        "{refused}"
    );
    // An approved request whose approval names no host id: a later approval (field 9) merges
    // into the first and sets its host id (field 1) to "".
    let mut no_host = REQUEST.to_vec();
    no_host.extend_from_slice(&[0x4a, 0x02, 0x0a, 0x00]);
    std::fs::write(dir.path().join(format!("requests/{id}.pb")), no_host).expect("write");
    let ca = TestCa::new("inconsistent-ca");
    let refused = store.issue(&ca.ca, &id).expect_err("no host id");
    assert!(
        refused
            .to_string()
            .ends_with("an approved request names no host id"),
        "{refused}"
    );
}

/// Catches a deny that does not mark its host's request when the request file is missing,
/// being reported as done.
#[test]
fn a_host_whose_request_is_missing_is_refused_at_deny() {
    let dir = scratch("missing-request");
    let store = store_of(dir.path(), <[u8]>::to_vec);
    let id = fixture_id();
    std::fs::remove_file(dir.path().join(format!("requests/{id}.pb"))).expect("rm");
    let refused = store.deny(&host_id("web-1")).expect_err("missing request");
    assert!(
        refused.to_string().ends_with(
            "hosts/web-1.pb is not a store file this release reads: the host's request is missing"
        ),
        "{refused}"
    );
}
