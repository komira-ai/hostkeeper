//! `hostkeeper-coordinator serve --live-rollouts N`: how many services may have a live rollout at
//! once. The flag defaults to 1, takes 1 to 16, is refused outside that with clap's usage error
//! (exit 2), is named in the help, and a coordinator is built from it. Driven in-process through
//! `config::{Cli, build, main}`; nothing binds a socket.
//!
//! The built coordinator reads its clock from the process (`Instant`), so it is within its quiet
//! start, which orders nothing, for as long as these tests run: they cannot drive two services
//! rolling at once through it. That the number is the rule the plan follows (a second service's
//! rollout is not held at 2, is at 1) is proved over a coordinator on a clock the test sets, in
//! `src/plan_status_tests.rs`; here only that the flag is accepted, refused and built.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_coordinator::config::{Cli, Command, Parts, Serve, StartError, build};
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_testkit::Scratch;
use rustls::pki_types::CertificateDer;
use support::{Ca, OPERATOR, scratch};

/// The files the flags name, in a directory of their own.
struct Files {
    dir: Scratch,
}

impl Files {
    fn new(name: &str) -> Self {
        let files = Self { dir: scratch(name) };
        let leaf = Ca::new("live rollouts CA").coordinator();
        files.put("tls.pem", &leaf.cert_pem);
        files.put("tls.key", &leaf.key_pem);
        files.put("ca.pem", &Ca::new("clients CA").pem);
        files.put("oidc.toml", &config_text(STATIC_ISSUER));
        files.put("operator.token", OPERATOR);
        files
    }

    /// Writes `file` with mode 0600, which the key and the token need and the rest take.
    fn put(&self, file: &str, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.dir.path().join(file);
        std::fs::write(&path, text).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    }

    /// The arguments of a `serve` that builds, with `extra` after them.
    fn args(&self, extra: &[&str]) -> Vec<OsString> {
        let path = |file: &str| self.dir.path().join(file).display().to_string();
        let mut args: Vec<String> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        for (flag, value) in [
            ("--heartbeat-listen", "192.0.2.1:1".to_owned()),
            ("--private-listen", "192.0.2.1:2".to_owned()),
            ("--tls-cert", path("tls.pem")),
            ("--tls-key", path("tls.key")),
            ("--client-ca", path("ca.pem")),
            ("--state-file", path("desired.json")),
            ("--oidc-config", path("oidc.toml")),
            ("--operator-token-file", path("operator.token")),
            ("--legacy-service", "kbf".to_owned()),
            ("--role-uri-prefix", "urn:kbf:role:".to_owned()),
            ("--host-role-uri-prefix", "urn:kbf:host-role:".to_owned()),
        ] {
            args.push(flag.to_owned());
            args.push(value);
        }
        args.extend(extra.iter().map(|a| (*a).to_owned()));
        args.into_iter().map(OsString::from).collect()
    }
}

/// The flags of a `serve` with `--live-rollouts value` after the rest.
fn flags_with(files: &Files, value: &str) -> Serve {
    flags_of(files.args(&["--live-rollouts", value]))
}

fn flags_of(args: Vec<OsString>) -> Serve {
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("serve")
    };
    flags
}

/// [`build`] trusting a CA of the test, so nothing reads the host's certificate store.
fn build_given(flags: &Serve) -> Result<Parts, StartError> {
    build(
        flags,
        Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)]),
    )
}

/// Proves: without the flag a coordinator lets one service's rollout be live at a time.
/// Catches: a default other than 1, which would let rollouts overlap on every farm that does not
/// ask for it.
#[test]
fn live_rollouts_defaults_to_one() {
    let files = Files::new("live-rollouts-default");
    assert_eq!(flags_of(files.args(&[])).live_rollouts, 1);
}

/// Proves: `--live-rollouts` takes every number from 1 to 16 as it is given.
/// Catches: a range that stops short of 16, or starts above 1.
#[test]
fn live_rollouts_takes_one_to_sixteen() {
    let files = Files::new("live-rollouts-range");
    for n in 1..=16_usize {
        assert_eq!(flags_with(&files, &n.to_string()).live_rollouts, n);
    }
}

/// Proves: `--live-rollouts` of 0, of 17, of a number too large for the field, of text and of a
/// negative number is refused as a usage error that names the flag and the value, and the
/// binary exits 2 for each.
/// Catches: 0 accepted (no service could ever roll), 17 or more accepted (the bound is 16), a
/// value that wraps into the range, and a refusal that exits 1 as a refused start does.
#[test]
fn live_rollouts_outside_one_to_sixteen_or_not_a_number_is_a_usage_error() {
    let files = Files::new("live-rollouts-refused");
    for value in ["0", "17", "18446744073709551616", "many", "-1", "2.5", ""] {
        // `=` keeps a value that starts with a dash from being read as another flag.
        let args = files.args(&[&format!("--live-rollouts={value}")]);
        let refusal = Cli::try_parse_from(args.clone()).expect_err(value);
        assert_eq!(refusal.kind(), ErrorKind::ValueValidation, "{value:?}");
        assert_eq!(refusal.exit_code(), 2, "{value:?}");
        let text = refusal.to_string();
        assert!(
            text.contains("--live-rollouts") && text.contains(&format!("'{value}'")),
            "{value:?}: {text}"
        );
        assert_eq!(
            hostkeeper_coordinator::config::main(args),
            ExitCode::from(2),
            "{value:?}"
        );
    }
}

/// Proves: the usage of `serve` names `--live-rollouts` and says what it counts.
/// Catches: a flag that works and is not in the help.
#[test]
fn the_help_names_live_rollouts() {
    let help =
        Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"]).expect_err("help");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let text = help.to_string();
    assert!(text.contains("--live-rollouts"), "{text}");
    assert!(
        text.contains("services may have a live rollout at once"),
        "{text}"
    );
}

/// Proves: a coordinator is built from `--live-rollouts 2` and from the ends of the range, 1 and
/// 16, and the flag does not change what else the start checks.
/// Catches: a flag that parses and stops the start, or one the build refuses outside the default.
/// The coordinator's number is not readable from outside it and its clock cannot pass the quiet
/// start here, so what it does with the number is proved in `src/plan_status_tests.rs`.
#[test]
fn a_coordinator_is_built_with_one_two_and_sixteen_live_rollouts() {
    for (n, name) in [
        ("2", "live-rollouts-build-two"),
        ("1", "live-rollouts-build-one"),
        ("16", "live-rollouts-build-sixteen"),
    ] {
        let files = Files::new(name);
        let flags = flags_with(&files, n);
        assert_eq!(flags.live_rollouts.to_string(), n);
        build_given(&flags).unwrap_or_else(|why| panic!("--live-rollouts {n}: {why}"));
    }
}
