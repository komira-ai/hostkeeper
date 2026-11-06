//! `serve --release-channel`: the release channel a set of the desired state is checked
//! against. Only an `https://` base URL starts; its two deadlines are flags of their own that
//! need it. Nothing here fetches: a start reads the URL and fetches only when a set is made,
//! which these tests do not make.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_coordinator::config::{Cli, Command, Serve, StartError, build, main};
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_testkit::Scratch;
use rustls::pki_types::CertificateDer;
use support::{Ca, OPERATOR, recorded, scratch};

/// The files a start reads, in a directory of their own, and the arguments that name them.
struct Files {
    dir: Scratch,
}

impl Files {
    fn new(name: &str) -> Self {
        let files = Self { dir: scratch(name) };
        let leaf = Ca::new("channel CA").coordinator();
        for (file, text, mode) in [
            ("tls.pem", leaf.cert_pem.as_str(), 0o644),
            ("tls.key", leaf.key_pem.as_str(), 0o600),
            ("ca.pem", &Ca::new("clients CA").pem, 0o644),
            ("oidc.toml", &config_text(STATIC_ISSUER), 0o644),
            ("operator.token", OPERATOR, 0o600),
            ("rollout.toml", &recorded("services.toml"), 0o600),
        ] {
            let path = files.dir.path().join(file);
            std::fs::write(&path, text).expect("written");
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(mode))
                .expect("mode");
        }
        files
    }

    /// The arguments of a `serve` over these files, followed by `extra`.
    fn args(&self, extra: &[&str]) -> Vec<OsString> {
        let path = |file: &str| self.dir.path().join(file).into_os_string();
        let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        for (flag, value) in [
            ("--heartbeat-listen", "192.0.2.1:1".into()),
            ("--private-listen", "192.0.2.1:2".into()),
            ("--tls-cert", path("tls.pem")),
            ("--tls-key", path("tls.key")),
            ("--client-ca", path("ca.pem")),
            ("--state-file", path("desired.json")),
            ("--oidc-config", path("oidc.toml")),
            ("--operator-token-file", path("operator.token")),
            ("--rollout-file", path("rollout.toml")),
            ("--legacy-service", "kbf".into()),
        ] {
            args.push(flag.into());
            args.push(value);
        }
        args.extend(extra.iter().map(OsString::from));
        args
    }
}

fn flags_of(args: Vec<OsString>) -> Serve {
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("not serve");
    };
    flags
}

/// The roots of a CA of the test, so that no start reads the host's certificate store.
fn roots() -> Roots {
    Roots::Given(vec![CertificateDer::from(Ca::new("channel roots").der)])
}

/// Catches: a release channel of another scheme than `https://` started on (every set would
/// then be checked over plain HTTP), or one with credentials or a query taken: the start is
/// refused, exit 1, with the flag and the URL named.
#[test]
fn a_release_channel_that_is_not_https_stops_the_start() {
    let files = Files::new("channel-not-https");
    for url in [
        "http://channel.example",
        "ftp://channel.example/releases",
        "channel.example",
        "https://user@channel.example",
        "https://channel.example/?v=1",
    ] {
        let args = files.args(&["--release-channel", url]);
        let Err(why) = build(&flags_of(args.clone()), roots()) else {
            panic!("{url} was taken");
        };
        assert!(matches!(why, StartError::ReleaseChannel(_)), "{url}: {why}");
        let text = why.to_string();
        assert!(
            text.starts_with(&format!("--release-channel: {url:?} ")),
            "{text}"
        );
        assert_eq!(main(args), ExitCode::from(1), "{url}");
    }
    let why = build(
        &flags_of(files.args(&["--release-channel", "http://channel.example"])),
        roots(),
    )
    .err()
    .expect("refused");
    assert_eq!(
        why.to_string(),
        "--release-channel: \"http://channel.example\" is not an https:// URL: the release \
         channel is fetched over HTTPS only"
    );
}

/// Catches: an `https://` channel refused, the start fetching from it, or the deadlines'
/// defaults lost: a start with the channel builds, with or without a base path, and without
/// the flag nothing about the channel is set.
#[test]
fn an_https_release_channel_starts_with_its_deadlines() {
    let files = Files::new("channel-https");
    for url in [
        "https://channel.example",
        "https://channel.example:8443/releases/",
    ] {
        let flags = flags_of(files.args(&["--release-channel", url]));
        assert_eq!(flags.release_channel.as_deref(), Some(url));
        assert_eq!(
            (
                flags.release_channel_connect_secs,
                flags.release_channel_timeout_secs
            ),
            (10, 30)
        );
        let parts = build(&flags, roots()).expect("the start");
        drop(parts);
    }
    let flags = flags_of(files.args(&[
        "--release-channel",
        "https://channel.example",
        "--release-channel-connect-secs",
        "3",
        "--release-channel-timeout-secs",
        "7",
    ]));
    assert_eq!(
        (
            flags.release_channel_connect_secs,
            flags.release_channel_timeout_secs
        ),
        (3, 7)
    );
    let unset = flags_of(files.args(&[]));
    assert_eq!(unset.release_channel, None);
    build(&unset, roots()).expect("the start without a channel");
}

/// Catches: a deadline of zero taken (every fetch would fail at once), or a deadline given
/// without a channel taken silently: both are usage errors, exit 2.
#[test]
fn a_deadline_of_zero_or_without_a_channel_is_a_usage_error() {
    let files = Files::new("channel-deadlines");
    for flag in [
        "--release-channel-connect-secs",
        "--release-channel-timeout-secs",
    ] {
        let zero = files.args(&["--release-channel", "https://channel.example", flag, "0"]);
        let usage = Cli::try_parse_from(zero.clone()).expect_err("zero");
        assert_eq!(usage.kind(), ErrorKind::ValueValidation, "{flag}");
        assert_eq!(main(zero), ExitCode::from(2), "{flag}");
        let alone = files.args(&[flag, "5"]);
        let usage = Cli::try_parse_from(alone.clone()).expect_err("without a channel");
        assert_eq!(usage.kind(), ErrorKind::MissingRequiredArgument, "{flag}");
        assert_eq!(main(alone), ExitCode::from(2), "{flag}");
    }
}

/// Catches: the flag missing from the usage, or read from the environment.
#[test]
fn the_release_channel_flags_are_documented_and_not_environment_variables() {
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    for flag in [
        "--release-channel <RELEASE_CHANNEL>",
        "--release-channel-connect-secs <RELEASE_CHANNEL_CONNECT_SECS>",
        "--release-channel-timeout-secs <RELEASE_CHANNEL_TIMEOUT_SECS>",
        "not_in_channel",
        "channel_digest_mismatch",
    ] {
        assert!(help.contains(flag), "{flag} is missing from {help}");
    }
    assert!(!help.contains("[env:"), "{help}");
}
