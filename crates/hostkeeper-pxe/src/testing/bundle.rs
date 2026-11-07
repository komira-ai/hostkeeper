//! Synthetic attestation bundles, as `hostkeeper-package`'s tests make them, and a fake `gh`.
//!
//! A bundle is unsigned: a certificate that holds only the Fulcio extensions the bundle reader
//! and the fake `gh` read (the build signer URI, the runner, the commit, the ref and the owner
//! URI), and an in-toto statement naming one subject and one commit. The fake `gh`
//! ([`FakeGh`]) is the attestation port of the daemon's real verifier, `GhVerifier`: it applies
//! the flags `GhVerifier` passes to the bundle's certificate, as `gh attestation verify` would.

use std::cell::Cell;
use std::io;
use std::path::Path;
use std::rc::Rc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::daemon::TrustedSigners;
use hostkeeper_config::trust::TrustError;
use hostkeeper_package::gh::{FileTrust, GhVerifier, Ran, Runner, Spawn};
use regex::Regex;
use serde_json::Value;

use hostkeeper_steps::Clock;

use super::{FakeClock, sha256_hex};

/// The organisation, workflow and ref the tests trust.
pub(crate) const ORGANISATION: &str = "example-org";
pub(crate) const WORKFLOW: &str = ".github/workflows/artifacts.yml";
pub(crate) const MAIN: &str = "refs/heads/main";
pub(crate) const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The trusted signer of the tests.
pub(crate) fn signers() -> TrustedSigners {
    TrustedSigners {
        organisation: ORGANISATION.to_owned(),
        workflow: WORKFLOW.to_owned(),
        refs: vec![MAIN.to_owned()],
        hosted_runner: true,
    }
}

/// Who made a bundle, and what it attests.
#[derive(Clone, Debug)]
pub(crate) struct Signer {
    pub organisation: String,
    pub workflow: String,
    pub source_ref: String,
}

impl Signer {
    /// The trusted signer.
    pub(crate) fn trusted() -> Signer {
        Signer {
            organisation: ORGANISATION.to_owned(),
            workflow: WORKFLOW.to_owned(),
            source_ref: MAIN.to_owned(),
        }
    }
}

const FULCIO: [u8; 9] = [0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xbf, 0x30, 0x01];

/// A DER element: the short form of its length below 128, the two-byte long form otherwise
/// (which the bundle reader takes for any length).
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let length = u16::try_from(content.len()).expect("under 64 KiB");
    let mut out = match u8::try_from(length).ok().filter(|short| *short < 0x80) {
        Some(short) => vec![tag, short],
        None => [[tag, 0x82].as_slice(), &length.to_be_bytes()].concat(),
    };
    out.extend_from_slice(content);
    out
}

/// One Fulcio extension whose value is the UTF8String `value`.
fn extension(arc: u8, value: &str) -> Vec<u8> {
    let mut oid = FULCIO.to_vec();
    oid.push(arc);
    let mut body = tlv(0x06, &oid);
    body.extend(tlv(0x04, &tlv(0x0c, value.as_bytes())));
    tlv(0x30, &body)
}

/// The certificate of `signer`'s bundle: a `tbsCertificate` with a version, a serial and the
/// Fulcio extensions. No key, no signature.
fn certificate(signer: &Signer) -> Vec<u8> {
    let uri = format!(
        "https://github.com/{}/tool/{}@{}",
        signer.organisation, signer.workflow, signer.source_ref
    );
    let owner = format!("https://github.com/{}", signer.organisation);
    let mut extensions = Vec::new();
    for (arc, value) in [
        (9, uri.as_str()),
        (11, "github-hosted"),
        (13, COMMIT),
        (14, signer.source_ref.as_str()),
        (16, owner.as_str()),
    ] {
        extensions.extend(extension(arc, value));
    }
    let mut tbs = tlv(0xa0, &tlv(0x02, &[0x02]));
    tbs.extend(tlv(0x02, &[0x01]));
    tbs.extend(tlv(0xa3, &tlv(0x30, &extensions)));
    tlv(0x30, &tlv(0x30, &tbs))
}

/// The bundle of `signer` attesting the bytes whose SHA-256 is `subject`.
pub(crate) fn bundle(signer: &Signer, subject: &str) -> Vec<u8> {
    let statement = format!(
        "{{\"_type\":\"https://in-toto.io/Statement/v1\",\
         \"subject\":[{{\"name\":\"asset\",\"digest\":{{\"sha256\":\"{subject}\"}}}}],\
         \"predicateType\":\"https://slsa.dev/provenance/v1\",\
         \"predicate\":{{\"buildDefinition\":{{\"resolvedDependencies\":\
         [{{\"uri\":\"git+https://github.com/{}/tool@{}\",\"digest\":{{\"gitCommit\":\"{COMMIT}\"}}}}]}}}}}}",
        signer.organisation, signer.source_ref
    );
    format!(
        "{{\"mediaType\":\"application/vnd.dev.sigstore.bundle.v0.3+json\",\
         \"verificationMaterial\":{{\"certificate\":{{\"rawBytes\":\"{}\"}}}},\
         \"dsseEnvelope\":{{\"payload\":\"{}\",\
         \"payloadType\":\"application/vnd.in-toto+json\",\"signatures\":[]}}}}\n",
        STANDARD.encode(certificate(signer)),
        STANDARD.encode(statement)
    )
    .into_bytes()
}

/// The UTF8String values of a DER certificate that are URIs of github.com.
fn uris(der: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    for at in 0..der.len().saturating_sub(2) {
        let length = usize::from(der[at + 1]);
        let value = der.get(at + 2..at + 2 + length).unwrap_or_default();
        let text = String::from_utf8_lossy(value);
        if (der[at] == 0x0c) & text.starts_with("https://github.com/") {
            found.push(text.into_owned());
        }
    }
    found
}

/// How the fake `gh` behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GhMode {
    /// Applies its flags to the bundle, as `gh` does.
    Checks,
    /// Cannot be started.
    Missing,
    /// Answers yes, after moving the clock past every deadline.
    Slow,
}

/// The fake `gh`: checks `--owner` against the certificate's owner URI, `--cert-identity-regex`
/// against its build signer URI, and that the statement lists the archive's SHA-256. Each check
/// that fails ends it with status 1 and a line naming the flag. It counts its runs.
pub(crate) struct FakeGh {
    pub mode: GhMode,
    pub runs: Rc<Cell<u32>>,
    pub clock: Rc<FakeClock>,
}

impl Runner for FakeGh {
    fn run(&self, spawn: &Spawn) -> io::Result<Ran> {
        self.runs.set(self.runs.get() + 1);
        match self.mode {
            GhMode::Missing => return Err(io::Error::from(io::ErrorKind::NotFound)),
            GhMode::Slow => {
                self.clock.sleep_secs(1_000_000);
                return Ok(Ran {
                    code: Some(0),
                    ..Ran::default()
                });
            }
            GhMode::Checks => {}
        }
        let args: Vec<String> = spawn
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let value = |flag: &str| {
            let at = args.iter().position(|arg| arg == flag).expect("the flag");
            args[at + 1].clone()
        };
        let bundle: Value =
            serde_json::from_slice(&std::fs::read(value("--bundle"))?).expect("a JSON bundle");
        let text = |pointer: &str| {
            bundle
                .pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let der = STANDARD
            .decode(text("/verificationMaterial/certificate/rawBytes"))
            .expect("base64");
        let statement = String::from_utf8(
            STANDARD
                .decode(text("/dsseEnvelope/payload"))
                .expect("base64"),
        )
        .expect("a statement");
        let found = uris(&der);
        let signer_uri = found
            .iter()
            .find(|uri| uri.contains('@'))
            .cloned()
            .unwrap_or_default();
        let owner_uri = found
            .iter()
            .find(|uri| !uri.contains('@'))
            .cloned()
            .unwrap_or_default();
        let archive = std::fs::read(&args[2])?;
        let checks = [
            (
                "--owner",
                owner_uri == format!("https://github.com/{}", value("--owner")),
            ),
            (
                "--cert-identity-regex",
                Regex::new(&value("--cert-identity-regex"))
                    .expect("a regular expression")
                    .is_match(&signer_uri),
            ),
            (
                "the subject digest",
                statement.contains(&sha256_hex(&archive)),
            ),
        ];
        let failed = checks.iter().find(|(_, ok)| !ok);
        Ok(match failed {
            None => Ran {
                code: Some(0),
                ..Ran::default()
            },
            Some((flag, _)) => Ran {
                code: Some(1),
                timed_out: false,
                stderr: format!("gh: verification failed: {flag}\n").into_bytes(),
            },
        })
    }
}

/// A [`FileTrust`] that trusts every file: the tests' `gh` and trusted root are not files.
pub(crate) struct TrustAll;

impl FileTrust for TrustAll {
    fn check(&self, _path: &Path) -> Result<(), TrustError> {
        Ok(())
    }
}

/// The daemon's verifier over the fake `gh`, running in `home`.
pub(crate) fn verifier(gh: FakeGh, home: &Path) -> GhVerifier {
    GhVerifier::new(
        "/the-tests/gh".into(),
        "/the-tests/trusted_root.json".into(),
        home.to_owned(),
        Box::new(gh),
        Box::new(TrustAll),
    )
}
