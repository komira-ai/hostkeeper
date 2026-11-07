//! The Mac nonce's binding to the request it was issued for, across the format change that
//! added it, and a Mac's own host id taken over from the key it held before a new MDM
//! enrollment (docs/design/hostkeeper-mdm.md, sections 5.1 and 5.2), over a scratch directory.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    ApprovalRecord, ApproveError, Claims, EnrollmentStore, MacNonceHash, MacNonceRefusal,
    NewRequest,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::{KeyHash, Role};
use prost::Message;
use support::{HostKey, NOW, claims, decision, host_id, request, scratch};

/// The serial number of the Mac in these tests.
const SERIAL: &str = "C02XK1ABJG5J";

/// The host id the Mac is approved as.
const MAC_HOST: &str = "mac-c02xk1abjg5j";

/// `MacNonce` as the release before the request field wrote and read it: fields 1 to 5.
#[derive(Clone, PartialEq, prost::Message)]
struct PreviousMacNonce {
    #[prost(uint32, tag = "1")]
    schema: u32,
    #[prost(bytes = "vec", tag = "2")]
    nonce_sha256: Vec<u8>,
    #[prost(string, tag = "3")]
    serial: String,
    #[prost(uint64, tag = "4")]
    issued_unix: u64,
    #[prost(bytes = "vec", tag = "5")]
    consumed_by: Vec<u8>,
}

fn open(dir: &Path) -> EnrollmentStore {
    EnrollmentStore::open(Arc::new(RealFs), dir).expect("open")
}

/// A Mac's request from `machine_id` claiming `serial`, which presented a nonce.
fn mac_request(key: &HostKey, machine_id: &str, serial: &str) -> NewRequest {
    NewRequest {
        claims: Claims {
            mac_serial: serial.to_owned(),
            ..claims(machine_id)
        },
        mac_nonce_presented: true,
        ..request(key, machine_id)
    }
}

/// The record of an approval by the nonce of `hash` at `at`, of a Mac whose current MDM
/// enrollment began at `since`.
fn by_nonce(hash: Option<MacNonceHash>, at: u64, since: Option<u64>) -> ApprovalRecord {
    ApprovalRecord {
        approver: "mdm-nonce".to_owned(),
        approved_unix: at,
        reason: "approved by its MDM nonce".to_owned(),
        mac_nonce: hash,
        mac_enrolled_since_unix: since,
    }
}

/// The path of the file of the nonce `hash` in the store at `dir`.
fn nonce_file(dir: &Path, hash: &MacNonceHash) -> std::path::PathBuf {
    dir.join("mac-nonces")
        .join(format!("{}.pb", hex::encode(hash.as_bytes())))
}

/// Posts `key`'s Mac request from `machine_id`, claiming `serial`: its id.
fn post(store: &EnrollmentStore, key: &HostKey, machine_id: &str, serial: &str) -> KeyHash {
    store
        .post(mac_request(key, machine_id, serial), NOW)
        .expect("post")
        .request_id
}

/// Catches a nonce that approves a request other than the one it was issued for, the binding
/// checked before the serial, and the token of the refusal: another key is refused
/// `nonce_wrong_request` and the nonce stays unused, the bound key is then approved by it, and
/// a nonce of another serial and another request is `nonce_wrong_serial`.
#[test]
fn a_nonce_approves_only_the_request_it_was_bound_to() {
    let dir = scratch("bound");
    let store = open(dir.path());
    let (daemon, local) = (HostKey::new(), HostKey::new());
    let hash = MacNonceHash::of(b"bound-nonce-0123456789abcdef0123");
    assert!(
        store
            .issue_mac_nonce(SERIAL, &daemon.key_hash(), &hash, NOW)
            .expect("issue")
    );
    let theirs = post(&store, &local, "machine-local", SERIAL);
    let wrong = store
        .approve_recorded(
            &theirs,
            &theirs,
            &decision(MAC_HOST, false),
            &by_nonce(Some(hash), NOW, None),
        )
        .expect_err("another request");
    assert!(
        matches!(wrong, ApproveError::MacNonce(MacNonceRefusal::WrongRequest)),
        "{wrong:?}"
    );
    assert_eq!(wrong.token(), Some("nonce_wrong_request"));
    assert_eq!(
        wrong.to_string(),
        "nonce_wrong_request: the nonce was issued for another request"
    );
    let ours = post(&store, &daemon, "machine-daemon", SERIAL);
    let approval = store
        .approve_recorded(
            &ours,
            &ours,
            &decision(MAC_HOST, false),
            &by_nonce(Some(hash), NOW, Some(NOW - 1)),
        )
        .expect("the bound request is approved");
    // A host id no key held is taken by the first approval, which replaces nothing.
    assert!(!approval.replace);

    let elsewhere = MacNonceHash::of(b"elsewhere-nonce-0123456789abcdef");
    assert!(
        store
            .issue_mac_nonce("C02OTHER0001", &daemon.key_hash(), &elsewhere, NOW)
            .expect("issue")
    );
    let third = post(&store, &HostKey::new(), "machine-third", SERIAL);
    let refused = store
        .approve_recorded(
            &third,
            &third,
            &decision("mac-third", false),
            &by_nonce(Some(elsewhere), NOW, None),
        )
        .expect_err("another serial and another request");
    assert_eq!(refused.token(), Some("nonce_wrong_serial"));
}

/// Catches the request field read as required, or not ignored by the release before it, and a
/// field that names no key read as no binding: a nonce file the release before wrote (no
/// request field) approves any request claiming its serial, a file this release writes reads
/// under the previous message with every earlier field as written, and a request field that is
/// not 32 bytes approves no request.
#[test]
fn a_nonce_without_a_request_id_is_judged_as_before() {
    let dir = scratch("unbound");
    let store = open(dir.path());

    // Written by the release before the field.
    let old = MacNonceHash::of(b"old-nonce-0123456789abcdef012345");
    let previous = PreviousMacNonce {
        schema: 1,
        nonce_sha256: old.as_bytes().to_vec(),
        serial: SERIAL.to_owned(),
        issued_unix: NOW,
        consumed_by: Vec::new(),
    };
    std::fs::write(nonce_file(dir.path(), &old), previous.encode_to_vec()).expect("written");
    let key = HostKey::new();
    let id = post(&store, &key, "machine-old", SERIAL);
    store
        .approve_recorded(
            &id,
            &id,
            &decision(MAC_HOST, false),
            &by_nonce(Some(old), NOW + 10, None),
        )
        .expect("a nonce without a request field approves as before");

    // Written by this release, read by the release before it.
    let new = MacNonceHash::of(b"new-nonce-0123456789abcdef012345");
    let bound = HostKey::new().key_hash();
    assert!(
        store
            .issue_mac_nonce(SERIAL, &bound, &new, NOW)
            .expect("issue")
    );
    let bytes = std::fs::read(nonce_file(dir.path(), &new)).expect("read");
    assert_eq!(
        PreviousMacNonce::decode(bytes.as_slice()).expect("the previous release reads it"),
        PreviousMacNonce {
            schema: 1,
            nonce_sha256: new.as_bytes().to_vec(),
            serial: SERIAL.to_owned(),
            issued_unix: NOW,
            consumed_by: Vec::new(),
        }
    );
    // The added field, field 6, holds the request id and nothing else was added.
    let mut tail = vec![0x32, 0x20];
    tail.extend_from_slice(bound.as_bytes());
    assert!(bytes.ends_with(&tail), "{bytes:?}");
    assert_eq!(
        bytes.len(),
        previous_len(&bytes[..bytes.len() - tail.len()]) + tail.len()
    );

    // A request field that is no key hash binds to no request.
    let odd = MacNonceHash::of(b"odd-nonce-0123456789abcdef012345");
    let mut written = PreviousMacNonce {
        nonce_sha256: odd.as_bytes().to_vec(),
        ..previous
    }
    .encode_to_vec();
    written.extend_from_slice(&[0x32, 0x03, 1, 2, 3]);
    std::fs::write(nonce_file(dir.path(), &odd), written).expect("written");
    let other = post(&store, &HostKey::new(), "machine-odd", SERIAL);
    let refused = store
        .approve_recorded(
            &other,
            &other,
            &decision("mac-odd", false),
            &by_nonce(Some(odd), NOW, None),
        )
        .expect_err("a malformed request field");
    assert_eq!(refused.token(), Some("nonce_wrong_request"));
}

/// The length of `bytes` when they are exactly one previous-release `MacNonce`.
fn previous_len(bytes: &[u8]) -> usize {
    PreviousMacNonce::decode(bytes)
        .expect("the earlier fields")
        .encoded_len()
}

/// One way the takeover of a Mac's own host id is asked for: what it changes from the case
/// where every condition holds.
#[derive(Clone, Copy, Debug)]
enum Takeover {
    /// The enrollment began one second after the old key's approval.
    Holds,
    /// The enrollment began in the second of the old key's approval.
    SameSecond,
    /// The gate does not know when the enrollment began.
    Unknown,
    /// The approval names the enrollment time but no nonce.
    NoNonce,
    /// The old key's request claimed another serial.
    OtherSerial,
    /// The old key's approval was recorded without a time.
    Untimed,
    /// The old key's request file is gone.
    RequestGone,
    /// The host id is denied.
    Denied,
    /// The new request is flagged a duplicate claim.
    Duplicate,
    /// The Mac has no serial, nor had the old key's request.
    NoSerial,
}

/// Catches a host id taken over from a key that is not the same Mac's earlier one, before the
/// new enrollment, from a denied host, over a duplicate claim, or without a nonce, and the
/// takeover not recorded as replacing: each row changes one condition, only the row that
/// changes none takes the host id over (recorded `replace`, the old key no longer holding it),
/// and every other is refused as an approval of a taken, denied or duplicate host id is.
#[test]
fn a_mac_takes_its_own_host_id_over_only_after_a_new_enrollment() {
    use Takeover::*;
    let rows = [
        (Holds, None),
        (SameSecond, Some("host_id_taken")),
        (Unknown, Some("host_id_taken")),
        (NoNonce, Some("host_id_taken")),
        (OtherSerial, Some("host_id_taken")),
        (Untimed, Some("host_id_taken")),
        (RequestGone, Some("host_id_taken")),
        (Denied, Some("denied")),
        (Duplicate, Some("duplicate_claim")),
        (NoSerial, Some("host_id_taken")),
    ];
    for (n, (row, refusal)) in rows.into_iter().enumerate() {
        let dir = scratch(&format!("takeover-{n}"));
        let store = open(dir.path());
        let serial = match row {
            NoSerial => "",
            _ => SERIAL,
        };
        let held_serial = match row {
            OtherSerial => "C02OTHER0001",
            _ => serial,
        };
        let old = HostKey::new();
        let approved_at = NOW + 100;
        post(&store, &old, "machine-old", held_serial);
        match row {
            Untimed => {
                store
                    .approve(&old.key_hash(), &old.key_hash(), &decision(MAC_HOST, false))
                    .expect("approved without a record");
            }
            _ => {
                let id = old.key_hash();
                store
                    .approve_recorded(
                        &id,
                        &id,
                        &decision(MAC_HOST, false),
                        &by_nonce(None, approved_at, None),
                    )
                    .expect("the old key approved");
            }
        }
        if let RequestGone = row {
            std::fs::remove_file(
                dir.path()
                    .join("requests")
                    .join(format!("{}.pb", old.key_hash())),
            )
            .expect("removed");
        }
        if let Denied = row {
            store.deny(&host_id(MAC_HOST)).expect("denied");
        }
        let since = match row {
            SameSecond => Some(approved_at),
            Unknown => None,
            _ => Some(approved_at + 1),
        };
        let new = HostKey::new();
        let machine = match row {
            Duplicate => "machine-old",
            _ => "machine-new",
        };
        let id = post(&store, &new, machine, serial);
        let hash = MacNonceHash::of(format!("takeover-{n}-0123456789abcdef").as_bytes());
        assert!(
            store
                .issue_mac_nonce(serial, &id, &hash, NOW)
                .expect("issue")
        );
        let nonce = match row {
            NoNonce => None,
            _ => Some(hash),
        };
        let at = NOW + 200;
        let result = store.approve_recorded(
            &id,
            &id,
            &decision(MAC_HOST, false),
            &by_nonce(nonce, at, since),
        );
        let holder = |key: &HostKey| {
            store
                .host_role_for(&host_id(MAC_HOST), &key.key_hash())
                .expect("read")
                .is_some()
        };
        match refusal {
            None => {
                let approval = result.expect("taken over");
                assert!(approval.replace, "{row:?}");
                assert!(!approval.cleared_deny, "{row:?}");
                assert!(holder(&new) && !holder(&old), "{row:?}");
                let recorded = store.recorded_approvals().expect("read");
                let mine = recorded
                    .iter()
                    .find(|approval| approval.request_id == id)
                    .expect("recorded");
                assert!(mine.replace && mine.mac_nonce, "{row:?}");
                assert_eq!(
                    store.host_role(&host_id(MAC_HOST)).expect("read"),
                    Some(Role::parse("web").expect("a role"))
                );
            }
            Some(token) => {
                let refused = result.expect_err("refused");
                assert_eq!(refused.token(), Some(token), "{row:?}");
                assert!(!holder(&new), "{row:?}");
            }
        }
    }
}

/// Catches a takeover that does not hold for an operator's own `replace`: an approval asked to
/// replace, with a nonce and an enrollment time that alone would not take the host id over, is
/// recorded replacing, as it was before.
#[test]
fn an_approval_asked_to_replace_still_replaces() {
    let dir = scratch("asked");
    let store = open(dir.path());
    let old = HostKey::new();
    let old_id = post(&store, &old, "machine-old", "C02OTHER0001");
    store
        .approve(&old_id, &old_id, &decision(MAC_HOST, false))
        .expect("approved");
    let new = HostKey::new();
    let id = post(&store, &new, "machine-new", SERIAL);
    let hash = MacNonceHash::of(b"asked-nonce-0123456789abcdef0123");
    assert!(
        store
            .issue_mac_nonce(SERIAL, &id, &hash, NOW)
            .expect("issue")
    );
    let approval = store
        .approve_recorded(
            &id,
            &id,
            &decision(MAC_HOST, true),
            &by_nonce(Some(hash), NOW, Some(NOW)),
        )
        .expect("replaced");
    assert!(approval.replace);
}
