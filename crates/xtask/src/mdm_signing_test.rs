//! `cargo xtask mdm-signing-test <verb>`: the device test of pkg signing, task M11 of
//! `docs/design/hostkeeper-mdm.md` (section 7.2). It answers one question: does a Mac install, by
//! `InstallEnterpriseApplication`, a pkg signed by a farm installer identity whose root a
//! configuration profile installs? The runbook is `docs/operations/pkg-signing-test.md`.
//!
//! - `build` ([`build`]), on a Mac other than the one under test: makes two test CAs and an
//!   installer identity under each with `/usr/bin/openssl`, builds the four pkgs of section 7.2
//!   with `pkgbuild`, `productbuild` and `productsign`, writes the profile that installs the
//!   first CA's root, and writes the kit's description, `kit.toml` ([`Kit`]). It deletes the
//!   keys, the keychain and the second root before it ends: the kit holds no private key, and
//!   the second root exists nowhere.
//! - `send` ([`send`]), on the MDM host: one step of the test ([`Step`]) as one MDM command
//!   ([`commands`]), enqueued through NanoHUB's API on loopback ([`nanohub`]). It prints the
//!   command and changes nothing unless `--apply` is given; each send it makes is appended to
//!   `results.toml` ([`results`]) beside the kit.
//! - `record` ([`results::record`]): what the operator read for that send: the device's answer
//!   in NanoHUB's log and, for a pkg, whether the Mac holds its receipt.
//! - `report` ([`results::report`]): the table of the run and what it decides for task M15.
//!
//! The four pkgs carry a marker file, not the hostkeeper payload, each under its own identifier
//! ([`PKGS`]): a receipt then names exactly one pkg, and no postinstall can fail an install for a
//! reason that is not its signature.

mod build;
mod commands;
mod nanohub;
mod results;

#[cfg(test)]
mod build_tests;
#[cfg(test)]
mod results_tests;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) use build::build;
pub(crate) use nanohub::NanoHubHttp;
use nanohub::{ApiUrl, Enqueue, EnrollmentId};
use results::Answer;

use crate::release::ports::PortError;

/// Why a verb of the signing test failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// A program the build runs could not answer.
    #[error(transparent)]
    Port(#[from] PortError),
    /// A file could not be read or written.
    #[error("{what}: {why}")]
    Io { what: String, why: String },
    /// The verb refuses, for the reason given.
    #[error("{0}")]
    Refused(String),
    /// NanoHUB did not queue the command.
    #[error("NanoHUB: {0}")]
    NanoHub(String),
}

impl Error {
    /// The error for the file operation `what` that failed.
    fn io(what: impl std::fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
        let what = what.to_string();
        move |e| Error::Io {
            what,
            why: e.to_string(),
        }
    }
}

/// One step of the test, in the order the runbook runs them.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, Serialize, Deserialize,
)]
pub(crate) enum Step {
    /// Install the profile that holds the first CA's root.
    #[value(name = "root-profile")]
    #[serde(rename = "root-profile")]
    RootProfile,
    /// Pkg 1: a component pkg (`pkgbuild`), unsigned.
    #[value(name = "pkg-1")]
    #[serde(rename = "pkg-1")]
    Pkg1,
    /// Pkg 2: a distribution pkg (`productbuild`), unsigned.
    #[value(name = "pkg-2")]
    #[serde(rename = "pkg-2")]
    Pkg2,
    /// Pkg 3: a distribution pkg signed by the identity whose root the profile installs.
    #[value(name = "pkg-3")]
    #[serde(rename = "pkg-3")]
    Pkg3,
    /// Pkg 4: a distribution pkg signed by the identity whose root is installed nowhere.
    #[value(name = "pkg-4")]
    #[serde(rename = "pkg-4")]
    Pkg4,
    /// Remove the root's profile again.
    #[value(name = "remove-root-profile")]
    #[serde(rename = "remove-root-profile")]
    RemoveRootProfile,
}

impl Step {
    /// The step's name on the command line and in the files.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Step::RootProfile => "root-profile",
            Step::Pkg1 => "pkg-1",
            Step::Pkg2 => "pkg-2",
            Step::Pkg3 => "pkg-3",
            Step::Pkg4 => "pkg-4",
            Step::RemoveRootProfile => "remove-root-profile",
        }
    }

    /// The pkg the step sends, if it sends one.
    pub(crate) fn pkg(self) -> Option<&'static PkgSpec> {
        PKGS.iter().find(|p| p.step == self)
    }
}

/// How a pkg is packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    /// `pkgbuild`'s output alone.
    Component,
    /// `productbuild --package` around a component.
    Distribution,
}

/// Who signs a pkg.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signer {
    /// Nobody.
    Unsigned,
    /// The identity under CA `A`, whose root the profile installs.
    RootInProfile,
    /// The identity under CA `B`, whose root is installed nowhere.
    RootNowhere,
}

/// One of the four pkgs of design section 7.2.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PkgSpec {
    pub(crate) step: Step,
    /// 1 to 4, the pkg's number in the design.
    pub(crate) number: u8,
    pub(crate) format: Format,
    pub(crate) signer: Signer,
    /// What the pkg is, in words, for the manifest's title and the report.
    pub(crate) what: &'static str,
}

impl PkgSpec {
    /// The pkg's identifier, and its component's: `dev.hostkeeper.signing-test.pkg<n>`.
    pub(crate) fn identifier(&self) -> String {
        format!("{IDENTIFIER_PREFIX}{}", self.number)
    }
}

/// The four pkgs, in order.
pub(crate) const PKGS: [PkgSpec; 4] = [
    PkgSpec {
        step: Step::Pkg1,
        number: 1,
        format: Format::Component,
        signer: Signer::Unsigned,
        what: "unsigned component pkg",
    },
    PkgSpec {
        step: Step::Pkg2,
        number: 2,
        format: Format::Distribution,
        signer: Signer::Unsigned,
        what: "unsigned distribution pkg",
    },
    PkgSpec {
        step: Step::Pkg3,
        number: 3,
        format: Format::Distribution,
        signer: Signer::RootInProfile,
        what: "distribution pkg signed under the root the profile installs",
    },
    PkgSpec {
        step: Step::Pkg4,
        number: 4,
        format: Format::Distribution,
        signer: Signer::RootNowhere,
        what: "distribution pkg signed under a root installed nowhere",
    },
];

/// The prefix of the pkgs' identifiers, under the product's reverse-DNS prefix.
const IDENTIFIER_PREFIX: &str = "dev.hostkeeper.signing-test.pkg";

/// The version of every pkg of the kit.
pub(crate) const PKG_VERSION: &str = "1";

/// Where each pkg installs its one marker file on the Mac.
pub(crate) const INSTALL_LOCATION: &str = "/Library/Application Support/hostkeeper-signing-test";

/// The kit's description, the file `build` writes.
pub(crate) const KIT_FILE: &str = "kit.toml";

/// The directory of the kit that holds each pkg as `<sha256>.pkg`, the names the proxy serves
/// them under (design section 7.1).
pub(crate) const PKG_DIR: &str = "pkg";

/// The root's profile in the kit.
pub(crate) const PROFILE_FILE: &str = "signing-test-root.mobileconfig";

/// What `build` made, as `kit.toml` says it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Kit {
    /// The farm's MDM profile identifier, the prefix of the root profile's.
    pub(crate) profile_identifier: String,
    pub(crate) root_profile: RootProfile,
    #[serde(rename = "pkg")]
    pub(crate) pkgs: Vec<KitPkg>,
}

/// The root's profile, [`PROFILE_FILE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootProfile {
    /// Its `PayloadIdentifier`, which `RemoveProfile` names.
    pub(crate) identifier: String,
    pub(crate) sha256: String,
    /// The SHA-256 of the root certificate it holds (DER).
    pub(crate) root_sha256: String,
}

/// One pkg of the kit, at `pkg/<sha256>.pkg`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KitPkg {
    pub(crate) step: Step,
    pub(crate) identifier: String,
    pub(crate) version: String,
    pub(crate) sha256: String,
    /// What `pkgutil --check-signature` printed on the Mac that built it (empty for an
    /// unsigned pkg): the identity and its chain as that Mac reads them.
    pub(crate) build_check_signature: String,
}

impl Kit {
    /// The kit described by `<dir>/kit.toml`.
    pub(crate) fn read(dir: &Path) -> Result<Kit, Error> {
        let path = dir.join(KIT_FILE);
        let text = std::fs::read_to_string(&path).map_err(Error::io(format!(
            "read {} (made by `mdm-signing-test build`)",
            path.display()
        )))?;
        let kit: Kit = toml::from_str(&text)
            .map_err(|e| Error::Refused(format!("{}: {}", path.display(), e.message())))?;
        let steps: Vec<Step> = kit.pkgs.iter().map(|p| p.step).collect();
        let want: Vec<Step> = PKGS.iter().map(|p| p.step).collect();
        if steps != want {
            return Err(Error::Refused(format!(
                "{}: the pkgs are {}, not pkg-1 to pkg-4 in order",
                path.display(),
                steps
                    .iter()
                    .map(|s| s.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(kit)
    }
}

/// The SHA-256 of the file at `path`, hex.
pub(crate) fn file_sha256(path: &Path) -> Result<String, Error> {
    crate::release::file_sha256(path).map_err(|e| Error::Refused(e.to_string()))
}

/// `bytes` read from `path`, refused unless their SHA-256 is `want`: the gate's check of a
/// digest before it names it (design section 7.1).
fn read_checked(path: &Path, want: &str) -> Result<Vec<u8>, Error> {
    use sha2::Digest;
    let bytes = std::fs::read(path).map_err(Error::io(format!("read {}", path.display())))?;
    let got = hex::encode(sha2::Sha256::digest(&bytes));
    if got != want {
        return Err(Error::Refused(format!(
            "{} has the SHA-256 {got}, not {want} as kit.toml says",
            path.display()
        )));
    }
    Ok(bytes)
}

/// The verbs of `cargo xtask mdm-signing-test`.
#[derive(clap::Subcommand)]
pub(crate) enum Verb {
    /// On a Mac other than the one under test: make the test CAs and build the four pkgs and
    /// the root's profile into a new directory.
    Build {
        /// The kit's directory; it must not exist.
        #[arg(long)]
        out: PathBuf,
        /// The farm's MDM profile identifier (`com.komira.hostkeeper.mdm` on the farm); the
        /// root's profile is `<it>.signing-test.root`.
        #[arg(long)]
        profile_identifier: commands::ProfileIdentifier,
    },
    /// On the MDM host: send one step's command to the Mac through NanoHUB. Prints the command
    /// and sends nothing without `--apply`.
    Send {
        /// The kit's directory, as `build` made it.
        #[arg(long)]
        kit: PathBuf,
        #[arg(long, value_enum)]
        step: Step,
        /// The Mac's enrollment id in NanoHUB (its UDID).
        #[arg(long)]
        enrollment_id: EnrollmentId,
        /// NanoHUB's API, `http://<loopback address>:<port>`.
        #[arg(long)]
        api_url: ApiUrl,
        /// The file that holds NanoHUB's API key, mode 0600 or tighter.
        #[arg(long)]
        api_key_file: PathBuf,
        /// The MDM host's name, as its certificate names it: a pkg's URL is
        /// `https://<it>/pkg/<sha256>.pkg`.
        #[arg(long)]
        mdm_host: commands::MdmHost,
        /// Send the command (without it, print it and change nothing).
        #[arg(long)]
        apply: bool,
    },
    /// Record what the operator read for the last send of a step.
    Record {
        #[arg(long)]
        kit: PathBuf,
        #[arg(long, value_enum)]
        step: Step,
        /// The device's answer, from NanoHUB's log line of the command's UUID.
        #[arg(long, value_enum)]
        answer: Answer,
        /// For a pkg: whether the Mac holds its receipt (`pkgutil --pkg-info`).
        #[arg(long)]
        installed: Option<bool>,
        /// For pkg 3: whether `pkgutil --check-signature` on the Mac, with the root's profile
        /// installed, reads its signature as trusted.
        #[arg(long)]
        trusted: Option<bool>,
        /// What the Mac's install log says about the pkg, in one line.
        #[arg(long)]
        note: Option<String>,
    },
    /// Print the run's table and what it decides.
    Report {
        #[arg(long)]
        kit: PathBuf,
    },
}

/// Runs `verb`: `build` runs its programs through `exec`; `send --apply` sends through the
/// client `connect` makes from NanoHUB's URL and API key. The lines to print.
pub(crate) fn run<N: Enqueue>(
    verb: Verb,
    exec: &dyn crate::release::ports::Exec,
    connect: impl FnOnce(&ApiUrl, String) -> N,
) -> Result<Vec<String>, Error> {
    match verb {
        Verb::Build {
            out,
            profile_identifier,
        } => build(exec, &out, &profile_identifier),
        Verb::Send {
            kit,
            step,
            enrollment_id,
            api_url,
            api_key_file,
            mdm_host,
            apply,
        } => {
            let target = Target {
                enrollment_id: &enrollment_id,
                mdm_host: &mdm_host,
            };
            if apply {
                let key = nanohub::read_key(&api_key_file)?;
                let client = connect(&api_url, key);
                send(&kit, step, &target, Some(&client))
            } else {
                send::<N>(&kit, step, &target, None)
            }
        }
        Verb::Record {
            kit,
            step,
            answer,
            installed,
            trusted,
            note,
        } => results::record(
            &kit,
            step,
            results::Outcome {
                answer,
                installed,
                trusted,
                note,
            },
        ),
        Verb::Report { kit } => results::report(&kit),
    }
}

/// The Mac a send goes to, and the host that serves the pkgs.
pub(crate) struct Target<'a> {
    pub(crate) enrollment_id: &'a EnrollmentId,
    pub(crate) mdm_host: &'a commands::MdmHost,
}

/// Sends `step` of the kit in `dir` to the target through `client`, or, with none, prints the
/// command it would send.
pub(crate) fn send<N: Enqueue>(
    dir: &Path,
    step: Step,
    target: &Target<'_>,
    client: Option<&N>,
) -> Result<Vec<String>, Error> {
    let kit = Kit::read(dir)?;
    let mut sent = results::Results::read(dir)?;
    sent.may_send(step)?;
    let uuid = commands::command_uuid(&kit, step, sent.sends_of(step));
    let command = match step.pkg() {
        Some(spec) => {
            // `Kit::read` holds the pkgs to pkg-1 to pkg-4 in order.
            let pkg = &kit.pkgs[usize::from(spec.number) - 1];
            read_checked(
                &dir.join(PKG_DIR).join(format!("{}.pkg", pkg.sha256)),
                &pkg.sha256,
            )?;
            commands::install_application(spec, pkg, target.mdm_host, &uuid)
        }
        None if step == Step::RootProfile => {
            let profile = read_checked(&dir.join(PROFILE_FILE), &kit.root_profile.sha256)?;
            commands::install_profile(&profile, &uuid)
        }
        None => commands::remove_profile(&kit.root_profile.identifier, &uuid),
    };
    let mut lines = vec![format!(
        "{} to {}: command {uuid}",
        step.name(),
        target.enrollment_id.as_str()
    )];
    let Some(client) = client else {
        lines.push(String::from_utf8_lossy(&command).into_owned());
        lines.push("dry run: nothing sent; add --apply to send it".to_owned());
        return Ok(lines);
    };
    let queued = client.enqueue(target.enrollment_id, &command)?;
    match &queued.push_error {
        None => lines.push("queued and pushed".to_owned()),
        Some(why) => lines.push(format!(
            "queued; the push failed ({why}): the Mac fetches it at its next check-in"
        )),
    }
    sent.add_send(step, &uuid, queued.push_error);
    sent.write(dir).map_err(|e| {
        Error::Refused(format!(
            "NanoHUB queued command {uuid} for {}, but it could not be recorded ({e}): record it \
             by hand before the next send of this step",
            step.name()
        ))
    })?;
    lines.push(format!(
        "find the answer: the NanoHUB unit's log line with command_uuid={uuid}, then \
         `mdm-signing-test record --step {}`",
        step.name()
    ));
    Ok(lines)
}
