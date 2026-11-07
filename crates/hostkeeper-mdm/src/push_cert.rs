//! `hostkeeper-mdm push-cert`: NanoHUB given the APNs push certificate and its key, and the
//! enrollment profile the proxy serves at `/enroll` written, as root.
//!
//! It is root's even for a dry run (it reads root-only files). It refuses, before anything
//! changes, when `MDM_STATE/www` is not a directory (`install` makes it).
//!
//! With `--cert PEM --key PEM` it checks the pair first and refuses: a file inside a git work
//! tree (keys are kept outside every repository); a missing file; a key file whose mode is not
//! 0600; a certificate that is not a PEM X.509 certificate, whose subject UID is not a push
//! topic (`com.apple.mgmt.External.<uuid>`), or that has expired by the injected clock
//! (`not_after` at or before now); a key that is encrypted or is not a PEM private key; a key
//! that is not the certificate's (their SubjectPublicKeyInfo differ). The topic and expiry are
//! printed (public data); key material never is.
//!
//! Two records, `MDM_STATE/push-topic` and `MDM_STATE/push-cert.sha256` (the sha256 of the
//! certificate's PEM bytes), root's, 0644, one line each, say what NanoHUB was given. When
//! both already equal this certificate's, nothing is uploaded. Otherwise, with `--apply`, the
//! certificate PEM followed by the key PEM is `PUT` to NanoHUB's `/api/v1/nanomdm/pushcert`
//! on loopback ([`crate::api`]; a dry run prints the call and makes none), and the topic in
//! its JSON answer must be the certificate's. Only then are the records written: a refused or
//! mismatched upload leaves them as they were.
//!
//! With `--profile-only` there is no certificate: the topic is `--topic`, else the recorded
//! `MDM_STATE/push-topic` (refused when missing or not a topic).
//!
//! Either way the enrollment profile (`hostkeeper_mdm_core::profile::enrollment`: the topic,
//! `MDM_DNS_NAME`, `MDM_ORG` and the SCEP challenge read from `MDM_ETC/scep.env`) is written to
//! `MDM_STATE/www/enroll.mobileconfig`, mode 0640, owned by root with the group `MDM_USER`:
//! the proxy runs as `MDM_USER` and serves it at `https://<MDM_DNS_NAME>/enroll`, and the
//! challenge in it is readable by the service group only. The service user must exist. The
//! challenge is in the file and never printed. A rerun with nothing new plans nothing: the
//! payload UUIDs are fixed.

use std::io::Write;

use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::profile::{self, Challenge, ProfileError, Topic};
use hostkeeper_mdm_step::{Account, Host, Kind, Mode, Planner, execute, ids};
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;
use sha2::{Digest, Sha256};

use crate::api::{self, Api, Call};
use crate::{Env, MdmError, secret};

/// The subject attribute an APNs certificate names its topic in: `UID`.
const UID_OID: &str = "0.9.2342.19200300.100.1.1";

/// Where the topic comes from.
pub enum With {
    /// A push certificate and its key (PEM files), given to NanoHUB.
    Pair {
        /// The certificate.
        cert: String,
        /// Its private key.
        key: String,
    },
    /// `--profile-only`: the profile alone, for `--topic` or the recorded topic.
    ProfileOnly(Option<Topic>),
}

/// What `push-cert` was asked for.
pub struct PushCert {
    /// The settings.
    pub config: MdmConfig,
    /// Where the topic comes from.
    pub with: With,
}

/// Why `push-cert` refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PushCertError {
    /// `MDM_STATE/www` is not a directory.
    #[error("{0} is not a directory: run hostkeeper-mdm install --apply first")]
    NotInstalled(String),
    /// The key file is not mode 0600.
    #[error("{path} is mode {mode:04o}, not 0600: chmod 0600 it (it holds a private key)")]
    KeyMode {
        /// The key file.
        path: String,
        /// Its mode (0 when it could not be looked at).
        mode: u32,
    },
    /// The certificate file is not a PEM certificate.
    #[error("{0} is not a PEM certificate")]
    NotCertificate(String),
    /// The certificate is not an MDM push certificate.
    #[error(
        "{cert} is not an MDM push certificate: its subject UID is {uid:?}, not com.apple.mgmt.External.<uuid>"
    )]
    NotPush {
        /// The certificate file.
        cert: String,
        /// Its subject UID ("" when it has none).
        uid: String,
    },
    /// The certificate has expired.
    #[error("{cert} expired at {not_after}: renew it at the Apple Push Certificates Portal")]
    Expired {
        /// The certificate file.
        cert: String,
        /// Its end of validity.
        not_after: String,
    },
    /// The key file is not a plain PEM private key.
    #[error("{0} is encrypted or not a PEM private key (NanoHUB needs it unencrypted)")]
    KeyUnusable(String),
    /// The key is another certificate's.
    #[error("{key} is not the key of {cert}")]
    NotItsKey {
        /// The key file.
        key: String,
        /// The certificate file.
        cert: String,
    },
    /// `--profile-only` and no topic is recorded.
    #[error(
        "{0} is not there: run push-cert --cert PEM --key PEM first, or give --topic with --profile-only"
    )]
    NoTopic(String),
    /// The profile could not be made: the recorded topic or the SCEP challenge is malformed.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// The service user or its group is not there.
    #[error("the user and group {0} are not there: run hostkeeper-mdm install --apply first")]
    NoUser(String),
    /// NanoHUB stored another topic than the certificate's.
    #[error(
        "NanoHUB answered topic {got:?} for a certificate whose topic is {want}: the records were not written"
    )]
    TopicDiffers {
        /// The topic in the answer ("" when there was none).
        got: String,
        /// The certificate's.
        want: String,
    },
}

impl PushCertError {
    /// The exit code: 1 when a call or a step failed, 2 when the input was refused.
    pub fn exit_code(&self) -> u8 {
        match self {
            PushCertError::TopicDiffers { .. } => 1,
            _ => 2,
        }
    }
}

/// A checked push certificate: its topic, expiry and sha256.
#[derive(Debug, PartialEq, Eq)]
pub struct Checked {
    /// The topic (the subject UID).
    pub topic: Topic,
    /// The end of validity, as text.
    pub not_after: String,
    /// The sha256 of the certificate's PEM bytes, as hex.
    pub sha256: String,
}

/// Check that `cert_pem` is a push certificate not expired at `now` (unix seconds) and
/// `key_pem` its plain private key (`cert` and `key` name the files in errors).
///
/// # Errors
/// [`PushCertError`]: the certificate or the key was refused.
pub fn check_pair(
    cert: &str,
    cert_pem: &[u8],
    key: &str,
    key_pem: &[u8],
    now: u64,
) -> Result<Checked, PushCertError> {
    let not_cert = || PushCertError::NotCertificate(cert.to_owned());
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem).map_err(|_| not_cert())?;
    let parsed = pem.parse_x509().map_err(|_| not_cert())?;
    let uid = parsed
        .subject()
        .iter_attributes()
        .find(|a| a.attr_type().to_id_string() == UID_OID)
        .and_then(|a| a.as_str().ok())
        .unwrap_or_default();
    let topic = Topic::new(uid).map_err(|_| PushCertError::NotPush {
        cert: cert.to_owned(),
        uid: uid.to_owned(),
    })?;
    let not_after = parsed.validity().not_after;
    // A date before 1970 is expired too.
    if !u64::try_from(not_after.timestamp()).is_ok_and(|end| end > now) {
        return Err(PushCertError::Expired {
            cert: cert.to_owned(),
            not_after: not_after.to_string(),
        });
    }
    let bad_key = || PushCertError::KeyUnusable(key.to_owned());
    let der = PrivateKeyDer::from_pem_slice(key_pem).map_err(|_| bad_key())?;
    let signer = rustls::crypto::ring::sign::any_supported_type(&der).map_err(|_| bad_key())?;
    let same = signer
        .public_key()
        .is_some_and(|public| public.as_ref() == parsed.public_key().raw);
    if !same {
        return Err(PushCertError::NotItsKey {
            key: key.to_owned(),
            cert: cert.to_owned(),
        });
    }
    Ok(Checked {
        topic,
        not_after: not_after.to_string(),
        sha256: hex::encode(Sha256::digest(cert_pem)),
    })
}

/// Check, then print or carry out the upload of the push certificate and the profile.
///
/// # Errors
/// [`MdmError`].
pub fn run(env: &Env<'_>, a: &PushCert, mode: Mode, out: &mut dyn Write) -> Result<(), MdmError> {
    let host = env.host;
    let c = &a.config;
    let euid = host.euid();
    if euid != 0 {
        return Err(MdmError::NeedsRoot {
            verb: "push-cert",
            euid,
        });
    }
    let www = format!("{}/www", c.state());
    if host.stat(&www)?.map(|m| m.kind) != Some(Kind::Dir) {
        return Err(PushCertError::NotInstalled(www).into());
    }
    let topic_file = format!("{}/push-topic", c.state());
    let sum_file = format!("{}/push-cert.sha256", c.state());
    let root = Account::root();
    let mut p = Planner::new(host);
    let mut upload = None;
    let topic = match &a.with {
        With::Pair { cert, key } => {
            let (cert_pem, key_pem) = pair_files(host, cert, key)?;
            let checked = check_pair(cert, &cert_pem, key, &key_pem, env.now)?;
            note(
                out,
                format!(
                    "push certificate: topic {}, valid until {} (renew it before then at the Apple Push Certificates Portal, with the same Apple Account)",
                    checked.topic, checked.not_after
                ),
            )?;
            let topic_line = format!("{}\n", checked.topic);
            let sum_line = format!("{}\n", checked.sha256);
            // Both records are planned, so neither short-circuits the other.
            let topic_changes = p.file(&topic_file, 0o644, &root, topic_line.as_bytes())?;
            let sum_changes = p.file(&sum_file, 0o644, &root, sum_line.as_bytes())?;
            if topic_changes || sum_changes {
                let mut body = cert_pem;
                body.extend_from_slice(&key_pem);
                upload = Some(body);
            } else {
                note(
                    out,
                    "NanoHUB: has this certificate already (push-cert.sha256)".to_owned(),
                )?;
            }
            checked.topic
        }
        With::ProfileOnly(Some(topic)) => topic.clone(),
        With::ProfileOnly(None) => {
            let recorded = host
                .read(&topic_file)?
                .ok_or_else(|| PushCertError::NoTopic(topic_file.clone()))?;
            Topic::new(String::from_utf8_lossy(&recorded).trim()).map_err(PushCertError::from)?
        }
    };
    let scep_env = format!("{}/scep.env", c.etc());
    let secret = secret::read(host, &scep_env, "SCEP_CHALLENGE_PASSWORD")?;
    let challenge = Challenge::new(secret.expose()).map_err(PushCertError::from)?;
    let service = Account::new(c.user(), c.user());
    if ids(host, &service)?.is_none() {
        return Err(PushCertError::NoUser(c.user().to_owned()).into());
    }
    let bytes = profile::enrollment(c, &topic, &challenge).map_err(PushCertError::from)?;
    // The proxy runs as MDM_USER and reads the profile through the group.
    let group = Account::new("root", c.user());
    p.file(&format!("{www}/enroll.mobileconfig"), 0o640, &group, &bytes)?;
    let name = c.dns_name();
    note(
        out,
        format!(
            "profile: ServerURL https://{name}/mdm, SCEP https://{name}/scep, topic {topic}; served at https://{name}/enroll (the challenge is in it, not shown)"
        ),
    )?;
    let plan = p.finish();
    if let Some(body) = upload {
        let nanohub = Api::nanohub(env, c)?;
        let put = Call::put("/api/v1/nanomdm/pushcert", body);
        if let Some(answer) = api::call(env, &nanohub, &put, mode, out)? {
            let got = stored_topic(&answer);
            if got != topic.as_str() {
                return Err(PushCertError::TopicDiffers {
                    got,
                    want: topic.to_string(),
                }
                .into());
            }
            writeln!(out, "NanoHUB: push certificate stored for {topic}")?;
        }
    }
    Ok(execute(&plan, mode, host, out)?)
}

/// One line of output.
fn note(out: &mut dyn Write, line: String) -> Result<(), MdmError> {
    Ok(writeln!(out, "{line}")?)
}

/// The `topic` in NanoHUB's JSON answer; empty when there is none.
fn stored_topic(answer: &[u8]) -> String {
    let value: Option<serde_json::Value> = serde_json::from_slice(answer).ok();
    value
        .as_ref()
        .and_then(|v| v.get("topic")?.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The certificate and the key, each refused inside a git work tree; the key also unless it
/// is mode 0600.
fn pair_files(host: &dyn Host, cert: &str, key: &str) -> Result<(Vec<u8>, Vec<u8>), MdmError> {
    crate::repo::file_outside_git(host, cert)?;
    crate::repo::file_outside_git(host, key)?;
    let cert_pem = host
        .read(cert)?
        .ok_or_else(|| MdmError::NoFile(cert.to_owned()))?;
    let key_pem = host
        .read(key)?
        .ok_or_else(|| MdmError::NoFile(key.to_owned()))?;
    let mode = host.stat(key)?.map_or(0, |m| m.mode);
    if mode != 0o600 {
        return Err(PushCertError::KeyMode {
            path: key.to_owned(),
            mode,
        }
        .into());
    }
    Ok((cert_pem, key_pem))
}
