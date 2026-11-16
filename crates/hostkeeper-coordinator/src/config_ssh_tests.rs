//! The flags of the SSH user CA and the start's use of them: what clap requires together, the
//! defaults, and what `signing` makes of the files the flags name.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;

use clap::Parser as _;

use super::{SigningKey, SshStartError, account, signing};
use crate::config::{Cli, Command, Serve};
use crate::enrollment::os_random;
use crate::ssh_cert::{CaKey, KeyError};
use crate::ssh_ledger::{Ledger, Memory};
use crate::testutil::scratch;

/// The flags of `serve` on the database, with `extra` after them.
fn serve(extra: &[&str]) -> Result<Serve, clap::Error> {
    let mut args = vec![
        "hostkeeper-coordinator",
        "serve",
        "--heartbeat-listen",
        "127.0.0.1:1",
        "--private-listen",
        "127.0.0.1:2",
        "--tls-cert",
        "c",
        "--tls-key",
        "k",
        "--client-ca",
        "ca",
        "--oidc-config",
        "o",
        "--database-dsn-file",
        "dsn",
        "--halt-marker-file",
        "marker",
    ];
    args.extend(extra);
    match Cli::try_parse_from(args)?.command {
        Command::Serve(flags) => Ok(flags),
        other => panic!("{other:?}"),
    }
}

fn secret(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, text).expect("a file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    path.display().to_string()
}

fn key_file(seed: u8) -> String {
    let counting = |buffer: &mut [u8]| {
        buffer.fill(seed);
        true
    };
    CaKey::generate("t", &counting).expect("a key").1
}

fn ledger() -> Option<Arc<dyn Ledger>> {
    Some(Arc::new(Memory::at(1)))
}

const OPERATORS: &str = "[[operator]]\nname = \"alice\"\nclasses = [\"fleet\"]\n";

/// Proves: the SSH flags are off by default, and each has the design's default: the signing key
/// `current`, an hour, `root` on Linux, and no login user for the Macs; with none given the
/// signing is none.
///
/// Catches: a default that turns the feature on, or a default that is not the design's.
#[test]
fn the_ssh_flags_are_off_by_default_and_have_the_designs_defaults() {
    let flags = serve(&[]).expect("flags");
    assert_eq!(flags.ssh.ssh_ca_secret_file, None);
    assert_eq!(flags.ssh.ssh_signing_key, SigningKey::Current);
    assert_eq!(flags.ssh.ssh_cert_ttl_s, 3600);
    assert_eq!(flags.ssh.ssh_login_user_linux, "root");
    assert_eq!(flags.ssh.ssh_login_user_macos, "");
    assert!(flags.ssh.ssh_control_plane_host.is_empty());
    assert!(signing(&flags, ledger()).expect("none").is_none());
}

/// Proves: the CA secret needs the operators file and the database, the operators file and each
/// other SSH flag need the CA secret, and the certificate's TTL is between a minute and eight
/// hours.
///
/// Catches: a coordinator that holds a CA and has no one to sign for, or whose certificates are
/// kept nowhere.
#[test]
fn the_ssh_flags_require_each_other_and_the_ttl_has_bounds() {
    let refused = |extra: &[&str]| serve(extra).expect_err("refused").to_string();
    assert!(refused(&["--ssh-ca-secret-file", "s"]).contains("--ssh-operators-file"));
    assert!(refused(&["--ssh-operators-file", "o"]).contains("--ssh-ca-secret-file"));
    for flag in [
        "--ssh-cp-ca-secret-file",
        "--ssh-control-plane-host",
        "--ssh-signing-key",
        "--ssh-cert-ttl-s",
        "--ssh-login-user-linux",
        "--ssh-login-user-macos",
    ] {
        let value = if flag == "--ssh-signing-key" { "next" } else if flag == "--ssh-cert-ttl-s" { "60" } else { "x" };
        assert!(refused(&[flag, value]).contains("--ssh-ca-secret-file"), "{flag}");
    }
    for ttl in ["59", "28801", "x"] {
        assert!(
            serve(&["--ssh-ca-secret-file", "s", "--ssh-operators-file", "o", "--ssh-cert-ttl-s", ttl]).is_err(),
            "{ttl}"
        );
    }
    for ttl in ["60", "28800"] {
        assert!(serve(&["--ssh-ca-secret-file", "s", "--ssh-operators-file", "o", "--ssh-cert-ttl-s", ttl]).is_ok());
    }
    let file_store = Cli::try_parse_from([
        "hostkeeper-coordinator", "serve", "--heartbeat-listen", "127.0.0.1:1", "--private-listen",
        "127.0.0.1:2", "--tls-cert", "c", "--tls-key", "k", "--client-ca", "ca", "--oidc-config", "o",
        "--state-file", "s", "--ssh-ca-secret-file", "s", "--ssh-operators-file", "o",
    ])
    .expect_err("the certificates are kept in the database")
    .to_string();
    assert!(file_store.contains("--database-dsn-file"), "{file_store}");
}

/// Proves: with the files given, `signing` holds the keys of the secret (one, or two with
/// `current` first), signs with the one `--ssh-signing-key` names, takes the control-plane key
/// and hosts, and carries the TTL and the login users of the flags.
///
/// Catches: a rotation that signs with the wrong key, and a flag that is read and dropped.
#[test]
fn signing_is_made_from_the_files_and_flags() {
    let dir = scratch("ssh-config-signing");
    let one = key_file(1);
    let two = key_file(2);
    let both = secret(dir.path(), "both", &format!("{one}{two}"));
    let cp = secret(dir.path(), "cp", &key_file(3));
    let operators = secret(dir.path(), "operators.toml", OPERATORS);
    let flags = serve(&[
        "--ssh-ca-secret-file", &both, "--ssh-operators-file", &operators,
        "--ssh-cp-ca-secret-file", &cp, "--ssh-control-plane-host", "nuc",
        "--ssh-control-plane-host", "nuc-2", "--ssh-signing-key", "next",
        "--ssh-cert-ttl-s", "900", "--ssh-login-user-linux", "ops", "--ssh-login-user-macos", "admin",
    ])
    .expect("flags");
    let made = signing(&flags, ledger()).expect("made").expect("on");
    assert_eq!(made.fleet.len(), 2);
    assert_eq!(made.signing, 1);
    assert!(made.control_plane.is_some());
    assert_eq!(made.control_plane_hosts.iter().map(String::as_str).collect::<Vec<_>>(), ["nuc", "nuc-2"]);
    assert_eq!((made.ttl_secs, made.login_linux.as_str(), made.login_macos.as_str()), (900, "ops", "admin"));
    assert_eq!(made.operators, Path::new(&operators));
    assert_eq!(made.fleet[0].public_line(), CaKey::parse_secret(&one).expect("a key")[0].public_line());
    let plain = serve(&["--ssh-ca-secret-file", &both, "--ssh-operators-file", &operators]).expect("flags");
    let made = signing(&plain, ledger()).expect("made").expect("on");
    assert_eq!((made.signing, made.control_plane.is_none()), (0, true));
    assert!(format!("{made:?}").starts_with("Signing"));
    let _ = os_random();
}

/// Proves: `signing` refuses at start, naming the flag: a CA secret file that others can read, or
/// is not keys, `next` with one key, a control-plane secret of two keys, a control-plane host
/// that is not a host id, a login user that is not an account name, and an operators file that does
/// not parse.
///
/// Catches: a coordinator that starts and then cannot sign, or signs with a key it read from a
/// file anyone could read.
#[test]
fn signing_refuses_what_is_wrong_with_a_file_or_a_flag_at_start() {
    let dir = scratch("ssh-config-refused");
    let one = secret(dir.path(), "one", &key_file(1));
    let two = secret(dir.path(), "two", &format!("{}{}", key_file(1), key_file(2)));
    let operators = secret(dir.path(), "operators.toml", OPERATORS);
    let junk = secret(dir.path(), "junk", "not keys");
    let broken = secret(dir.path(), "broken.toml", "not [ toml");
    let open = dir.path().join("open");
    fs::write(&open, key_file(1)).expect("a file");
    fs::set_permissions(&open, fs::Permissions::from_mode(0o644)).expect("mode");
    let open = open.display().to_string();
    let base = ["--ssh-ca-secret-file", &one, "--ssh-operators-file", &operators];
    let refused = |extra: &[&str]| {
        let flags = serve(&[&base[..], extra].concat()).expect("flags");
        signing(&flags, ledger()).expect_err("refused")
    };
    let mut on_open = serve(&["--ssh-ca-secret-file", &open, "--ssh-operators-file", &operators]).expect("flags");
    assert!(matches!(signing(&on_open, ledger()), Err(SshStartError::Secret(_))));
    on_open.ssh.ssh_ca_secret_file = Some(junk.into());
    assert!(matches!(
        signing(&on_open, ledger()),
        Err(SshStartError::Key { flag: "--ssh-ca-secret-file", why: KeyError::BadPrivateKey })
    ));
    assert!(matches!(refused(&["--ssh-signing-key", "next"]), SshStartError::NoNextKey));
    assert!(matches!(refused(&["--ssh-cp-ca-secret-file", &two]), SshStartError::ControlPlaneKeys(2)));
    assert!(matches!(refused(&["--ssh-control-plane-host", "Nuc"]), SshStartError::HostId(h) if h == "Nuc"));
    assert!(matches!(refused(&["--ssh-login-user-linux", ""]), SshStartError::LoginUser("--ssh-login-user-linux")));
    assert!(matches!(refused(&["--ssh-login-user-macos", "-x"]), SshStartError::LoginUser("--ssh-login-user-macos")));
    let flags = serve(&["--ssh-ca-secret-file", &one, "--ssh-operators-file", &broken]).expect("flags");
    assert!(matches!(signing(&flags, ledger()), Err(SshStartError::Operators(_))));
    assert!(refused(&["--ssh-login-user-linux", "a b"]).to_string().contains("not an account name"));
}

/// Proves: an account name is 1 to 32 bytes of letters, digits, `_`, `-` and `.` that does not
/// start with `-`.
///
/// Catches: a login user with a space or a leading dash that reaches a command line.
#[test]
fn an_account_name_is_a_plain_one() {
    for good in ["root", "admin", "ops_1", "a.b-c", &"a".repeat(32)] {
        assert!(account(good), "{good}");
    }
    for bad in ["", "-x", "a b", "a/b", "a;b", &"a".repeat(33)] {
        assert!(!account(bad), "{bad}");
    }
}
