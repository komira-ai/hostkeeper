//! The files and flags of a `serve` the start tests drive in process
//! (`config::{Cli, build, main}`): the TLS files, the OIDC allow-list, and the operator token in a
//! scratch directory, the recorded flags over them, and the calls that build a
//! coordinator from them or say why it is refused.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_coordinator::config::{Cli, Command, Parts, Serve, StartError, build};
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_testkit::Scratch;
use rustls::pki_types::CertificateDer;
use sha2::{Digest as _, Sha256};

use super::{Ca, OPERATOR, scratch};

/// The files the flags name, in a directory of their own.
pub struct Files {
    dir: Scratch,
}

impl Files {
    pub fn new(name: &str) -> Self {
        let dir = scratch(name);
        let leaf = Ca::new("config CA").coordinator();
        let files = Self { dir };
        files.put("tls.pem", &leaf.cert_pem, 0o644);
        files.put("tls.key", &leaf.key_pem, 0o600);
        files.put("ca.pem", &Ca::new("clients CA").pem, 0o644);
        files.put("oidc.toml", &config_text(STATIC_ISSUER), 0o644);
        files.put("operator.token", OPERATOR, 0o600);
        files
    }

    pub fn put(&self, file: &str, text: &str, mode: u32) {
        let path = self.dir.path().join(file);
        std::fs::write(&path, text).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.dir.path().join(file)
    }

    /// Writes `tokens.toml` (mode 0600) with a `[[token]]` for each `(name, token)`, holding
    /// the hash of the token; answers its path as text.
    pub fn tokens(&self, entries: &[(&str, &str)]) -> String {
        let text: String = entries
            .iter()
            .map(|(name, token)| {
                format!(
                    "[[token]]\nname = \"{name}\"\nsha256 = \"{}\"\n",
                    sha256_hex(token)
                )
            })
            .collect();
        self.put("tokens.toml", &text, 0o600);
        self.path("tokens.toml").display().to_string()
    }

    /// The arguments of a `serve` the recorded flags describe, kbf's namespaces included.
    pub fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        let pairs = [
            ("--heartbeat-listen", "192.0.2.1:1".to_owned()),
            ("--private-listen", "192.0.2.1:2".to_owned()),
            ("--tls-cert", self.path("tls.pem").display().to_string()),
            ("--tls-key", self.path("tls.key").display().to_string()),
            ("--client-ca", self.path("ca.pem").display().to_string()),
            (
                "--state-file",
                self.path("desired.json").display().to_string(),
            ),
            (
                "--oidc-config",
                self.path("oidc.toml").display().to_string(),
            ),
            (
                "--operator-token-file",
                self.path("operator.token").display().to_string(),
            ),
            ("--legacy-service", "kbf".to_owned()),
            ("--role-uri-prefix", "urn:kbf:role:".to_owned()),
            ("--host-role-uri-prefix", "urn:kbf:host-role:".to_owned()),
        ];
        for (flag, value) in pairs {
            args.push(flag.into());
            args.push(value.into());
        }
        args
    }
}

/// `args` with the value of `flag` replaced.
pub fn with(args: &[OsString], flag: &str, value: &str) -> Vec<OsString> {
    let mut args = args.to_vec();
    let at = args.iter().position(|a| a == flag).expect("a flag");
    args[at + 1] = value.into();
    args
}

/// `args` with every element equal to `from` spelled `to`.
pub fn renamed(args: &[OsString], from: &str, to: &str) -> Vec<OsString> {
    args.iter()
        .map(|a| {
            if a == from {
                OsString::from(to)
            } else {
                a.clone()
            }
        })
        .collect()
}

/// The SHA-256 of `text` as lowercase hex.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `args` without `flag` and its value.
pub fn without(args: &[OsString], flag: &str) -> Vec<OsString> {
    let mut args = args.to_vec();
    let at = args.iter().position(|a| a == flag).expect("a flag");
    args.drain(at..=at + 1);
    args
}

/// The roots the issuer's keys are fetched trusting: a CA of the test, so no test reads the
/// host's certificate store.
pub fn roots() -> Roots {
    Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)])
}

/// [`build`] trusting [`roots`].
pub fn build_given(flags: &Serve) -> Result<Parts, StartError> {
    build(flags, roots())
}

pub fn flags_of(args: Vec<OsString>) -> Serve {
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("serve")
    };
    flags
}

/// Why `build` refuses `args`.
pub fn refusal(args: Vec<OsString>) -> StartError {
    match build_given(&flags_of(args)) {
        Err(why) => why,
        Ok(_) => panic!("the start was not refused"),
    }
}

pub fn exit_code(args: Vec<OsString>) -> ExitCode {
    hostkeeper_coordinator::config::main(args)
}
