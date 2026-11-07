//! `hostkeeper-mdm dep`: NanoDEP linked to Apple Business Manager (ABM) and given the Automated
//! Device Enrollment (ADE) profile, as root. Each verb is one of the operator's ABM steps, in
//! this order. Every verb refuses a user who is not root, even for a dry run (the API key is
//! a root-only file), and refuses before any call when `MDM_ETC/nanodep.env` is not a 0600
//! file of root's holding `NANODEP_API`. Every call goes to depserver on loopback
//! ([`crate::api`]) with that key, is printed, and is made only with `--apply`.
//!
//! - `get-cert --out FILE`: refuses `FILE` inside a git work tree (a `.git` in it or in a
//!   directory above). When `FILE` holds a PEM certificate it is kept and nothing is asked:
//!   NanoDEP makes a new key pair on every call, so the certificate uploaded in ABM must be
//!   the last one made. When `FILE` holds anything else the verb refuses (exit 2). Otherwise
//!   it asks NanoDEP for the token certificate (`GET /v1/tokenpki/<MDM_DEP_NAME>` with the
//!   common name and validity of the settings); the answer must be a PEM certificate (else
//!   exit 1), written to `FILE` (root's, 0644). It prints the next step: upload it in ABM and
//!   download the server token (`.p7m`).
//! - `upload-token --token FILE`: refuses `FILE` inside a git work tree, or missing. It puts
//!   the token to NanoDEP (`PUT /v1/tokenpki/<MDM_DEP_NAME>`), asks Apple for the account
//!   (`GET /proxy/<MDM_DEP_NAME>/account`), prints its organisation and MDM server name (`?`
//!   for a missing field) as the proof, and says the token file may be deleted. Every run
//!   uploads.
//! - `define-profile`: the profile of [`AdeProfile`] is posted to Apple
//!   (`POST /proxy/<MDM_DEP_NAME>/profile`), the `profile_uuid` of the answer (16 to 64 hex
//!   digits, else exit 1) is made the assigner's (`PUT /v1/assigner/<MDM_DEP_NAME>`), and then
//!   `MDM_STATE/dep-profile-uuid` and `MDM_STATE/dep-profile.sha256` (root's, 0644) record the
//!   uuid and the sha256 of the profile; nothing is recorded when a call fails. A profile
//!   whose sha256 is the recorded one is not defined again and calls nothing. In the same
//!   plan the depsyncer service is enabled and started when it is not active: this is the
//!   verb that starts it, `install` leaves it alone.
//! - `assign --serial SERIAL...`: refuses (exit 2) when no profile is recorded; otherwise
//!   posts the recorded uuid and the serials to Apple
//!   (`POST /proxy/<MDM_DEP_NAME>/profile/devices`) and prints `<serial>: <status>` for each.
//!   The run returns `false`, which the command line turns into exit 1, unless Apple answered
//!   `SUCCESS` for every serial. Every run assigns; a dry run assigns nothing.

use std::io::Write;

use hostkeeper_mdm_core::commands::dep_cert_request_path;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::profile::{AdeProfile, ProfileError};
use hostkeeper_mdm_core::render::UnitNames;
use hostkeeper_mdm_step::{Account, DRY_RUN_NOTE, Host, Kind, Mode, Planner, execute};
use sha2::{Digest, Sha256};

use crate::api::{self, Api, Call};
use crate::{Env, MdmError};

/// What an operator does once the certificate is written.
const NEXT: &str = "upload it in Apple Business Manager (your MDM server, Upload Public Key), then download the server token (.p7m)";

/// A device serial number as ABM lists it: 8 to 14 upper-case letters and digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Serial(String);

impl Serial {
    /// `text` as a serial number.
    ///
    /// # Errors
    /// Not 8 to 14 upper-case letters and digits; the text says so.
    pub fn new(text: &str) -> Result<Serial, String> {
        let ok = (8..=14).contains(&text.len())
            && text
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        match ok {
            true => Ok(Serial(text.to_owned())),
            false => Err(format!(
                "{text:?} is not a serial number (8 to 14 upper-case letters and digits)"
            )),
        }
    }

    /// The serial number.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One `dep` verb.
pub enum Dep {
    /// NanoDEP's token certificate, to upload in ABM, written to `out`.
    GetCert {
        /// Where the certificate goes.
        out: String,
    },
    /// The server token ABM gives for that certificate, given to NanoDEP.
    UploadToken {
        /// The token file (`.p7m`).
        token: String,
    },
    /// The ADE profile defined at Apple and made the assigner's; depsyncer started.
    DefineProfile,
    /// The recorded ADE profile assigned to devices already in ABM.
    Assign {
        /// The devices.
        serials: Vec<Serial>,
    },
}

/// Why `dep` refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum DepError {
    /// The file `get-cert` was to write holds something that is not a certificate.
    #[error(
        "{0} is there and is not a PEM certificate: move it aside, and run get-cert again to make a new one"
    )]
    NotCertificate(String),
    /// An answer of a service is not what the verb needs.
    #[error("{what}: {why}")]
    Answer {
        /// What was asked.
        what: &'static str,
        /// What is wrong with the answer.
        why: String,
    },
    /// The server token file given to `upload-token` cannot be a token.
    #[error("{path} is {why}: give the server token (.p7m) Apple Business Manager gave")]
    Token {
        /// The file.
        path: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// `assign` found no recorded profile.
    #[error("{0} is not there: run hostkeeper-mdm dep define-profile --apply first")]
    NoProfile(String),
    /// The profile could not be written out.
    #[error(transparent)]
    Profile(#[from] ProfileError),
}

impl DepError {
    /// The exit code: 1 when a call or a step failed, 2 when the input was refused.
    pub fn exit_code(&self) -> u8 {
        match self {
            DepError::NotCertificate(_) | DepError::NoProfile(_) | DepError::Token { .. } => 2,
            DepError::Answer { .. } | DepError::Profile(_) => 1,
        }
    }
}

/// Check, then print or carry out `d`. `Ok(false)` when Apple did not assign every device.
///
/// # Errors
/// [`MdmError`].
pub fn run(
    env: &Env<'_>,
    c: &MdmConfig,
    d: &Dep,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<bool, MdmError> {
    let euid = env.host.euid();
    if euid != 0 {
        return Err(MdmError::NeedsRoot { verb: "dep", euid });
    }
    let api = Api::nanodep(env, c)?;
    match d {
        Dep::GetCert { out: file } => get_cert(env, &api, c, file, mode, out)?,
        Dep::UploadToken { token } => upload_token(env, &api, c, token, mode, out)?,
        Dep::DefineProfile => define_profile(env, &api, c, mode, out)?,
        Dep::Assign { serials } => return assign(env, &api, c, serials, mode, out),
    }
    Ok(true)
}

fn is_certificate(bytes: &[u8]) -> bool {
    x509_parser::pem::parse_x509_pem(bytes).is_ok_and(|(_, pem)| pem.parse_x509().is_ok())
}

fn get_cert(
    env: &Env<'_>,
    api: &Api,
    c: &MdmConfig,
    file: &str,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let host = env.host;
    crate::repo::file_outside_git(host, file)?;
    if host.stat(file)?.is_some() {
        let held = host.read(file)?.filter(|bytes| is_certificate(bytes));
        if held.is_none() {
            return Err(DepError::NotCertificate(file.to_owned()).into());
        }
        writeln!(out, "{file}: holds a certificate, kept; {NEXT}")?;
        return Ok(());
    }
    let get = Call::get(dep_cert_request_path(c));
    let Some(pem) = api::call(env, api, &get, mode, out)? else {
        writeln!(
            out,
            "DRY-RUN: write {file} (0644, the certificate NanoDEP answers)"
        )?;
        writeln!(out, "{DRY_RUN_NOTE}")?;
        return Ok(());
    };
    if !is_certificate(&pem) {
        return Err(DepError::Answer {
            what: "the token certificate",
            why: "not a PEM certificate".to_owned(),
        }
        .into());
    }
    let mut planner = Planner::new(host);
    planner.file(file, 0o644, &Account::root(), &pem)?;
    execute(&planner.finish(), mode, host, out)?;
    writeln!(out, "{file}: {NEXT}")?;
    Ok(())
}

/// The largest server token file read, in bytes.
const MAX_TOKEN: u64 = 64 * 1024;

fn upload_token(
    env: &Env<'_>,
    api: &Api,
    c: &MdmConfig,
    token: &str,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let name = c.dep_name();
    crate::repo::file_outside_git(env.host, token)?;
    // One bounded read: a server token is a few kilobytes of S/MIME.
    let read = env
        .host
        .read_limited(token, MAX_TOKEN)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::FileTooLarge => MdmError::from(DepError::Token {
                path: token.to_owned(),
                why: "larger than 64 KiB",
            }),
            _ => MdmError::Io(e),
        });
    let bytes = read?.ok_or_else(|| MdmError::NoFile(token.to_owned()))?;
    if bytes.is_empty() {
        return Err(DepError::Token {
            path: token.to_owned(),
            why: "empty",
        }
        .into());
    }
    let put = Call::put(format!("/v1/tokenpki/{name}"), bytes);
    api::call(env, api, &put, mode, out)?;
    let get = Call::get(format!("/proxy/{name}/account"));
    let Some(account) = api::call(env, api, &get, mode, out)? else {
        writeln!(out, "{DRY_RUN_NOTE}")?;
        return Ok(());
    };
    // The token is stored by now; an answer that is not a JSON object fails the check of it.
    let account: serde_json::Value = serde_json::from_slice(&account)
        .ok()
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| DepError::Answer {
            what: "the account check",
            why: format!(
                "not a JSON object: {}",
                String::from_utf8_lossy(&account)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ),
        })?;
    let field = |key: &str| account.get(key).and_then(|v| v.as_str()).unwrap_or("?");
    writeln!(
        out,
        "ABM: org {}, MDM server {}",
        field("org_name"),
        field("server_name")
    )?;
    writeln!(
        out,
        "the token is stored; you may delete {token} (NanoDEP keeps it)"
    )?;
    Ok(())
}

/// The bytes of the recorded file `path`, when it is a regular file.
fn recorded(host: &dyn Host, path: &str) -> Result<Option<Vec<u8>>, MdmError> {
    Ok(match host.stat(path)?.map(|m| m.kind) {
        Some(Kind::File) => host.read(path)?,
        _ => None,
    })
}

fn define_profile(
    env: &Env<'_>,
    api: &Api,
    c: &MdmConfig,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let host = env.host;
    let json = AdeProfile::from_config(c)
        .to_json()
        .map_err(DepError::from)?;
    let sum = format!("{}\n", hex::encode(Sha256::digest(&json)));
    let uuid_file = format!("{}/dep-profile-uuid", c.state());
    let sum_file = format!("{}/dep-profile.sha256", c.state());
    let root = Account::root();
    let mut planner = Planner::new(host);
    // The profile is the same only when its digest is recorded and so is the uuid Apple gave it:
    // a lost uuid record would leave `assign` without a profile, so it is defined again.
    let same = recorded(host, &sum_file)?.as_deref() == Some(sum.as_bytes());
    let uuid = recorded(host, &uuid_file)?.unwrap_or_default();
    let uuid = String::from_utf8_lossy(&uuid).trim().to_owned();
    if same && is_uuid(&uuid) {
        writeln!(out, "ADE profile: unchanged (uuid {uuid})")?;
    } else {
        let name = c.dep_name();
        let define = Call::post_json(format!("/proxy/{name}/profile"), json.clone());
        match api::call(env, api, &define, mode, out)? {
            Some(answer) => {
                let uuid = profile_uuid(&answer)?;
                let assigner = Call::put(
                    format!("/v1/assigner/{name}?profile_uuid={uuid}"),
                    Vec::new(),
                );
                api::call(env, api, &assigner, mode, out)?;
                writeln!(
                    out,
                    "ADE profile: defined, uuid {uuid}; the assigner gives it to new devices"
                )?;
                planner.file(&uuid_file, 0o644, &root, format!("{uuid}\n").as_bytes())?;
            }
            None => {
                writeln!(
                    out,
                    "DRY-RUN: PUT /v1/assigner/{name}?profile_uuid=<the profile_uuid Apple answers>"
                )?;
                writeln!(out, "DRY-RUN: write {uuid_file} (that uuid)")?;
                for line in String::from_utf8_lossy(&json).lines() {
                    writeln!(out, "    | {line}")?;
                }
            }
        }
        planner.file(&sum_file, 0o644, &root, sum.as_bytes())?;
    }
    planner.service(&UnitNames::new(c).depsyncer, false)?;
    execute(&planner.finish(), mode, host, out)?;
    Ok(())
}

/// Whether `text` is a profile uuid as Apple answers it: 16 to 64 hex digits.
fn is_uuid(text: &str) -> bool {
    (16..=64).contains(&text.len()) && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// The `profile_uuid` of Apple's answer to a profile definition: 16 to 64 hex digits.
fn profile_uuid(answer: &[u8]) -> Result<String, DepError> {
    let value: serde_json::Value = serde_json::from_slice(answer).unwrap_or_default();
    let uuid = value
        .get("profile_uuid")
        .and_then(|u| u.as_str())
        .unwrap_or_default();
    if !is_uuid(uuid) {
        return Err(DepError::Answer {
            what: "the profile definition",
            why: format!(
                "no profile_uuid of 16 to 64 hex digits in {}",
                String::from_utf8_lossy(answer)
            ),
        });
    }
    Ok(uuid.to_owned())
}

fn assign(
    env: &Env<'_>,
    api: &Api,
    c: &MdmConfig,
    serials: &[Serial],
    mode: Mode,
    out: &mut dyn Write,
) -> Result<bool, MdmError> {
    let uuid_file = format!("{}/dep-profile-uuid", c.state());
    let uuid = recorded(env.host, &uuid_file)?
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
        .filter(|uuid| !uuid.is_empty())
        .ok_or(DepError::NoProfile(uuid_file))?;
    let devices: Vec<&str> = serials.iter().map(Serial::as_str).collect();
    let body = serde_json::json!({ "profile_uuid": uuid, "devices": devices });
    let post = Call::post_json(
        format!("/proxy/{}/profile/devices", c.dep_name()),
        body.to_string().into_bytes(),
    );
    let Some(answer) = api::call(env, api, &post, mode, out)? else {
        writeln!(out, "{DRY_RUN_NOTE}")?;
        return Ok(true);
    };
    // `{"profile_uuid": ..., "devices": {"<serial>": "SUCCESS" | "NOT_ACCESSIBLE" | ...}}`
    let answer: serde_json::Value = serde_json::from_slice(&answer).unwrap_or_default();
    let mut all = true;
    for serial in devices {
        let status = answer
            .get("devices")
            .and_then(|d| d.get(serial))
            .and_then(|s| s.as_str())
            .unwrap_or("no answer");
        all &= status == "SUCCESS";
        writeln!(out, "{serial}: {status}")?;
    }
    Ok(all)
}
