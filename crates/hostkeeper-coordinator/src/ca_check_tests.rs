//! `check-ca enrollment` and `check-ca ssh`: what each passes and refuses, by the report and the
//! refusal it gives, and that neither holds a byte of a key.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::ca::{Ca, CaFiles, Signer as _};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::validity::SystemClock;
use hostkeeper_testkit::Scratch;

use super::{CheckCaCommand, CheckCaError, Enrollment, MAX_SECRET_BYTES, Ssh, run};
use crate::enrollment::os_random;
use crate::ssh_cert::CaKey;
use crate::testutil::scratch;

/// An enrollment CA as the coordinator makes it: `ca.key` and `ca.key.cert` in `dir`.
struct EnrollmentCa {
    key: PathBuf,
    certificate: PathBuf,
    sha256: String,
}

fn enrollment_ca(dir: &Path) -> EnrollmentCa {
    fs::create_dir_all(dir).expect("a directory");
    let key = dir.join("ca.key");
    let certificate = dir.join("ca.key.cert");
    let files = CaFiles {
        key: &key,
        certificate: &certificate,
    };
    let ca = Ca::init(&RealFs, files, Arc::new(SystemClock)).expect("a CA");
    let sha256 = ca.bundle().sha256_hex();
    EnrollmentCa {
        key,
        certificate,
        sha256,
    }
}

fn check_enrollment(key: &Path, certificate: &Path, pin: &str) -> Result<String, CheckCaError> {
    run(&CheckCaCommand::Enrollment(Enrollment {
        key_file: key.to_owned(),
        certificate_file: certificate.to_owned(),
        ca_sha256: pin.to_owned(),
    }))
    .map(|report| report.to_string())
}

fn check_ssh(key: &Path, public: &Path) -> Result<String, CheckCaError> {
    run(&CheckCaCommand::Ssh(Ssh {
        key_file: key.to_owned(),
        public_file: public.to_owned(),
    }))
    .map(|report| report.to_string())
}

/// `bytes` at `path`, mode `mode`.
fn put(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).expect("a file");
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("a mode");
}

/// A new SSH CA key and its private key file's text.
fn ssh_key() -> (CaKey, String) {
    CaKey::generate("hostkeeper-ssh-user-ca", &*os_random()).expect("a key")
}

/// The base64 lines of an OpenSSH private key file: the key's bytes as they would print.
fn body_lines(pem: &str) -> Vec<&str> {
    pem.lines()
        .filter(|line| !line.starts_with("-----"))
        .collect()
}

/// Proves: a key and the certificate made with it, checked against the SHA-256 the daemons pin
/// (in either case of hex), pass with a report of the paths and that SHA-256 alone; no byte of
/// the key file, as hex or as its bytes, is in the report.
/// Catches: a check that passes without comparing the pin, or that prints the key.
#[test]
fn a_key_and_its_certificate_pass_with_the_pinned_sha256() {
    let dir = scratch("check-ca-enrollment-pass");
    let ca = enrollment_ca(dir.path());
    for pin in [ca.sha256.clone(), ca.sha256.to_ascii_uppercase()] {
        let report = check_enrollment(&ca.key, &ca.certificate, &pin).expect("the pinned CA");
        assert_eq!(
            report,
            format!(
                "the key {} is the key of the certificate {}\nca_sha256: {}\n\
                 the certificate is the one --ca-sha256 pins\n",
                ca.key.display(),
                ca.certificate.display(),
                ca.sha256
            )
        );
        let key = fs::read(&ca.key).expect("the key");
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert!(!report.contains(&hex[hex.len() - 64..]), "{report}");
        assert!(
            !report
                .as_bytes()
                .windows(32)
                .any(|w| key.windows(32).any(|k| k == w))
        );
    }
}

/// Proves: the key of one CA with the certificate of another is refused, naming the
/// certificate, whatever pin is given.
/// Catches: a check that reads the certificate's SHA-256 alone and never ties it to the key.
#[test]
fn a_key_from_another_ca_is_refused() {
    let dir = scratch("check-ca-enrollment-other");
    let one = enrollment_ca(&dir.path().join("one"));
    let other = enrollment_ca(&dir.path().join("other"));
    for pin in [&one.sha256, &other.sha256] {
        let refused = check_enrollment(&other.key, &one.certificate, pin).expect_err("two CAs");
        assert_eq!(
            refused.to_string(),
            format!(
                "the CA certificate {} does not carry the CA key's public key",
                one.certificate.display()
            )
        );
    }
}

/// Proves: a pair that is one CA but not the CA the daemons pin is refused, naming both
/// SHA-256s; a pin that is not 64 hex digits is refused before a file is read.
/// Catches: a restored copy of some other farm's CA passing as this fleet's.
#[test]
fn a_ca_the_daemons_do_not_pin_is_refused() {
    let dir = scratch("check-ca-enrollment-pin");
    let one = enrollment_ca(&dir.path().join("one"));
    let other = enrollment_ca(&dir.path().join("other"));
    let refused =
        check_enrollment(&one.key, &one.certificate, &other.sha256).expect_err("another pin");
    assert_eq!(
        refused.to_string(),
        format!(
            "the certificate's SHA-256 is {}, not the --ca-sha256 the daemons pin, {}",
            one.sha256, other.sha256
        )
    );
    let missing = dir.path().join("missing");
    for pin in [
        &one.sha256[1..],
        &format!("{}0", one.sha256),
        &"g".repeat(64),
        "",
    ] {
        let refused = check_enrollment(&missing, &missing, pin).expect_err("not a pin");
        assert_eq!(
            refused.to_string(),
            "--ca-sha256: not a SHA-256 of 64 hex digits"
        );
    }
}

/// Proves: a key file or a certificate file cut short (as a copy that did not finish), an
/// empty one, and a missing one are refused, naming the file and never a byte of the key.
/// Catches: a check that passes a partial copy, or reports the key's bytes in a parse error.
#[test]
fn a_truncated_empty_or_missing_file_is_refused() {
    let dir = scratch("check-ca-enrollment-truncated");
    let ca = enrollment_ca(&dir.path().join("ca"));
    let key = fs::read(&ca.key).expect("the key");
    let certificate = fs::read(&ca.certificate).expect("the certificate");
    let short_key = dir.path().join("short.key");
    let short_certificate = dir.path().join("short.cert");
    for cut in [0, key.len() / 2, key.len() - 1] {
        put(&short_key, &key[..cut], 0o600);
        let refused =
            check_enrollment(&short_key, &ca.certificate, &ca.sha256).expect_err("a short key");
        assert_eq!(
            refused.to_string(),
            format!(
                "the CA key file {} is not a PKCS#8 ECDSA P-256 key: Could not parse key pair",
                short_key.display()
            )
        );
    }
    let cuts = [
        (0, "Parsing requires 1 bytes/chars"),
        (certificate.len() / 2, "Parsing Error: NomError(Eof)"),
        (certificate.len() - 1, "Parsing Error: NomError(Eof)"),
    ];
    for (cut, why) in cuts {
        put(&short_certificate, &certificate[..cut], 0o644);
        let refused = check_enrollment(&ca.key, &short_certificate, &ca.sha256)
            .expect_err("a short certificate");
        assert_eq!(
            refused.to_string(),
            format!(
                "the CA certificate file {} is not a DER certificate: {why}",
                short_certificate.display()
            )
        );
    }
    let missing = dir.path().join("missing");
    let refused = check_enrollment(&missing, &ca.certificate, &ca.sha256).expect_err("no key");
    assert_eq!(
        refused.to_string(),
        format!("{} does not exist; make the CA first", missing.display())
    );
    let refused = check_enrollment(&ca.key, &missing, &ca.sha256).expect_err("no certificate");
    assert_eq!(
        refused.to_string(),
        format!("{} does not exist; make the CA first", missing.display())
    );
}

/// Proves: a key file group or others may read is refused, as the coordinator's start refuses
/// it, so a copy the check passes is one the coordinator starts with.
/// Catches: a restore drill that leaves the key readable and is told it is good.
#[test]
fn a_key_open_to_others_is_refused() {
    let dir = scratch("check-ca-enrollment-mode");
    let ca = enrollment_ca(dir.path());
    fs::set_permissions(&ca.key, fs::Permissions::from_mode(0o640)).expect("a mode");
    let refused = check_enrollment(&ca.key, &ca.certificate, &ca.sha256).expect_err("open");
    assert!(
        matches!(
            refused,
            CheckCaError::Enrollment(hostkeeper_enrollment::ca::CaError::KeyNotPrivate { .. })
        ),
        "{refused}"
    );
}

/// An SSH key file and a public key file in a new directory.
struct SshFiles {
    _dir: Scratch,
    key: PathBuf,
    public: PathBuf,
}

fn ssh_files(name: &str, secret: &str, public: &str) -> SshFiles {
    let dir = scratch(name);
    let key = dir.path().join("ssh_user_ca");
    let public_file = dir.path().join("ca_keys.pub");
    put(&key, secret.as_bytes(), 0o600);
    put(&public_file, public.as_bytes(), 0o644);
    SshFiles {
        _dir: dir,
        key,
        public: public_file,
    }
}

/// Proves: a key whose public line the file holds (with `ssh-ca init`'s comment) passes with a
/// report of its fingerprint and the paths alone: no line of the private key file is in it.
/// Catches: a check that prints the key, or passes without reading the public file.
#[test]
fn an_ssh_key_the_hosts_trust_passes_with_its_fingerprint_and_no_key() {
    let (key, pem) = ssh_key();
    let public = format!("{} hostkeeper-ssh-user-ca\n", key.public_line());
    let files = ssh_files("check-ca-ssh-pass", &pem, &public);
    let report = check_ssh(&files.key, &files.public).expect("a trusted key");
    assert_eq!(
        report,
        format!(
            "fingerprint: {}\nevery key of {} is a line of {}\n",
            key.fingerprint(),
            files.key.display(),
            files.public.display()
        )
    );
    for line in body_lines(&pem) {
        assert!(!report.contains(line), "{report}");
    }
}

/// Proves: during a rotation, a key file of two keys (current, then next) passes against a
/// hosts' file of both lines among blank lines and comments, reporting both fingerprints in
/// the file's order; with the next key missing from the hosts' file, it is refused by its
/// fingerprint.
/// Catches: a check that looks at the first key of a two-key secret only.
#[test]
fn both_keys_of_a_rotation_must_be_trusted() {
    let (current, current_pem) = ssh_key();
    let (next, next_pem) = ssh_key();
    let secret = format!("{current_pem}{next_pem}");
    let both = format!(
        "# the fleet's CA keys\n\n{}\n{} next\n",
        current.public_line(),
        next.public_line()
    );
    let files = ssh_files("check-ca-ssh-rotation", &secret, &both);
    let report = check_ssh(&files.key, &files.public).expect("both trusted");
    assert_eq!(
        report.lines().take(2).collect::<Vec<_>>(),
        [
            format!("fingerprint: {}", current.fingerprint()),
            format!("fingerprint: {}", next.fingerprint())
        ]
    );
    put(
        &files.public,
        format!("{}\n", current.public_line()).as_bytes(),
        0o644,
    );
    let refused = check_ssh(&files.key, &files.public).expect_err("next not trusted");
    assert_eq!(
        refused.to_string(),
        format!(
            "the key {} is not a line of --public-file",
            next.fingerprint()
        )
    );
}

/// Proves: the key of another CA (the control plane's against the fleet's public line) is
/// refused, naming its fingerprint and no byte of it.
/// Catches: a check that passes any well-formed key.
#[test]
fn an_ssh_key_from_another_ca_is_refused() {
    let (fleet, _) = ssh_key();
    let (control_plane, pem) = ssh_key();
    let files = ssh_files(
        "check-ca-ssh-other",
        &pem,
        &format!("{}\n", fleet.public_line()),
    );
    let refused = check_ssh(&files.key, &files.public).expect_err("another CA");
    assert_eq!(
        refused.to_string(),
        format!(
            "the key {} is not a line of --public-file",
            control_plane.fingerprint()
        )
    );
}

/// Proves: an SSH key file cut short, empty, larger than two keys, or open to others is
/// refused with the whole message, which names the flag and no byte of the key.
/// Catches: a partial copy passing, or a parse error that quotes the file.
#[test]
fn a_truncated_or_unsafe_ssh_key_file_is_refused() {
    let (key, pem) = ssh_key();
    let public = format!("{}\n", key.public_line());
    let files = ssh_files("check-ca-ssh-truncated", &pem, &public);
    let bad = "--key-file: not an unencrypted Ed25519 OpenSSH private key";
    for cut in [pem.len() - 40, pem.len() / 2, 40, 0] {
        put(&files.key, &pem.as_bytes()[..cut], 0o600);
        let refused = check_ssh(&files.key, &files.public).expect_err("a short key");
        assert_eq!(refused.to_string(), bad);
    }
    put(&files.key, format!("{pem}{pem}{pem}").as_bytes(), 0o600);
    let refused = check_ssh(&files.key, &files.public).expect_err("three keys");
    assert_eq!(refused.to_string(), bad);
    let large = vec![b'a'; usize::try_from(MAX_SECRET_BYTES).expect("small") + 1];
    put(&files.key, &large, 0o600);
    let refused = check_ssh(&files.key, &files.public).expect_err("too large");
    assert_eq!(refused.to_string(), "--key-file: larger than 16384 bytes");
    put(&files.key, pem.as_bytes(), 0o644);
    let refused = check_ssh(&files.key, &files.public).expect_err("open to others");
    assert!(matches!(refused, CheckCaError::Secret(_)), "{refused}");
    assert!(refused.to_string().starts_with("--key-file: "), "{refused}");
}

/// Proves: a public file that is missing, too large, holds no key, or holds a line that is not
/// a public key is refused, naming the flag (and the line).
/// Catches: an empty or broken hosts' file read as "no key to compare", and so a pass.
#[test]
fn a_public_file_with_no_key_or_a_bad_line_is_refused() {
    let (key, pem) = ssh_key();
    let files = ssh_files("check-ca-ssh-public", &pem, "# nothing\n\n");
    let refused = check_ssh(&files.key, &files.public).expect_err("no key");
    assert_eq!(refused.to_string(), "--public-file: holds no public key");
    put(
        &files.public,
        format!("{}\nssh-ed25519 AAAA\n", key.public_line()).as_bytes(),
        0o644,
    );
    let refused = check_ssh(&files.key, &files.public).expect_err("a bad line");
    assert_eq!(
        refused.to_string(),
        "--public-file: line 2: not a public key of a type the CA signs"
    );
    put(&files.public, &vec![b'#'; 8 * 1024 + 1], 0o644);
    let refused = check_ssh(&files.key, &files.public).expect_err("too large");
    assert_eq!(
        refused.to_string(),
        format!(
            "--public-file: {}: too long for a public key file",
            files.public.display()
        )
    );
    fs::remove_file(&files.public).expect("removed");
    let refused = check_ssh(&files.key, &files.public).expect_err("missing");
    assert!(
        refused
            .to_string()
            .starts_with(&format!("--public-file: {}: ", files.public.display())),
        "{refused}"
    );
}
