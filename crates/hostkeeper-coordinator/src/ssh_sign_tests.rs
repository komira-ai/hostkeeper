//! `Ssh.SignUserCert` and `Ssh.Revoke` as the coordinator answers them: who is refused and how,
//! what the certificate says, and that the ledger holds it before it exists. The ledger is the
//! memory one; the Postgres one is tested beside the store.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::SshRefusal as Refusal;
use ring::signature::{ED25519, UnparsedPublicKey};

use super::{Signing, principal, reason_ok};
use crate::enrollment::Random;
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::host_groups_support::{coordinator_on, enroll_host};
use crate::marks_support::operator;
use crate::operator::Caller;
use crate::service::Coordinator;
use crate::ssh_cert::tests::{ED25519_KEY, counting, read_cert};
use crate::ssh_cert::{CaKey, UserKey};
use crate::ssh_ledger::{Issue, Issued, Ledger, LedgerError, Memory, Revocation};
use crate::store::Fake;
use crate::testutil::Rig;

const NOW: u64 = 1_800_000_000;
const OPERATORS: &str = r#"
[[operator]]
name = "operator"
classes = ["fleet"]

[[operator]]
name = "bob"
hosts = ["node-08"]

[[operator]]
name = "carol"
classes = ["control-plane"]
"#;

/// The ledger of a test: the memory ledger, and the order in which it and the signing were
/// asked.
struct Watched {
    inner: Arc<Memory>,
    steps: Arc<Mutex<Vec<&'static str>>>,
}

impl Ledger for Watched {
    fn issue(&self, issue: &Issue<'_>) -> Result<Issued, LedgerError> {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("issue");
        self.inner.issue(issue)
    }

    fn revoke(&self, by: &str, target: Revocation<'_>) -> Result<u64, LedgerError> {
        self.inner.revoke(by, target)
    }

    fn denied(&self, by: &str, host_id: &str, why: &str) -> Result<(), LedgerError> {
        self.inner.denied(by, host_id, why)
    }
}

/// A coordinator that signs, with three enrolled hosts (`node-07`, `node-08` and `nuc`), and what
/// a test looks at.
pub(crate) struct World {
    pub(crate) coordinator: Coordinator,
    pub(crate) ledger: Arc<Memory>,
    steps: Arc<Mutex<Vec<&'static str>>>,
    records: Arc<MemoryRecords>,
    /// The keys of the fleet secret, as they were made, and the control-plane key's public half.
    fleet: Vec<String>,
    control_plane: String,
    operators: std::path::PathBuf,
    _enrollment: EnrollmentRig,
    _rig: Rig,
}

/// What a test changes in the signing before it starts.
#[derive(Default)]
pub(crate) struct Tune {
    pub(crate) signing: usize,
    pub(crate) no_control_plane_key: bool,
    pub(crate) random_fails: bool,
}

pub(crate) fn world(name: &str, file: Option<&str>, tune: Tune) -> World {
    let records = Arc::new(MemoryRecords::new());
    let enrollment = EnrollmentRig::over_records(&format!("{name}-enrollment"), records.clone());
    let rig = Rig::new(name);
    let coordinator = coordinator_on(
        &Fake::new(),
        &rig.clock,
        Some(Arc::clone(&enrollment.enrollment)),
    );
    for host in ["node-07", "node-08", "nuc"] {
        enroll_host(&enrollment.enrollment, host);
    }
    let operators = rig.dir.path().join("operators.toml");
    if let Some(text) = file {
        std::fs::write(&operators, text).expect("the operators file");
    }
    let make = |start: u8| CaKey::generate("t", &counting(start)).expect("a key").0;
    let fleet = vec![make(1), make(40)];
    let cp = make(80);
    let (fleet_public, cp_public) = (
        fleet.iter().map(CaKey::public_line).collect(),
        cp.public_line(),
    );
    let ledger = Arc::new(Memory::at(NOW));
    let steps = Arc::new(Mutex::new(Vec::new()));
    let watched = Arc::clone(&steps);
    let fails = tune.random_fails;
    let random: Random = Box::new(move |buffer| {
        watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("sign");
        buffer.fill(7);
        !fails
    });
    let signing = Signing {
        fleet,
        signing: tune.signing,
        control_plane: (!tune.no_control_plane_key).then_some(cp),
        control_plane_hosts: BTreeSet::from(["nuc".to_owned()]),
        operators: operators.clone(),
        ttl_secs: 3600,
        login_linux: "root".to_owned(),
        login_macos: "admin".to_owned(),
        ledger: Arc::new(Watched {
            inner: Arc::clone(&ledger),
            steps: Arc::clone(&steps),
        }),
        random,
    };
    World {
        coordinator: coordinator.with_ssh(signing),
        ledger,
        steps,
        records,
        fleet: fleet_public,
        control_plane: cp_public,
        operators,
        _enrollment: enrollment,
        _rig: rig,
    }
}

fn request(host: &str, key: &str, reason: &str) -> proto::SignUserCertRequest {
    proto::SignUserCertRequest {
        public_key: key.to_owned(),
        reason: reason.to_owned(),
        host_id: host.to_owned(),
    }
}

fn named(name: &str) -> Caller {
    Caller::Operator(name.parse().expect("a token name"))
}

/// The line of a ledger line's token, as the refusal wrote it.
fn denied(world: &World) -> Vec<(String, Option<String>, String)> {
    world
        .ledger
        .audit()
        .into_iter()
        .filter(|row| row.line.action == "ssh_denied")
        .map(|row| (row.line.by, row.line.subject, row.line.detail))
        .collect()
}

/// Proves: the operator the file lists for the fleet gets a certificate for a fleet host whose
/// every field is the design's: its principal `hk-host-<host id>` and no other, the key id
/// `<operator>/<serial>` with the serial the ledger gave, valid from a minute before the
/// ledger's time to one hour after it, `permit-pty` the only extension, signed by the fleet's
/// current key (and verified with its public key); the response names the key id, the last valid
/// second and the Linux login user; and the ledger holds the certificate, with the key's
/// fingerprint and the reason.
///
/// Catches: a principal or validity taken from the request, a certificate signed by the wrong key
/// of the rotation, and a response that disagrees with the certificate.
#[test]
fn the_operator_the_file_lists_gets_a_certificate_for_one_host_that_the_ca_signed() {
    let world = world("ssh-sign-ok", Some(OPERATORS), Tune::default());
    let answer = world
        .coordinator
        .ssh_sign_user_cert(
            &operator(),
            &request("node-07", ED25519_KEY, "look at a log"),
        )
        .expect("signed");
    assert_eq!(answer.key_id, "operator/1");
    assert_eq!(answer.login_user, "root");
    assert_eq!(answer.expires_unix, NOW + 3600);
    let cert = read_cert(&answer.certificate, 1);
    assert_eq!(cert.serial, 1);
    assert_eq!(cert.key_id, "operator/1");
    assert_eq!(cert.principals, ["hk-host-node-07"]);
    assert_eq!(
        (cert.valid_after, cert.valid_before),
        (NOW - 60, NOW + 3601)
    );
    assert_eq!(cert.extensions, [("permit-pty".to_owned(), Vec::new())]);
    assert!(cert.critical.is_empty());
    let ca = &world.fleet[0];
    let blob = ca.split(' ').nth(1).expect("a blob");
    use base64::Engine as _;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .expect("base64");
    assert_eq!(cert.ca, blob);
    UnparsedPublicKey::new(&ED25519, &blob[blob.len() - 32..])
        .verify(&cert.signed, &cert.raw_signature)
        .expect("signed by the current key");
    let row = &world.ledger.certs()[&1];
    assert_eq!(
        (row.operator.as_str(), row.host_id.as_str()),
        ("operator", "node-07")
    );
    assert_eq!(
        row.fingerprint,
        UserKey::parse(ED25519_KEY).expect("a key").fingerprint
    );
    assert_eq!(row.reason, "look at a log");
    assert_eq!(principal("node-07"), "hk-host-node-07");
}

/// Proves: the ledger is asked before the key signs: the order of the two is the ledger's write,
/// then the signing, so the certificate cannot exist without its row and its line.
///
/// Catches: a signing that runs before the audit write, which a failing write would leave
/// unaudited.
#[test]
fn the_ledger_writes_before_the_key_signs() {
    let world = world("ssh-sign-order", Some(OPERATORS), Tune::default());
    world
        .coordinator
        .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""))
        .expect("signed");
    let steps = world
        .steps
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(steps, ["issue", "sign"]);
}

/// Proves: when the ledger cannot write, the call is `audit_unavailable`, no certificate is
/// made (the key is not asked to sign) and nothing is recorded; and once the ledger writes again
/// the same request is signed.
///
/// Catches: an unaudited certificate.
#[test]
fn no_certificate_is_made_when_the_ledger_cannot_write() {
    let world = world("ssh-sign-ledger-down", Some(OPERATORS), Tune::default());
    world.ledger.refuse_next(1);
    let refused = world
        .coordinator
        .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""));
    assert_eq!(refused.err(), Some(Refusal::AuditUnavailable));
    assert!(world.ledger.certs().is_empty());
    assert_eq!(
        *world.steps.lock().unwrap_or_else(PoisonError::into_inner),
        ["issue"]
    );
    assert!(
        world
            .coordinator
            .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""))
            .is_ok()
    );
}

/// Proves: the signing key that is not the first of the secret signs when `signing` says so (a
/// rotation's `next`), and the control-plane host's certificate is signed by the control-plane
/// key and not by the fleet's; a control-plane host with no key for its class is refused
/// `not_configured`, though the operator is listed for it.
///
/// Catches: a fleet key that signs for the control plane, so a stolen coordinator is root there.
#[test]
fn the_key_that_signs_is_the_rotations_and_the_hosts_class() {
    let next = world(
        "ssh-sign-next",
        Some(OPERATORS),
        Tune {
            signing: 1,
            ..Tune::default()
        },
    );
    let answer = next
        .coordinator
        .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""))
        .expect("signed");
    let ca_of = |line: &str| {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(line.split(' ').nth(1).expect("a blob"))
            .expect("base64")
    };
    assert_eq!(read_cert(&answer.certificate, 1).ca, ca_of(&next.fleet[1]));
    let plane = world("ssh-sign-plane", Some(OPERATORS), Tune::default());
    let answer = plane
        .coordinator
        .ssh_sign_user_cert(&named("carol"), &request("nuc", ED25519_KEY, ""))
        .expect("signed");
    let cert = read_cert(&answer.certificate, 1);
    assert_eq!(cert.ca, ca_of(&plane.control_plane));
    assert_eq!(cert.principals, ["hk-host-nuc"]);
    let bare = world(
        "ssh-sign-plane-bare",
        Some(OPERATORS),
        Tune {
            no_control_plane_key: true,
            ..Tune::default()
        },
    );
    let refused = bare
        .coordinator
        .ssh_sign_user_cert(&named("carol"), &request("nuc", ED25519_KEY, ""));
    assert_eq!(refused.err(), Some(Refusal::NotConfigured));
    assert!(bare.ledger.certs().is_empty());
}

/// Proves: default deny, each refusal after the caller is known an audit line `ssh_denied` with the
/// caller, the host and the token: an operator the file does not list, an operator listed for
/// other hosts only (`bob` for `node-08` asking for `node-07`), a fleet operator asking for the
/// control-plane host, and a missing or unreadable operators file; and nothing is signed.
///
/// Catches: a default that allows, a class that reaches the other class, and a deny that leaves
/// no trace.
#[test]
fn an_operator_is_refused_what_the_file_does_not_list_and_the_refusal_is_a_line() {
    let world = world("ssh-sign-deny", Some(OPERATORS), Tune::default());
    let ask = |who: &str, host: &str| {
        world
            .coordinator
            .ssh_sign_user_cert(&named(who), &request(host, ED25519_KEY, ""))
            .err()
    };
    assert_eq!(ask("dave", "node-07"), Some(Refusal::NotPermitted));
    assert_eq!(ask("bob", "node-07"), Some(Refusal::NotPermitted));
    assert_eq!(ask("operator", "nuc"), Some(Refusal::NotPermitted));
    assert_eq!(ask("bob", "node-08"), None);
    let line = |who: &str, host: &str| {
        (
            who.to_owned(),
            Some(host.to_owned()),
            "{\"why\":\"not_permitted\"}".to_owned(),
        )
    };
    assert_eq!(
        denied(&world),
        [
            line("dave", "node-07"),
            line("bob", "node-07"),
            line("operator", "nuc")
        ]
    );
    assert_eq!(world.ledger.certs().len(), 1);
    // A refusal stands when its line cannot be written, and writes nothing.
    let lines = world.ledger.audit().len();
    world.ledger.refuse_next(1);
    assert_eq!(ask("dave", "node-07"), Some(Refusal::NotPermitted));
    assert_eq!(world.ledger.audit().len(), lines);
    std::fs::write(&world.operators, "not [ toml").expect("a broken file");
    assert_eq!(ask("operator", "node-07"), Some(Refusal::NotPermitted));
    std::fs::remove_file(&world.operators).expect("removed");
    assert_eq!(ask("operator", "node-07"), Some(Refusal::NotPermitted));
    let none = self::world("ssh-sign-nofile", None, Tune::default());
    let refused = none
        .coordinator
        .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""));
    assert_eq!(refused.err(), Some(Refusal::NotPermitted));
}

/// Proves: the file is read again at every call: an operator removed from it is refused at once,
/// with no restart.
///
/// Catches: a removal that waits for a restart while the operator keeps signing.
#[test]
fn removing_an_operator_from_the_file_takes_effect_at_the_next_call() {
    let world = world("ssh-sign-reload", Some(OPERATORS), Tune::default());
    let ask = || {
        world
            .coordinator
            .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""))
            .err()
    };
    assert_eq!(ask(), None);
    std::fs::write(
        &world.operators,
        "[[operator]]\nname = \"bob\"\nhosts = [\"node-08\"]\n",
    )
    .expect("rewritten");
    assert_eq!(ask(), Some(Refusal::NotPermitted));
}

/// Proves: the request's own faults are refused with the token for each and written as lines
/// when the caller is known: a host id that is not one is `invalid_request` (no line: it names
/// no host), a host that is not enrolled `unknown_host`, a reason over 200 characters or with a
/// control or non-ASCII character `bad_reason`, a key the CA does not sign `bad_key`; an empty
/// reason is fine.
///
/// Catches: a reason that reaches the audit with a control character, and a key that is not
/// checked before the ledger is written.
#[test]
fn a_request_with_a_fault_is_refused_by_its_token_before_anything_is_written() {
    let world = world("ssh-sign-faults", Some(OPERATORS), Tune::default());
    let ask = |host: &str, key: &str, reason: &str| {
        world
            .coordinator
            .ssh_sign_user_cert(&operator(), &request(host, key, reason))
            .err()
    };
    assert_eq!(
        ask("Node_7", ED25519_KEY, ""),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(ask("", ED25519_KEY, ""), Some(Refusal::InvalidRequest));
    assert_eq!(ask("node-99", ED25519_KEY, ""), Some(Refusal::UnknownHost));
    assert_eq!(
        ask("node-07", ED25519_KEY, &"x".repeat(201)),
        Some(Refusal::BadReason)
    );
    assert_eq!(
        ask("node-07", ED25519_KEY, "a\nb"),
        Some(Refusal::BadReason)
    );
    assert_eq!(
        ask("node-07", ED25519_KEY, "caf\u{e9}"),
        Some(Refusal::BadReason)
    );
    assert_eq!(
        ask("node-07", "ssh-ed25519 AAAA", ""),
        Some(Refusal::BadKey)
    );
    assert_eq!(ask("node-07", ED25519_KEY, &"x".repeat(200)), None);
    let tokens: Vec<String> = denied(&world)
        .into_iter()
        .map(|(_, _, detail)| detail)
        .collect();
    assert_eq!(
        tokens,
        [
            "{\"why\":\"unknown_host\"}",
            "{\"why\":\"bad_reason\"}",
            "{\"why\":\"bad_reason\"}",
            "{\"why\":\"bad_reason\"}",
            "{\"why\":\"bad_key\"}",
        ]
    );
    assert!(reason_ok("") && reason_ok(" ~") && !reason_ok("\u{7f}") && !reason_ok("\t"));
    assert_eq!(world.ledger.certs().len(), 1);
}

/// Proves: a caller that is not an operator is refused before anything else: no credential is
/// `unauthorized`, CI's is `forbidden`; and a coordinator that holds no CA answers
/// `not_configured` to an operator, whatever the request says.
///
/// Catches: a path a CI token or a stranger could reach the signing by.
#[test]
fn only_an_operator_is_heard_and_a_coordinator_with_no_ca_is_not_configured() {
    let world = world("ssh-sign-callers", Some(OPERATORS), Tune::default());
    let good = request("node-07", ED25519_KEY, "");
    assert_eq!(
        world
            .coordinator
            .ssh_sign_user_cert(&Caller::Anonymous, &good)
            .err(),
        Some(Refusal::Unauthorized)
    );
    assert_eq!(
        world
            .coordinator
            .ssh_sign_user_cert(&Caller::Ci, &good)
            .err(),
        Some(Refusal::Forbidden)
    );
    let revoke = proto::RevokeRequest {
        operator: "operator".to_owned(),
        serial: 0,
    };
    assert_eq!(
        world
            .coordinator
            .ssh_revoke(&Caller::Anonymous, &revoke)
            .err(),
        Some(Refusal::Unauthorized)
    );
    assert_eq!(
        world.coordinator.ssh_revoke(&Caller::Ci, &revoke).err(),
        Some(Refusal::Forbidden)
    );
    let bare = coordinator_on(&Fake::new(), &Rig::new("ssh-sign-bare").clock, None);
    assert_eq!(
        bare.ssh_sign_user_cert(&operator(), &proto::SignUserCertRequest::default())
            .err(),
        Some(Refusal::NotConfigured)
    );
    assert_eq!(
        bare.ssh_revoke(&operator(), &revoke).err(),
        Some(Refusal::NotConfigured)
    );
}

/// Proves: a Mac's certificate names the Macs' login account and a Linux host's the Linux one; a
/// host record that cannot be read is `store_failed`; and a key that cannot sign (no random bytes
/// for the nonce) is `store_failed` after the ledger wrote, so the audit says signed where no
/// certificate went out, the safe side of the two.
///
/// Catches: a Mac sent the Linux account, and a store fault that answers as a refusal of the
/// request.
#[test]
fn the_login_user_follows_the_host_and_faults_of_the_store_and_the_key_are_store_failed() {
    let world = world("ssh-sign-faults2", Some(OPERATORS), Tune::default());
    let mut host = world
        .coordinator
        .enrolled_host("node-07")
        .expect("read")
        .expect("enrolled");
    let ssh = world.coordinator.ssh.as_ref().expect("signing");
    assert_eq!(ssh.login_for(&host), "root");
    host.claims.mac_serial = "C02ABC".to_owned();
    assert_eq!(ssh.login_for(&host), "admin");
    world.records.fail_at(world.records.count() + 1);
    let refused = world
        .coordinator
        .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""));
    assert_eq!(refused.err(), Some(Refusal::StoreFailed));
    assert!(world.ledger.certs().is_empty());
    let silent = self::world(
        "ssh-sign-norandom",
        Some(OPERATORS),
        Tune {
            random_fails: true,
            ..Tune::default()
        },
    );
    let refused = silent
        .coordinator
        .ssh_sign_user_cert(&operator(), &request("node-07", ED25519_KEY, ""));
    assert_eq!(refused.err(), Some(Refusal::StoreFailed));
    assert_eq!(silent.ledger.certs().len(), 1);
}

/// Proves: `Revoke` by an operator the file lists revokes by operator name or by serial and says
/// how many were newly revoked; asking again says 0; a name or serial no certificate was made for
/// is `unknown_cert`; both fields, neither, a name that is not a token name or a serial of 0 with
/// no name are `invalid_request`; an operator the file does not list may not revoke
/// (`not_permitted`, with its line); and a ledger that cannot write is `audit_unavailable`.
///
/// Catches: a revocation by someone no file names, and a request that revokes everything because
/// both fields were empty.
#[test]
fn revoking_takes_one_operator_or_one_serial_and_only_from_a_listed_operator() {
    let world = world("ssh-sign-revoke", Some(OPERATORS), Tune::default());
    for host in ["node-07", "node-08"] {
        world
            .coordinator
            .ssh_sign_user_cert(&operator(), &request(host, ED25519_KEY, ""))
            .expect("signed");
    }
    let revoke = |by: &str, name: &str, serial: u64| {
        world.coordinator.ssh_revoke(
            &named(by),
            &proto::RevokeRequest {
                operator: name.to_owned(),
                serial,
            },
        )
    };
    assert_eq!(revoke("bob", "", 1).map(|r| r.revoked), Ok(1));
    assert_eq!(revoke("bob", "operator", 0).map(|r| r.revoked), Ok(1));
    assert_eq!(revoke("bob", "operator", 0).map(|r| r.revoked), Ok(0));
    assert_eq!(revoke("bob", "", 9).err(), Some(Refusal::UnknownCert));
    assert_eq!(revoke("bob", "nobody", 0).err(), Some(Refusal::UnknownCert));
    for (name, serial) in [("operator", 1), ("", 0), ("Operator", 0)] {
        assert_eq!(
            revoke("bob", name, serial).err(),
            Some(Refusal::InvalidRequest),
            "{name} {serial}"
        );
    }
    assert_eq!(
        revoke("dave", "operator", 0).err(),
        Some(Refusal::NotPermitted)
    );
    assert!(
        denied(&world)
            .iter()
            .any(|(by, _, why)| by == "dave" && why.contains("not_permitted"))
    );
    world.ledger.refuse_next(1);
    assert_eq!(
        revoke("bob", "operator", 0).err(),
        Some(Refusal::AuditUnavailable)
    );
}
