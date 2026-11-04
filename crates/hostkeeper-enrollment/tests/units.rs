//! The small parts: the name grammars, the key hash, the file-system port, the validity
//! window, and the exact text of every error.

mod support;

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_enrollment::ca::{CaError, ProfileError, SignError};
use hostkeeper_enrollment::enroll::{
    ApproveError, EnrollmentStore, IssueError, PollError, PostError, RequestState, StoreError,
};
use hostkeeper_enrollment::fs::{FaultFs, FileStat, Fs, Op, RealFs, temp_name, write_atomic};
use hostkeeper_enrollment::names::{BadName, HostId, KeyHash, Role, ServiceName};
use hostkeeper_enrollment::validity::{SystemClock, UnixClock, Window};
use support::{HostKey, scratch};

/// Catches a host id, role or service name outside its grammar being accepted, or one inside
/// it refused, at each boundary.
#[test]
fn names_follow_their_grammars() {
    let at = |n: usize| "a".repeat(n);
    for ok in ["a", "0", "web-1", "a-b-c", at(63).as_str()] {
        assert_eq!(HostId::parse(ok).expect(ok).as_str(), ok);
    }
    for bad in ["", "-a", "a-", "A", "a_b", "a.b", "a b", at(64).as_str()] {
        let refused = HostId::parse(bad).expect_err(bad);
        assert_eq!(
            refused,
            BadName {
                what: "host id",
                value: bad.to_owned(),
                grammar: HostId::GRAMMAR,
            }
        );
    }
    assert_eq!(
        HostId::parse("A").expect_err("upper").to_string(),
        "host id \"A\" does not match its grammar \
         (1 to 63 of a-z, 0-9 and -, not starting or ending with -)"
    );
    assert_eq!(Role::parse(&at(32)).expect("32").as_str(), at(32));
    for bad in ["", at(33).as_str(), "Web", "-web"] {
        assert_eq!(Role::parse(bad).expect_err(bad).what, "role");
    }
    for ok in [
        "api",
        "Api_v2",
        "a-b",
        "api-",
        "a.b",
        "_x",
        "0",
        at(255).as_str(),
    ] {
        assert_eq!(ServiceName::parse(ok).expect(ok).as_str(), ok);
    }
    for bad in ["", "-api", ".api", "a+b", "a b", "a/b", at(256).as_str()] {
        assert_eq!(ServiceName::parse(bad).expect_err(bad).what, "service name");
    }
    assert_eq!(HostId::parse("web-1").expect("id").to_string(), "web-1");
}

/// Catches a key hash with a second spelling (upper-case hex), of the wrong length, or not the
/// SHA-256 of the key.
#[test]
fn a_key_hash_has_one_spelling() {
    let key = HostKey::new();
    let hash = key.key_hash();
    assert_eq!(hash, key.p256().key_hash());
    let hex = hash.to_hex();
    assert_eq!(hex.len(), 64);
    assert_eq!(hash.to_string(), hex);
    assert_eq!(format!("{hash:?}"), format!("KeyHash({hex})"));
    assert_eq!(KeyHash::from_hex(&hex), Some(hash));
    assert_eq!(KeyHash::from_hex(&hex.to_uppercase()), None);
    assert_eq!(KeyHash::from_hex(&hex[..62]), None);
    assert_eq!(KeyHash::from_hex("zz"), None);
    assert_eq!(KeyHash::from_bytes(hash.as_bytes()), Some(hash));
    assert_eq!(KeyHash::from_bytes(&[0; 31]), None);
    assert!(format!("{:?}", key.p256()).starts_with("P256Key("));
}

/// Catches the real file system reporting a missing file as an error or another failure as
/// missing, following a link where it must not, leaving a stale temporary file's mode, or
/// listing out of order; and the fault-injecting one failing an operation other than the one
/// chosen.
#[test]
fn the_file_system_port_does_what_it_says() {
    let dir = scratch("fs-port");
    let fs: &dyn Fs = &RealFs;
    let file = dir.path().join("b.pb");
    assert_eq!(fs.read(&file).expect("missing"), None);
    assert_eq!(fs.read_private(&file).expect("missing"), None);
    assert!(fs.read(dir.path()).is_err(), "a directory is not a file");
    assert!(
        fs.read_private(dir.path()).is_err(),
        "a directory is not read"
    );
    // A stale temporary file of mode 0644 is written with the asked-for mode.
    let tmp = dir.path().join(temp_name("b.pb"));
    std::fs::write(&tmp, b"stale").expect("write");
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    write_atomic(fs, dir.path(), "b.pb", b"bytes", 0o600).expect("write");
    let (bytes, stat) = fs.read_private(&file).expect("read").expect("present");
    assert_eq!(bytes, b"bytes");
    assert_eq!((stat.is_file, stat.mode), (true, 0o600), "{stat:?}");
    assert!(!tmp.exists());
    std::fs::write(dir.path().join("a.pb"), b"").expect("write");
    std::fs::create_dir(dir.path().join("c")).expect("mkdir");
    assert_eq!(fs.list(dir.path()).expect("list"), ["a.pb", "b.pb", "c"]);
    assert!(fs.list(&dir.path().join("absent")).is_err());
    let nested = dir.path().join("x/y");
    fs.create_dir(&nested).expect("mkdir -p");
    fs.create_dir(&nested).expect("again");
    let mode = std::fs::metadata(&nested)
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    drop(fs.lock(&dir.path().join("lock")).expect("lock"));
    let relock = fs
        .lock(&dir.path().join("lock"))
        .expect("lock again once released");
    assert!(format!("{relock:?}").starts_with("FileLock"));
    assert!(fs.lock(&dir.path().join("absent/lock")).is_err());
    assert!(fs.sync_file(&dir.path().join("absent")).is_err());
    assert!(fs.sync_dir(&dir.path().join("absent")).is_err());

    let fault = FaultFs::new();
    fault.fail_at(2);
    assert!(fault.read(&file).is_ok());
    let err = fault.read(&file).expect_err("the second");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(err.to_string(), "injected failure at operation 2");
    assert!(fault.read(&file).is_ok(), "only the chosen one fails");
    let stray = dir.path().join("d");
    fault.create_dir(&stray).expect("mkdir");
    fault.list(dir.path()).expect("list");
    fault.lock(&dir.path().join("lock2")).expect("lock");
    fault.read_private(&file).expect("read");
    write_atomic(&fault, dir.path(), "e.pb", b"e", 0o600).expect("write");
    assert_eq!(fault.count(), 11);
    let ops = fault.ops();
    assert_eq!(ops[3], Op::CreateDir(stray));
    assert_eq!(ops[4], Op::List(dir.path().to_path_buf()));
    assert_eq!(ops[5], Op::Lock(dir.path().join("lock2")));
    assert_eq!(ops[6], Op::ReadPrivate(file.clone()));
    assert_eq!(ops[7], Op::WriteNew(dir.path().join("e.pb.tmp")));
    let never = FaultFs::new();
    never.fail_at(0);
    assert!(never.read(&file).is_ok());
    let _ = FileStat {
        is_file: true,
        uid: 0,
        mode: 0,
    };
    let store = EnrollmentStore::open(Arc::new(RealFs), &dir.path().join("store")).expect("open");
    assert!(format!("{store:?}").starts_with("EnrollmentStore("));
}

/// Catches a validity window whose end overflows or falls past what a certificate can hold
/// being accepted, and the system clock reading nothing.
#[test]
fn a_validity_window_fits_a_certificate() {
    assert_eq!(
        Window::from_now(10, 5),
        Some(Window {
            not_before: 10,
            not_after: 15
        })
    );
    assert_eq!(Window::from_now(0, 0).map(|w| w.not_after), Some(0));
    assert_eq!(Window::from_now(u64::MAX, 1), None);
    assert_eq!(
        Window::from_now(253_402_300_799, 0).map(|w| w.not_before),
        Some(253_402_300_799)
    );
    assert_eq!(Window::from_now(253_402_300_799, 1), None);
    assert_eq!(Window::from_now(u64::MAX, 0), None);
    assert!(SystemClock.now_unix() > 0);
}

/// Catches an error message that does not say what failed, or that names the wrong token.
#[test]
fn every_error_says_what_failed() {
    let path = PathBuf::from("dir/x.pb");
    let io = || io::Error::other("disk");
    let stat = FileStat {
        is_file: true,
        uid: 7,
        mode: 0o644,
    };
    let ca = [
        (
            CaError::Io {
                path: path.clone(),
                source: io(),
            },
            "cannot use dir/x.pb: disk",
        ),
        (
            CaError::KeyNotPrivate {
                path: path.clone(),
                uid: 8,
                stat,
            },
            "the CA key file dir/x.pb must be a regular file owned by uid 8 with mode 0600 (it is \
             FileStat { is_file: true, uid: 7, mode: 420 })",
        ),
        (
            CaError::Missing { path: path.clone() },
            "dir/x.pb does not exist; make the CA first",
        ),
        (
            CaError::Exists { path: path.clone() },
            "dir/x.pb already exists; the CA is made once",
        ),
        (
            CaError::BadPath { path: path.clone() },
            "dir/x.pb does not name a file",
        ),
        (
            CaError::BadKey {
                path: path.clone(),
                reason: "r".to_owned(),
            },
            "the CA key file dir/x.pb is not a PKCS#8 ECDSA P-256 key: r",
        ),
        (
            CaError::BadCertificate {
                path: path.clone(),
                reason: "r".to_owned(),
            },
            "the CA certificate file dir/x.pb is not a DER certificate: r",
        ),
        (
            CaError::CertificateNotOfKey { path: path.clone() },
            "the CA certificate dir/x.pb does not carry the CA key's public key",
        ),
        (
            CaError::ClockOutOfRange { now: 9 },
            "the clock reads 9, whose CA validity window does not fit a certificate",
        ),
        (CaError::Signing("s".to_owned()), "making the CA failed: s"),
    ];
    for (err, message) in ca {
        assert_eq!(err.to_string(), message);
    }
    let store = [
        (
            StoreError::Io {
                path: path.clone(),
                source: io(),
            },
            "cannot use dir/x.pb: disk",
        ),
        (
            StoreError::Corrupt {
                path: path.clone(),
                reason: "r".to_owned(),
            },
            "dir/x.pb is not a store file this release reads: r",
        ),
        (
            StoreError::NewerSchema {
                path: path.clone(),
                schema: 3,
            },
            "dir/x.pb has schema 3; this release reads schema 1",
        ),
        (
            StoreError::UnknownState {
                path: path.clone(),
                value: 7,
            },
            "dir/x.pb holds state 7, which this release does not know",
        ),
    ];
    for (err, message) in store {
        assert_eq!(err.to_string(), message);
    }
    let unknown = || StoreError::UnknownHost(HostId::parse("web-1").expect("id"));
    let messages = [
        (
            PostError::Store(unknown()).to_string(),
            "no host web-1 was approved",
        ),
        (
            PostError::Profile(ProfileError::KeyNotP256).to_string(),
            "key_not_p256: the key is not an uncompressed ECDSA P-256 key",
        ),
        (
            ApproveError::UnknownRequest.to_string(),
            "unknown_request: no request has this id",
        ),
        (
            ApproveError::RenewalOfOtherHost.to_string(),
            "renewal_of_other_host: the request renews another host id",
        ),
        (
            ApproveError::Store(unknown()).to_string(),
            "no host web-1 was approved",
        ),
        (
            IssueError::UnknownRequest.to_string(),
            "unknown_request: no request has this id",
        ),
        (
            IssueError::NotApproved(RequestState::Pending).to_string(),
            "the request is Pending, not approved",
        ),
        (IssueError::Denied.to_string(), "denied: the host is denied"),
        (
            IssueError::Sign(SignError::Signing("s".to_owned())).to_string(),
            "signing failed: s",
        ),
        (
            IssueError::Store(unknown()).to_string(),
            "no host web-1 was approved",
        ),
        (
            PollError::UnknownRequest.to_string(),
            "unknown_request: no request has this id",
        ),
        (
            PollError::BadSignature.to_string(),
            "bad_signature: the signature does not verify under the request's key",
        ),
        (
            PollError::Store(unknown()).to_string(),
            "no host web-1 was approved",
        ),
        (
            SignError::ClockOutOfRange { now: 1 }.to_string(),
            "the clock reads 1, whose validity window does not fit a certificate",
        ),
        (
            SignError::BadDnsName("x".to_owned()).to_string(),
            "\"x\" is not a DNS name the CA signs",
        ),
    ];
    for (got, want) in messages {
        assert_eq!(got, want);
    }
    assert_eq!(IssueError::Sign(SignError::EmptyValidity).token(), None);
    assert!(RequestState::DuplicateClaim.is_pending());
    assert!(!RequestState::Approved.is_pending());
}
