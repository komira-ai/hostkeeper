//! The flags of the SSH user CA and the start's use of them: what clap requires together, the
//! defaults, and what `signing` makes of the files the flags name.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;

use clap::Parser as _;

use super::{
    SSHD_PORT, SigningKey, SshFlags, SshStartError, account, attach, drained, limits, signing,
};
use crate::config::{Cli, Command, Serve, StartError};
use crate::enrollment::{Random, os_random};
use crate::host_groups_support::coordinator_on;
use crate::ssh_book::{self, Book};
use crate::ssh_cert::{CaKey, KeyError};
use crate::ssh_ledger::{Ledger, Memory};
use crate::ssh_tunnel::tests::Rig as TunnelRig;
use crate::store::Fake;
use crate::testutil::{Rig, scratch};

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
        let value = if flag == "--ssh-signing-key" {
            "next"
        } else if flag == "--ssh-cert-ttl-s" {
            "60"
        } else {
            "x"
        };
        assert!(
            refused(&[flag, value]).contains("--ssh-ca-secret-file"),
            "{flag}"
        );
    }
    for ttl in ["59", "28801", "x"] {
        assert!(
            serve(&[
                "--ssh-ca-secret-file",
                "s",
                "--ssh-operators-file",
                "o",
                "--ssh-cert-ttl-s",
                ttl
            ])
            .is_err(),
            "{ttl}"
        );
    }
    for ttl in ["60", "28800"] {
        assert!(
            serve(&[
                "--ssh-ca-secret-file",
                "s",
                "--ssh-operators-file",
                "o",
                "--ssh-cert-ttl-s",
                ttl
            ])
            .is_ok()
        );
    }
    let file_store = Cli::try_parse_from([
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
        "--state-file",
        "s",
        "--ssh-ca-secret-file",
        "s",
        "--ssh-operators-file",
        "o",
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
        "--ssh-ca-secret-file",
        &both,
        "--ssh-operators-file",
        &operators,
        "--ssh-cp-ca-secret-file",
        &cp,
        "--ssh-control-plane-host",
        "nuc",
        "--ssh-control-plane-host",
        "nuc-2",
        "--ssh-signing-key",
        "next",
        "--ssh-cert-ttl-s",
        "900",
        "--ssh-login-user-linux",
        "ops",
        "--ssh-login-user-macos",
        "admin",
    ])
    .expect("flags");
    let made = signing(&flags, ledger()).expect("made").expect("on");
    assert_eq!(made.fleet.len(), 2);
    assert_eq!(made.signing, 1);
    assert!(made.control_plane.is_some());
    assert_eq!(
        made.control_plane_hosts
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["nuc", "nuc-2"]
    );
    assert_eq!(
        (
            made.ttl_secs,
            made.login_linux.as_str(),
            made.login_macos.as_str()
        ),
        (900, "ops", "admin")
    );
    assert_eq!(made.operators, Path::new(&operators));
    assert_eq!(
        made.fleet[0].public_line(),
        CaKey::parse_secret(&one).expect("a key")[0].public_line()
    );
    let plain = serve(&[
        "--ssh-ca-secret-file",
        &both,
        "--ssh-operators-file",
        &operators,
    ])
    .expect("flags");
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
    let two = secret(
        dir.path(),
        "two",
        &format!("{}{}", key_file(1), key_file(2)),
    );
    let operators = secret(dir.path(), "operators.toml", OPERATORS);
    let junk = secret(dir.path(), "junk", "not keys");
    let broken = secret(dir.path(), "broken.toml", "not [ toml");
    let open = dir.path().join("open");
    fs::write(&open, key_file(1)).expect("a file");
    fs::set_permissions(&open, fs::Permissions::from_mode(0o644)).expect("mode");
    let open = open.display().to_string();
    let base = [
        "--ssh-ca-secret-file",
        &one,
        "--ssh-operators-file",
        &operators,
    ];
    let refused = |extra: &[&str]| {
        let flags = serve(&[&base[..], extra].concat()).expect("flags");
        signing(&flags, ledger()).expect_err("refused")
    };
    let mut on_open = serve(&[
        "--ssh-ca-secret-file",
        &open,
        "--ssh-operators-file",
        &operators,
    ])
    .expect("flags");
    assert!(matches!(
        signing(&on_open, ledger()),
        Err(SshStartError::Secret(_))
    ));
    on_open.ssh.ssh_ca_secret_file = Some(junk.into());
    assert!(matches!(
        signing(&on_open, ledger()),
        Err(SshStartError::Key {
            flag: "--ssh-ca-secret-file",
            why: KeyError::BadPrivateKey
        })
    ));
    assert!(matches!(
        refused(&["--ssh-signing-key", "next"]),
        SshStartError::NoNextKey
    ));
    assert!(matches!(
        refused(&["--ssh-cp-ca-secret-file", &two]),
        SshStartError::ControlPlaneKeys(2)
    ));
    assert!(
        matches!(refused(&["--ssh-control-plane-host", "Nuc"]), SshStartError::HostId(h) if h == "Nuc")
    );
    assert!(matches!(
        refused(&["--ssh-login-user-linux", ""]),
        SshStartError::LoginUser("--ssh-login-user-linux")
    ));
    assert!(matches!(
        refused(&["--ssh-login-user-macos=-x"]),
        SshStartError::LoginUser("--ssh-login-user-macos")
    ));
    let flags = serve(&[
        "--ssh-ca-secret-file",
        &one,
        "--ssh-operators-file",
        &broken,
    ])
    .expect("flags");
    assert!(matches!(
        signing(&flags, ledger()),
        Err(SshStartError::Operators(_))
    ));
    assert!(
        refused(&["--ssh-login-user-linux", "a b"])
            .to_string()
            .contains("not an account name")
    );
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

/// The files of a CA and an operators file in `dir`, as the flags name them.
fn ca_flags(dir: &Path) -> [String; 4] {
    [
        "--ssh-ca-secret-file".to_owned(),
        secret(dir, "ca", &key_file(1)),
        "--ssh-operators-file".to_owned(),
        secret(dir, "operators.toml", OPERATORS),
    ]
}

fn serve_with(base: &[String], extra: &[&str]) -> Result<Serve, clap::Error> {
    let all: Vec<&str> = base
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied())
        .collect();
    serve(&all)
}

/// The values of the eight tunnel flags, in the order of [`TUNNEL_FLAGS`].
fn tunnel_values(ssh: &SshFlags) -> [u64; 8] {
    [
        ssh.ssh_live_s,
        ssh.ssh_idle_s,
        ssh.ssh_max_session_s,
        ssh.ssh_dial_timeout_s,
        ssh.ssh_max_tunnels,
        ssh.ssh_max_tunnels_per_operator,
        ssh.ssh_revoke_poll_s,
        ssh.ssh_drain_s,
    ]
}

/// Each tunnel flag with its default and its range, as the design's section 10 gives them.
const TUNNEL_FLAGS: [(&str, u64, u64, u64); 8] = [
    ("--ssh-live-s", 120, 10, 3600),
    ("--ssh-idle-s", 900, 10, 86_400),
    ("--ssh-max-session-s", 28_800, 60, 86_400),
    ("--ssh-dial-timeout-s", 5, 1, 60),
    ("--ssh-max-tunnels", 16, 1, 1024),
    ("--ssh-max-tunnels-per-operator", 4, 1, 64),
    ("--ssh-revoke-poll-s", 10, 1, 300),
    ("--ssh-drain-s", 600, 0, 3600),
];

/// Proves: each of the eight tunnel flags has its default, takes the two ends of its range and
/// refuses a value outside it or one that is not a number, and each needs `--ssh-ca-secret-file`.
///
/// Catches: a default that is not the design's, a range that lets a zero timer or a day-long dial
/// through, and a tunnel flag taken on a coordinator that has no CA.
#[test]
fn the_tunnel_flags_have_defaults_and_ranges_and_need_the_ca() {
    let dir = scratch("ssh-config-tunnel-flags");
    let base = ca_flags(dir.path());
    let plain = serve_with(&base, &[]).expect("flags");
    assert_eq!(
        tunnel_values(&plain.ssh),
        TUNNEL_FLAGS.map(|(_, default, _, _)| default)
    );
    for (n, (flag, _, min, max)) in TUNNEL_FLAGS.into_iter().enumerate() {
        for (value, ok) in [(min, true), (max, true), (max + 1, false)] {
            let flags = serve_with(&base, &[flag, &value.to_string()]);
            assert_eq!(flags.is_ok(), ok, "{flag} {value}");
            if let Ok(flags) = flags {
                assert_eq!(tunnel_values(&flags.ssh)[n], value, "{flag}");
            }
        }
        if min > 0 {
            assert!(
                serve_with(&base, &[flag, &(min - 1).to_string()]).is_err(),
                "{flag} below its range"
            );
        }
        assert!(serve_with(&base, &[flag, "x"]).is_err(), "{flag} x");
        let alone = serve(&[flag, &min.to_string()]).expect_err("needs the CA");
        assert!(
            alone.to_string().contains("--ssh-ca-secret-file"),
            "{flag}: {alone}"
        );
    }
}

/// Proves: the limits of the tunnels are the flags' values (seconds as durations), and the port
/// dialled is the one given, which the start makes 22.
///
/// Catches: a flag that is read and dropped, two flags swapped, and a start that dials a port
/// other than the host's sshd.
#[test]
fn the_limits_of_the_tunnels_are_the_flags_values() {
    let dir = scratch("ssh-config-limits");
    let base = ca_flags(dir.path());
    let flags = serve_with(
        &base,
        &[
            "--ssh-live-s",
            "11",
            "--ssh-idle-s",
            "12",
            "--ssh-max-session-s",
            "61",
            "--ssh-dial-timeout-s",
            "4",
            "--ssh-max-tunnels",
            "7",
            "--ssh-max-tunnels-per-operator",
            "3",
            "--ssh-revoke-poll-s",
            "9",
            "--ssh-drain-s",
            "0",
        ],
    )
    .expect("flags");
    let made = limits(&flags.ssh, SSHD_PORT);
    assert_eq!(made.live_secs, 11);
    assert_eq!(made.idle, std::time::Duration::from_secs(12));
    assert_eq!(made.max_session, std::time::Duration::from_secs(61));
    assert_eq!(made.dial_timeout, std::time::Duration::from_secs(4));
    assert_eq!(made.max_tunnels, 7);
    assert_eq!(made.per_operator, 3);
    assert_eq!(made.poll, std::time::Duration::from_secs(9));
    assert_eq!(made.drain, std::time::Duration::ZERO);
    assert_eq!(made.port, 22);
}

fn book() -> Option<Arc<dyn Book>> {
    Some(Arc::new(ssh_book::Memory::over(Arc::new(Memory::at(1)))))
}

/// A random source that fills each buffer with `byte`.
fn filling(byte: u8) -> Random {
    Box::new(move |buffer| {
        buffer.fill(byte);
        true
    })
}

/// Proves: with the CA flags, `attach` gives the coordinator its signing and its tunnels, the
/// instance's id the 16 hex characters of the random source; with no CA flag it gives it neither;
/// a random source that has no bytes refuses the start.
///
/// Catches: a start that signs and cannot tunnel (or the reverse), a tunnel runtime made on a
/// coordinator that has no CA, and an instance id that is empty or the same on every instance.
#[test]
fn attach_gives_the_coordinator_its_tunnels_exactly_when_it_signs() {
    let dir = scratch("ssh-config-attach");
    let rig = Rig::new("ssh-config-attach-rig");
    let bare = || coordinator_on(&Fake::new(), &rig.clock, None);
    let on = serve_with(&ca_flags(dir.path()), &[]).expect("flags");
    let attached = attach(bare(), &on, ledger(), book(), &filling(0xab)).expect("attached");
    assert!(attached.ssh.is_some());
    let tunnels = attached.tunnels.expect("tunnels");
    assert_eq!(tunnels.instance(), "abababababababab");
    assert!(!tunnels.draining());

    let off = serve(&[]).expect("flags");
    let attached = attach(bare(), &off, ledger(), book(), &filling(1)).expect("attached");
    assert!(attached.ssh.is_none() && attached.tunnels.is_none());

    let none: Random = Box::new(|_| false);
    assert!(matches!(
        attach(bare(), &on, ledger(), book(), &none),
        Err(SshStartError::InstanceId)
    ));
    assert_eq!(
        SshStartError::InstanceId.to_string(),
        "the random source gave no bytes for the id of this instance"
    );
    let bookless = attach(bare(), &on, ledger(), None, &filling(1)).expect("attached");
    assert!(bookless.ssh.is_none() && bookless.tunnels.is_none());
}

fn serving_ends() -> std::future::Ready<Result<std::convert::Infallible, StartError>> {
    std::future::ready(Err(StartError::from(std::io::Error::other("deposed"))))
}

/// Proves: the drain of a stop ends at once for a coordinator without tunnels and for one whose
/// tunnels are all closed, leaving the instance draining, and waits for a tunnel that is still
/// held; the serving future is polled while it waits, and its end is the result.
///
/// Catches: a stop that waits on tunnels that are not there, one that stops serving while
/// tunnels drain, and one that swallows why the serving ended.
#[tokio::test]
async fn the_drain_of_a_stop_waits_for_tunnels_while_serving_goes_on() {
    let rig = Rig::new("ssh-config-drained-none");
    let bare = coordinator_on(&Fake::new(), &rig.clock, None);
    drained(&bare, std::future::pending())
        .await
        .expect("nothing to drain");

    let tunnels = TunnelRig::new("ssh-config-drained", |_| {}).await;
    let slot = tunnels.tunnels.reserve().expect("a slot");
    let ended = drained(&tunnels.coordinator, serving_ends()).await;
    let why = ended.expect_err("the serving ended while a tunnel was held");
    assert!(why.to_string().contains("deposed"), "{why}");
    assert!(tunnels.tunnels.draining());

    drop(slot);
    drained(&tunnels.coordinator, std::future::pending())
        .await
        .expect("no tunnel is held: the drain is over");
}
