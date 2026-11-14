//! `cargo xtask repository check <base-url-or-dir>`: the conformance check of the hostkeeper
//! repository format (docs/deploy/package-repositories.md, "hostkeeper repository format,
//! version 1"), for the publisher of a repository to run before publishing it.
//!
//! It runs the rules the coordinator judges a repository by when an operator adds it: the
//! descriptor by [`hostkeeper_release::descriptor::check`], the keyring it names by
//! [`hostkeeper_release::registry::keys`], and the fingerprints it claims against the keyring's.
//! Given a directory (the tree about to be uploaded) it reads the files from it and also checks
//! that the layout the descriptor promises is there: the `InRelease` file of each channel's apt
//! suite, the directory of each channel of the rpm format, the directory of the macOS installers.
//! Given an `https` URL it reads the descriptor and the keyring from the host as `channels
//! descriptor check` does (`curl`, behind [`Host`]).
//!
//! The check never writes to the repository and never needs a credential.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use hostkeeper_release::descriptor as rules;
use hostkeeper_release::registry::MAX_KEYRING;

use crate::channels::descriptor::{check as check_url, fingerprints_of};
use crate::channels::host::Host;
use crate::channels::{Error, read};

/// The `repository` verbs.
#[derive(Subcommand)]
pub(crate) enum Verb {
    /// A repository judged by the rules the coordinator adds it by.
    Check {
        /// The repository: a directory that holds `hostkeeper-repository.json` at its root, or
        /// the `https` URL of the base, without a trailing `/`.
        base: String,
        /// A directory for the downloads; needed for a URL, ignored for a directory.
        #[arg(long)]
        work: Option<PathBuf>,
        /// The `curl` program, for a URL.
        #[arg(long, default_value = "curl")]
        curl: PathBuf,
    },
}

/// Runs a verb: the lines to print.
///
/// # Errors
/// Whatever the check refuses or fails on.
pub(crate) fn run(verb: Verb, host: &dyn Host) -> Result<Vec<String>, Error> {
    match verb {
        Verb::Check { base, work, curl } => {
            if base.starts_with("https://") {
                let work = work.ok_or_else(|| {
                    Error::Usage(
                        "--work is needed to check a URL: a directory for the downloads".to_owned(),
                    )
                })?;
                check_url(host, &base, &work, &curl)
            } else {
                check_dir(Path::new(&base))
            }
        }
    }
}

/// The check of the tree under `dir`: the first thing that is wrong with the descriptor or the
/// keyring, or every claim of the descriptor that the tree does not bear out.
fn check_dir(dir: &Path) -> Result<Vec<String>, Error> {
    if !dir.is_dir() {
        return Err(Error::Usage(format!(
            "{} is neither an https URL nor a directory",
            dir.display()
        )));
    }
    let file = dir.join(rules::FILE);
    let parsed = rules::check(&read(&file)?)
        .map_err(|e| Error::Failed(format!("{}: {e}", file.display())))?;
    let ring = read(&dir.join(&parsed.keyring))?;
    if ring.len() > MAX_KEYRING {
        return Err(Error::Failed(format!(
            "{} is {} bytes, at most {MAX_KEYRING}",
            parsed.keyring,
            ring.len()
        )));
    }
    let held = fingerprints_of(&ring, &parsed.keyring)?;
    let mut problems: Vec<String> = parsed
        .fingerprints
        .iter()
        .filter(|claimed| !held.contains(claimed))
        .map(|claimed| {
            format!(
                "the descriptor claims the fingerprint {claimed}, which no key of {} has",
                parsed.keyring
            )
        })
        .collect();
    for channel in &parsed.channels {
        let release = format!("{}/dists/{channel}/InRelease", parsed.apt.path);
        if !dir.join(&release).is_file() {
            problems.push(format!(
                "{release} is not a file: the apt suite {channel} is not there"
            ));
        }
        if let Some(rpm) = &parsed.rpm {
            let channel_dir = format!("{}/{channel}", rpm.path);
            if !dir.join(&channel_dir).is_dir() {
                problems.push(format!(
                    "{channel_dir} is not a directory: the rpm channel {channel} is not there"
                ));
            }
        }
    }
    if let Some(mac) = &parsed.macos_pkg
        && !dir.join(&mac.path).is_dir()
    {
        problems.push(format!(
            "{} is not a directory: the macOS installers are not there",
            mac.path
        ));
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "{}: valid, {} channels, platforms {}, keys {}",
        file.display(),
        parsed.channels.len(),
        parsed.platforms().join(" "),
        held.join(" ")
    )])
}

#[cfg(test)]
mod tests;
