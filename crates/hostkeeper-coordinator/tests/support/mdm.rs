//! The files and flags of a `serve` with the MDM policy gate that the start tests of
//! `tests/mdm.rs` and `tests/mdm_baseline.rs` share: the files the flags name, the enrollment and
//! gate flags over them, and the calls that build a coordinator from them or say why it is
//! refused.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use clap::Parser;
use hostkeeper_coordinator::config::{Cli, Command, Parts, Serve, StartError, build};
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use rustls::pki_types::CertificateDer;

use super::{Ca, OPERATOR, scratch};

/// The URL of NanoHUB's API: loopback, and nothing listens there.
pub const API_URL: &str = "http://127.0.0.1:9004";

/// Where the webhook is served.
pub const WEBHOOK: &str = "127.0.0.1:9100";

/// The files the flags name, in a directory of their own, and the CA of the private listener.
pub struct Files {
    pub dir: Scratch,
    pub private_ca: Ca,
}

impl Files {
    pub fn new(name: &str) -> Self {
        let files = Self {
            dir: scratch(name),
            private_ca: Ca::new("private listener CA"),
        };
        let leaf = files.private_ca.coordinator();
        files.put("tls.pem", &leaf.cert_pem, 0o644);
        files.put("tls.key", &leaf.key_pem, 0o600);
        files.put("ca.pem", &Ca::new("farm CA").pem, 0o644);
        files.put("oidc.toml", &config_text(STATIC_ISSUER), 0o644);
        files.put("operator.token", OPERATOR, 0o600);
        files.put("mdm.key", "a-nanohub-api-key-for-the-tests\n", 0o600);
        files
    }

    pub fn put(&self, file: &str, text: &str, mode: u32) {
        let path = self.path(file);
        std::fs::write(&path, text).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.dir.path().join(file)
    }

    /// The arguments of a `serve`, then `extra`; each `=name` is the path of `name` in the
    /// directory.
    pub fn args(&self, extra: &[&str]) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        let base = [
            "--heartbeat-listen",
            "192.0.2.1:1",
            "--private-listen",
            "192.0.2.1:2",
            "--tls-cert",
            "=tls.pem",
            "--tls-key",
            "=tls.key",
            "--client-ca",
            "=ca.pem",
            "--state-file",
            "=desired.json",
            "--oidc-config",
            "=oidc.toml",
            "--operator-token-file",
            "=operator.token",
            "--legacy-service",
            "kbf",
        ];
        for arg in base.iter().chain(extra) {
            args.push(match arg.strip_prefix('=') {
                Some(file) => self.path(file).into_os_string(),
                None => OsString::from(arg),
            });
        }
        args
    }
}

/// The enrollment flags, which make the CA.
pub fn enrollment() -> Vec<&'static str> {
    vec![
        "--enrollment-dir",
        "=enrollment",
        "--ca-key-file",
        "=ca.key",
        "--server-cert-file",
        "=server.pem",
        "--server-name",
        "localhost",
        "--ca-init",
    ]
}

/// The enrollment flags and the three gate flags, with `url`, `key` and `listen` for them.
pub fn gate(url: &'static str, key: &'static str, listen: &'static str) -> Vec<&'static str> {
    let mut flags = enrollment();
    flags.extend(["--mdm-api-url", url, "--mdm-api-key-file", key]);
    flags.extend(["--mdm-webhook-listen", listen]);
    flags
}

/// The good gate flags.
pub fn good() -> Vec<&'static str> {
    gate(API_URL, "=mdm.key", WEBHOOK)
}

pub fn flags_of(args: Vec<OsString>) -> Serve {
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("serve")
    };
    flags
}

pub fn start(files: &Files, extra: &[&str]) -> Result<Parts, StartError> {
    let roots = Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)]);
    build(&flags_of(files.args(extra)), roots)
}

/// The text of why a start was refused.
pub fn refusal(files: &Files, extra: &[&str]) -> String {
    match start(files, extra) {
        Err(why) => why.to_string(),
        Ok(_) => panic!("started with {extra:?}"),
    }
}

/// The farm's profile identifier in these tests.
pub const IDENTIFIER: &str = "com.example.hostkeeper.mdm";

/// A daemon config the daemon's loader accepts.
pub const DAEMON_CONFIG: &str = r#"coordinator = "https://coordinator.example:8443"
ca_sha256 = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// Puts the host group `build` into the state files of `files`, through `HostGroups.Put` of a
/// coordinator started over them, which is stopped again.
pub async fn put_build_group(files: &Files) {
    let roots = Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)]);
    let first = build(&flags_of(files.args(&enrollment())), roots).expect("started");
    first
        .coordinator
        .host_groups_put(
            &first.coordinator.caller(Some(OPERATOR)).await,
            proto::PutHostGroupRequest {
                host_group: Some(proto::HostGroup {
                    name: "build".to_owned(),
                    packages: vec!["hostkeeper".to_owned()],
                    policy: None,
                }),
                if_generation: None,
                reason: "the Mac builders".to_owned(),
            },
        )
        .expect("put");
    drop(first);
}
