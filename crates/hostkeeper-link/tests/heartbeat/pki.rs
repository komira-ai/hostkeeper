//! The file-based certificate source, [`PkiCertSource`], over a pki directory that the writer of
//! `hostkeeper-enroll` lays out, with a test PKI made at run time (no key is committed).

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enroll::{KeyFrom, PkiDir, Store, StoreLock};
use hostkeeper_link::{
    BeatOutcome, CertSource, CertState, ClientIdentity, DEFAULT_HEARTBEAT_SECS, Heartbeater,
    LoopConfig, PkiCertSource, Platform,
};
use hostkeeper_testkit::Scratch;
use p256::ecdsa::SigningKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rcgen::{CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};

use crate::{Rig, version};

/// A P-256 key, as enrollment makes one.
fn new_key() -> KeyPair {
    KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a P-256 key")
}

/// A test CA and the certificates it issues.
struct TestCa(CertifiedIssuer<'static, KeyPair>);

impl TestCa {
    fn new() -> TestCa {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params
            .distinguished_name
            .push(DnType::CommonName, "hostkeeper test ca");
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        TestCa(CertifiedIssuer::self_signed(params, new_key()).expect("the CA"))
    }

    fn pem(&self) -> String {
        self.0.pem()
    }

    /// A host certificate for `key`, named `name`: two names give two certificates.
    fn issue(&self, key: &KeyPair, name: &str) -> String {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.distinguished_name.push(DnType::CommonName, name);
        params
            .signed_by(key, &*self.0)
            .expect("a host certificate")
            .pem()
    }
}

/// The pair the source gives for `cert` and `key`: the certificate as written, the key as
/// PKCS#8 PEM.
fn expected(cert: &str, key: &KeyPair) -> ClientIdentity {
    let signing = SigningKey::from_pkcs8_der(&key.serialize_der()).expect("a P-256 key");
    let pem = signing.to_pkcs8_pem(LineEnding::LF).expect("PEM");
    assert!(
        pem.starts_with("-----BEGIN PRIVATE KEY-----\n"),
        "{}",
        pem.len()
    );
    ClientIdentity::new(cert.as_bytes().to_vec(), pem.as_bytes().to_vec())
}

/// A pki directory written by enrollment's own writer, holding its store lock.
struct Pki {
    scratch: Scratch,
    store: PkiDir,
    _lock: StoreLock,
}

impl Pki {
    fn new(name: &str) -> Pki {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let scratch = Scratch::under(base, name).expect("a scratch directory");
        let store = PkiDir::new(&scratch.path().join("pki"));
        let lock = store.open().expect("the store opens");
        Pki {
            scratch,
            store,
            _lock: lock,
        }
    }

    fn root(&self) -> PathBuf {
        self.scratch.path().join("pki")
    }

    fn current(&self) -> PathBuf {
        self.root().join("current")
    }

    fn generation(&self, number: u64) -> PathBuf {
        self.root().join(format!("gen-{number}"))
    }

    /// The first generation: `cert`, issued for the pending key `key`.
    fn enroll(&self, cert: &str, key: &KeyPair, ca: &TestCa) {
        self.store
            .write_pending_key(&key.serialize_der())
            .expect("the pending key");
        self.store
            .write_generation(cert, &ca.pem(), KeyFrom::Pending)
            .expect("generation");
    }

    /// A renewal: `cert`, with the key of generation `from`.
    fn renew(&self, cert: &str, from: u64, ca: &TestCa) {
        self.store
            .write_generation(cert, &ca.pem(), KeyFrom::Generation(from))
            .expect("generation");
    }

    fn source(&self, rig: &Rig) -> Arc<PkiCertSource> {
        Arc::new(PkiCertSource::new(&self.root(), rig.world.log.clone()))
    }
}

/// The loop over the rig's fakes, with `certs` as its certificate source.
fn heartbeater_over(rig: &Rig, certs: Arc<PkiCertSource>) -> Heartbeater {
    let mut ports = rig.world.ports();
    ports.certs = certs;
    Heartbeater::new(
        LoopConfig {
            platform: Platform::LinuxX86_64,
            agent_version: version("0.9.0"),
            state_dir: rig.state().to_path_buf(),
            period_secs: DEFAULT_HEARTBEAT_SECS,
            installers: Vec::new(),
            private_repository: false,
        },
        ports,
    )
}

/// The pairs the fake link was called with, in order.
fn used(rig: &Rig) -> Vec<ClientIdentity> {
    rig.world
        .link
        .calls()
        .into_iter()
        .map(|(pair, _)| pair)
        .collect()
}

fn kept_line(fault: &str) -> String {
    format!(
        "error: the client certificate and key are not used: {fault}; the last good pair is kept"
    )
}

fn none_line(fault: &str) -> String {
    format!("error: the client certificate and key are not used: {fault}; no pair is used")
}

fn mismatch(generation: &Path) -> String {
    format!(
        "the key {} does not match the certificate {}",
        generation.join("key").display(),
        generation.join("cert.pem").display()
    )
}

/// Catches a source that reads its pair once and keeps it: a renewal (a new generation, the
/// switch of `current`) is used at the next heartbeat, and so is a certificate file replaced
/// in place in the current generation. The fake link records the pair of every call.
#[test]
fn a_replaced_certificate_file_is_used_at_the_next_heartbeat() {
    let rig = Rig::new("pki-replaced");
    let pki = Pki::new("pki-replaced");
    let ca = TestCa::new();
    let key = new_key();
    let first = ca.issue(&key, "host:a");
    pki.enroll(&first, &key, &ca);
    let mut heartbeater = heartbeater_over(&rig, pki.source(&rig));

    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let renewed = ca.issue(&key, "host:a renewed");
    pki.renew(&renewed, 1, &ca);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let replaced = ca.issue(&key, "host:a replaced");
    let tmp = pki.generation(2).join("cert-new.pem");
    std::fs::write(&tmp, &replaced).expect("write");
    std::fs::rename(&tmp, pki.generation(2).join("cert.pem")).expect("rename");
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);

    assert_eq!(
        used(&rig),
        [
            expected(&first, &key),
            expected(&renewed, &key),
            expected(&replaced, &key),
        ]
    );
    assert_eq!(rig.world.log.lines(), Vec::<String>::new());
}

/// Catches a source that uses a new generation whose key is not its certificate's, or one that
/// then gives no pair so the loop stops calling, or one that logs the refusal at every
/// heartbeat: three heartbeats over the bad generation are three calls with the last good pair
/// and one line. Another bad generation is said once more; a good one is used, and a bad one
/// after it is said again.
#[test]
fn a_key_that_does_not_match_its_certificate_keeps_the_last_good_pair() {
    let rig = Rig::new("pki-mismatch");
    let pki = Pki::new("pki-mismatch");
    let ca = TestCa::new();
    let key = new_key();
    let good = ca.issue(&key, "host:a");
    pki.enroll(&good, &key, &ca);
    let mut heartbeater = heartbeater_over(&rig, pki.source(&rig));
    heartbeater.beat_once();

    // Generation 2: a certificate for `key`, with another key.
    pki.enroll(&ca.issue(&key, "host:a second"), &new_key(), &ca);
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    // Generation 3: a certificate for another key, with the key of generation 1.
    pki.renew(&ca.issue(&new_key(), "host:other"), 1, &ca);
    heartbeater.beat_once();
    heartbeater.beat_once();
    // Generation 4: good again; generation 5: the bad pair of generation 2's kind.
    let next = ca.issue(&key, "host:a next");
    pki.renew(&next, 1, &ca);
    heartbeater.beat_once();
    pki.enroll(&ca.issue(&key, "host:a fifth"), &new_key(), &ca);
    heartbeater.beat_once();

    let good = expected(&good, &key);
    let next = expected(&next, &key);
    assert_eq!(
        used(&rig),
        [
            good.clone(),
            good.clone(),
            good.clone(),
            good.clone(),
            good.clone(),
            good,
            next.clone(),
            next,
        ]
    );
    assert_eq!(
        rig.world.log.lines(),
        [
            kept_line(&mismatch(&pki.generation(2))),
            kept_line(&mismatch(&pki.generation(3))),
            kept_line(&mismatch(&pki.generation(5))),
        ]
    );
}

/// Catches a source that gives an empty or made-up pair when there is no certificate, or a
/// loop that dials without one: a pki directory with no `current` link, and one that does not
/// exist, give "not enrolled" waiting for `<pki>/current`, and the link is never called.
#[test]
fn no_certificate_means_no_call() {
    let rig = Rig::new("pki-none");
    let pki = Pki::new("pki-none");
    let source = pki.source(&rig);
    let waiting = CertState::NotEnrolled {
        waiting_for: pki.current(),
    };
    assert_eq!(source.current(), waiting);
    let missing = pki.scratch.path().join("missing");
    assert_eq!(
        PkiCertSource::new(&missing, rig.world.log.clone()).current(),
        CertState::NotEnrolled {
            waiting_for: missing.join("current"),
        }
    );

    let mut heartbeater = heartbeater_over(&rig, source);
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::NotEnrolled);
    }
    assert!(rig.world.link.calls().is_empty());
    assert_eq!(
        rig.world.log.lines(),
        [format!(
            "not enrolled: waiting for {}",
            pki.current().display()
        )]
    );
}

/// Catches a source that keeps a pair after its files are gone: with the `current` link
/// removed it is not enrolled, and a bad generation after that has no last good pair to keep.
#[test]
fn a_removed_current_link_forgets_the_last_good_pair() {
    let rig = Rig::new("pki-removed");
    let pki = Pki::new("pki-removed");
    let ca = TestCa::new();
    let key = new_key();
    let cert = ca.issue(&key, "host:a");
    pki.enroll(&cert, &key, &ca);
    let source = pki.source(&rig);
    assert_eq!(source.current(), CertState::Enrolled(expected(&cert, &key)));

    std::fs::remove_file(pki.current()).expect("remove current");
    let waiting = CertState::NotEnrolled {
        waiting_for: pki.current(),
    };
    assert_eq!(source.current(), waiting);
    pki.enroll(&ca.issue(&key, "host:a again"), &new_key(), &ca);
    assert_eq!(source.current(), waiting);
    assert_eq!(
        rig.world.log.lines(),
        [none_line(&mismatch(&pki.generation(2)))]
    );
}

/// One way a pki directory can hold no usable pair.
struct Broken {
    what: &'static str,
    /// Lays out the directory under `root`, from a certificate and the DER of its own key.
    make: fn(root: &Path, cert: &str, key_der: &[u8]),
    /// The fault the log line names, from the root.
    fault: fn(root: &Path) -> String,
}

fn write_generation(root: &Path, cert: Option<&[u8]>, key: Option<&[u8]>) {
    let generation = root.join("gen-1");
    std::fs::create_dir_all(&generation).expect("mkdir");
    if let Some(cert) = cert {
        std::fs::write(generation.join("cert.pem"), cert).expect("write cert");
    }
    if let Some(key) = key {
        std::fs::write(generation.join("key"), key).expect("write key");
    }
    symlink("gen-1", root.join("current")).expect("link current");
}

fn file(root: &Path, name: &str) -> String {
    root.join("gen-1").join(name).display().to_string()
}

const NOT_FOUND: &str = "No such file or directory (os error 2)";

/// Every way the files under `current` can fail to be a pair, before any good pair was read.
fn broken() -> [Broken; 6] {
    [
        Broken {
            what: "current is a file, not a link",
            make: |root, _, _| std::fs::write(root.join("current"), b"gen-1").expect("write"),
            fault: |root| {
                format!(
                    "{} cannot be read: Invalid argument (os error 22)",
                    root.join("current").display()
                )
            },
        },
        Broken {
            what: "current names a generation that is not there",
            make: |root, _, _| symlink("gen-9", root.join("current")).expect("link"),
            fault: |root| {
                format!(
                    "{} cannot be read: {NOT_FOUND}",
                    root.join("gen-9").join("cert.pem").display()
                )
            },
        },
        Broken {
            what: "no key",
            make: |root, cert, _| write_generation(root, Some(cert.as_bytes()), None),
            fault: |root| format!("{} cannot be read: {NOT_FOUND}", file(root, "key")),
        },
        Broken {
            what: "a certificate that is not PEM",
            make: |root, _, key| write_generation(root, Some(b"not a certificate"), Some(key)),
            fault: |root| {
                format!(
                    "{} does not begin with a PEM certificate",
                    file(root, "cert.pem")
                )
            },
        },
        Broken {
            what: "a key that is not a P-256 PKCS#8 key",
            make: |root, cert, _| write_generation(root, Some(cert.as_bytes()), Some(b"not a key")),
            fault: |root| format!("{} is not a P-256 PKCS#8 key", file(root, "key")),
        },
        Broken {
            what: "a key in PEM, where the file holds DER",
            make: |root, cert, key| {
                let pem = SigningKey::from_pkcs8_der(key)
                    .expect("a key")
                    .to_pkcs8_pem(LineEnding::LF)
                    .expect("PEM");
                write_generation(root, Some(cert.as_bytes()), Some(pem.as_bytes()));
            },
            fault: |root| format!("{} is not a P-256 PKCS#8 key", file(root, "key")),
        },
    ]
}

/// Catches a source that uses, or turns into an empty pair, files that are not a pair, and
/// one whose log line drops the path or the cause: each broken layout gives "not enrolled" and
/// exactly one line naming the file and the fault, however often it is read. The same
/// certificate with its own key is then a pair, so each case fails only for its own fault.
#[test]
fn files_that_are_not_a_pair_are_not_used_and_said_once() {
    let ca = TestCa::new();
    let key = new_key();
    let cert = ca.issue(&key, "host:a");
    for (n, broken) in broken().iter().enumerate() {
        let rig = Rig::new("pki-broken");
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "pki-broken")
            .expect("a scratch directory");
        let root = scratch.path().to_path_buf();
        (broken.make)(&root, &cert, &key.serialize_der());
        let source = PkiCertSource::new(&root, rig.world.log.clone());
        let waiting = CertState::NotEnrolled {
            waiting_for: root.join("current"),
        };
        for _ in 0..3 {
            assert_eq!(source.current(), waiting, "case {n}: {}", broken.what);
        }
        assert_eq!(
            rig.world.log.lines(),
            [none_line(&(broken.fault)(&root))],
            "case {n}: {}",
            broken.what
        );

        let _ = std::fs::remove_file(root.join("current"));
        let _ = std::fs::remove_dir_all(root.join("gen-1"));
        write_generation(&root, Some(cert.as_bytes()), Some(&key.serialize_der()));
        assert_eq!(
            source.current(),
            CertState::Enrolled(expected(&cert, &key)),
            "case {n}: {}",
            broken.what
        );
    }
}
