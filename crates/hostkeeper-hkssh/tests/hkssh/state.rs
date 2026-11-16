//! The state directory and the host id rule.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use hostkeeper_hkssh::{CertInfo, State, check_host_id};
use hostkeeper_testkit::Scratch;

fn scratch() -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "hkstate").expect("a scratch directory")
}

/// Proves: a host id is 1 to 64 of letters, digits, `.`, `_` and `-`, not starting with `.` or
/// `-`: nothing that could name another path or look like a flag.
#[test]
fn a_host_id_is_a_plain_word() {
    for good in ["h", "host-7", "a.b_c", "0123", &"x".repeat(64)] {
        assert!(check_host_id(good).is_ok(), "{good}");
    }
    for bad in ["", ".hidden", "-flag", "a/b", "a b", "..", "h\u{e9}", &"x".repeat(65), "a\nb"] {
        let error = check_host_id(bad).expect_err(bad);
        assert_eq!(error.exit_code(), 2, "{bad:?}");
    }
}

/// Proves: the directory and the files in it are private (0700 and 0600), a certificate record
/// reads back as written, a record that is not one reads as none, and pins are one per line.
#[test]
fn the_state_is_private_and_a_record_reads_back() {
    let scratch = scratch();
    let state = State::new(scratch.path().join("state"));
    let info = CertInfo {
        serial: 5,
        expires: 99,
        login_user: "root".into(),
        key_id: "alice/5".into(),
        public_key: "ssh-ed25519 AAAA".into(),
    };
    assert_eq!(state.cert_info("h"), None);
    state.store_cert("h", "ssh-ed25519-cert-v01@openssh.com AAAA", &info).expect("store");
    state.store_pins("h", &["SHA256:a".into(), "SHA256:b".into()]).expect("pins");
    assert_eq!(state.cert_info("h"), Some(info));
    let mode = |path: &Path| std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&scratch.path().join("state")), 0o700);
    assert_eq!(mode(&scratch.path().join("state/hosts")), 0o700);
    assert_eq!(mode(&state.cert_file("h")), 0o600);
    assert_eq!(std::fs::read_to_string(state.pins_file("h")).expect("pins"), "SHA256:a\nSHA256:b\n");
    assert_eq!(mode(&state.pins_file("h")), 0o600);
    std::fs::write(state.pins_file("h").with_file_name("h.info"), "serial=x\n").expect("junk");
    assert_eq!(state.cert_info("h"), None);
}

/// Proves: a state directory that cannot be made (its parent is a file) and a file that cannot be
/// written (its directory is missing) are errors that name the path.
#[test]
fn a_state_that_cannot_be_written_is_an_error_naming_the_path() {
    let scratch = scratch();
    let file = scratch.path().join("file");
    std::fs::write(&file, "").expect("a file");
    let state = State::new(file.join("state"));
    let error = state.ensure().expect_err("not a directory");
    assert!(error.to_string().starts_with(&format!("cannot write {}", file.join("state").display())), "{error}");
    let error = State::new(scratch.path().join("absent"))
        .write(&scratch.path().join("absent/x"), "t")
        .expect_err("no directory");
    assert!(error.to_string().contains("absent/x.tmp") || error.to_string().contains("absent/x"), "{error}");
}
