//! `ssh-ca init` and `ssh-ca sign`: the key file the first writes and the certificate the second
//! prints, by the files they leave and the bytes they print.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::Ordering;

use super::{Init, KEY_FILE, OFFLINE_SERIALS, Sign, SshCaCommand, SshCaError, run};
use crate::enrollment::{Random, os_random};
use crate::ssh_cert::tests::{ED25519_KEY, read_cert};
use crate::ssh_cert::{CaKey, KeyError};
use crate::testutil::scratch;

const NOW: u64 = 1_800_000_000;

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("a file").permissions().mode() & 0o777
}

fn init(out: &Path) -> Result<String, SshCaError> {
    run(
        &SshCaCommand::Init(Init {
            out: out.to_owned(),
        }),
        NOW,
        &os_random(),
    )
    .map(|report| report.to_string())
}

fn never() -> Random {
    Box::new(|_| false)
}

/// A random source that serves `n` draws and then none.
fn draws(n: usize) -> Random {
    let left = std::sync::atomic::AtomicUsize::new(n);
    Box::new(move |buffer| {
        let ok = left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |l| l.checked_sub(1))
            .is_ok();
        buffer.fill(9);
        ok
    })
}

/// Proves: `init` makes the directory (mode 0700) and the key file in it (mode 0600), and prints
/// the public key line and its fingerprint, which are the key file's: reading the file back gives
/// the same public key; what it prints holds none of the private key file's text.
///
/// Catches: a private key that is readable by others, and one that is printed.
#[test]
fn init_writes_the_private_key_to_a_file_and_prints_only_the_public_key() {
    let dir = scratch("ssh-ca-init");
    let out = dir.path().join("ca");
    let report = init(&out).expect("made");
    let file = out.join(KEY_FILE);
    assert_eq!((mode(&out), mode(&file)), (0o700, 0o600));
    let text = fs::read_to_string(&file).expect("the key file");
    let keys = CaKey::parse_secret(&text).expect("a key");
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0],
        format!("wrote the private key {}", file.display())
    );
    assert_eq!(
        lines[1],
        format!(
            "public key: {} hostkeeper-ssh-user-ca",
            keys[0].public_line()
        )
    );
    assert_eq!(lines[2], format!("fingerprint: {}", keys[0].fingerprint()));
    let secret_body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
    assert!(!report.contains(&secret_body[..40]));
}

/// Proves: `init` never replaces a key that is there: a second run into the same directory
/// is refused naming the file, and the first key is as it was; a directory that cannot be made
/// is refused naming it; and without random bytes no key file is made.
///
/// Catches: a re-run that replaces the CA key, so every host trusts a key that no longer signs.
#[test]
fn init_never_replaces_a_key_and_names_what_it_could_not_write() {
    let dir = scratch("ssh-ca-init-twice");
    let out = dir.path().join("ca");
    init(&out).expect("made");
    let before = fs::read(out.join(KEY_FILE)).expect("the key file");
    let again = init(&out).expect_err("refused");
    assert!(
        matches!(&again, SshCaError::Write { path, .. } if *path == out.join(KEY_FILE)),
        "{again}"
    );
    assert_eq!(fs::read(out.join(KEY_FILE)).expect("the key file"), before);
    let file = dir.path().join("a-file");
    fs::write(&file, "x").expect("a file");
    let blocked = init(&file.join("ca")).expect_err("refused");
    assert!(
        matches!(&blocked, SshCaError::Write { path, .. } if *path == file.join("ca")),
        "{blocked}"
    );
    let silent = dir.path().join("silent");
    let refused = run(
        &SshCaCommand::Init(Init {
            out: silent.clone(),
        }),
        NOW,
        &never(),
    );
    assert!(matches!(refused, Err(SshCaError::Random)));
    assert!(!silent.join(KEY_FILE).exists());
}

/// Makes a CA key file and an operator's public key file, and the `sign` flags that use them.
fn signing(dir: &Path, host: &str, operator: &str) -> (Sign, CaKey) {
    let out = dir.join("ca");
    init(&out).expect("made");
    let public = dir.join("alice.pub");
    fs::write(&public, format!("{ED25519_KEY}\n")).expect("a public key");
    let key = CaKey::parse_secret(&fs::read_to_string(out.join(KEY_FILE)).expect("a file"))
        .expect("a key")
        .remove(0);
    (
        Sign {
            ca_key_file: out.join(KEY_FILE),
            public_key_file: public,
            host_id: host.to_owned(),
            operator: operator.to_owned(),
            ttl_s: 600,
        },
        key,
    )
}

/// Proves: `sign` prints one line, the certificate: the one principal `hk-host-<host id>`, the key
/// id `<operator>/<serial>` with a serial in the high half of the numbers (where the coordinator's
/// sequence does not come), valid from a minute before now to the TTL after it, signed by the key
/// of the file (verified with its public key); and nothing else.
///
/// Catches: an offline certificate that the coordinator's serials could collide with, and one
/// that is signed by another key than the file's.
#[test]
fn sign_prints_the_certificate_the_ca_file_signs_for_the_one_host() {
    let dir = scratch("ssh-ca-sign");
    let (flags, key) = signing(dir.path(), "nuc", "carol");
    let report = run(&SshCaCommand::Sign(flags), NOW, &os_random())
        .expect("signed")
        .to_string();
    assert_eq!(report.lines().count(), 1);
    let cert = read_cert(report.trim_end(), 1);
    assert_eq!(cert.principals, ["hk-host-nuc"]);
    assert!(cert.serial >= OFFLINE_SERIALS);
    assert_eq!(cert.key_id, format!("carol/{}", cert.serial));
    assert_eq!((cert.valid_after, cert.valid_before), (NOW - 60, NOW + 601));
    assert_eq!(cert.ca.len(), key.blob().len());
    assert_eq!(cert.ca, key.blob());
    use ring::signature::KeyPair as _;
    use ring::signature::{ED25519, UnparsedPublicKey};
    UnparsedPublicKey::new(&ED25519, key.pair.public_key().as_ref())
        .verify(&cert.signed, &cert.raw_signature)
        .expect("signed by the file's key");
}

/// Proves: `sign` refuses, naming the flag, a CA key file that others can read, one that is not a
/// key, a public key file that is missing, too long or not a key the CA signs, a host id or an
/// operator that is not one, and a system with no random bytes at either of the two places it
/// draws them; and prints nothing then.
///
/// Catches: a CA key read from a file anyone can read, and a certificate for a name the
/// coordinator's own rules would refuse.
#[test]
fn sign_refuses_what_it_should_not_sign_with_the_reason() {
    let dir = scratch("ssh-ca-sign-refused");
    let (good, _) = signing(dir.path(), "nuc", "carol");
    let sign = |flags: &Sign, random: &Random| run(&SshCaCommand::Sign(flags.clone()), NOW, random);
    let mut open = good.clone();
    fs::set_permissions(&good.ca_key_file, fs::Permissions::from_mode(0o644)).expect("mode");
    assert!(matches!(
        sign(&open, &os_random()),
        Err(SshCaError::Secret(_))
    ));
    fs::set_permissions(&good.ca_key_file, fs::Permissions::from_mode(0o600)).expect("mode");
    let junk = dir.path().join("junk");
    fs::write(&junk, "not a key").expect("a file");
    fs::set_permissions(&junk, fs::Permissions::from_mode(0o600)).expect("mode");
    open.ca_key_file = junk;
    assert!(matches!(
        sign(&open, &os_random()),
        Err(SshCaError::CaKey(KeyError::BadPrivateKey))
    ));
    let mut bad = good.clone();
    bad.host_id = "Node_7".to_owned();
    assert!(matches!(sign(&bad, &os_random()), Err(SshCaError::HostId)));
    let mut bad = good.clone();
    bad.operator = "Carol".to_owned();
    assert!(matches!(
        sign(&bad, &os_random()),
        Err(SshCaError::Operator)
    ));
    let mut bad = good.clone();
    bad.public_key_file = dir.path().join("missing.pub");
    let error = sign(&bad, &os_random()).expect_err("missing");
    assert!(
        matches!(&error, SshCaError::Read { path, .. } if *path == bad.public_key_file),
        "{error}"
    );
    let mut bad = good.clone();
    bad.public_key_file = dir.path().join("long.pub");
    fs::write(&bad.public_key_file, "#".repeat(8 * 1024 + 1)).expect("a file");
    assert!(matches!(
        sign(&bad, &os_random()),
        Err(SshCaError::Read { .. })
    ));
    fs::write(&bad.public_key_file, "ssh-ed25519 AAAA\n").expect("a file");
    assert!(matches!(
        sign(&bad, &os_random()),
        Err(SshCaError::PublicKey(KeyError::BadPublicKey))
    ));
    assert!(matches!(sign(&good, &never()), Err(SshCaError::Random)));
    assert!(matches!(sign(&good, &draws(1)), Err(SshCaError::Random)));
    assert!(sign(&good, &draws(2)).is_ok());
}
