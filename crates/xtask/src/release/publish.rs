//! `release publish <dir> --repo R --version V --commit C`: publishes the checked assets in a
//! directory as the GitHub release of the version, and never rebuilds anything.
//!
//! The directory is checked first ([`check`]); a directory that fails is never published. The
//! release `v<version>` is then looked up:
//!
//! - a published release is left alone when it is at the commit and holds exactly these assets
//!   (the run is idempotent); any other published release is an error that names the first
//!   differing asset;
//! - a draft at the commit is completed with the assets it lacks, after checking that every
//!   asset it holds is one of these with the same digest;
//! - with no release, a draft is created and every asset uploaded.
//!
//! The draft is read back before it is published and must hold exactly the checked assets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::check::check;
use super::contract::{
    ARTIFACTS_WORKFLOW, Commit, Platform, RELEASE_COMMIT, REPO, Repo, SIGNER_REF, SIGNER_WORKFLOW,
    Version, asset_names, stamp_string,
};
use super::ports::{Ports, Release, StoredAsset};
use super::{Error, file_sha256};

/// Publishes the assets in `dir` as release `v<version>` of `repo` at `commit`. The lines it
/// returns are those of the check, then what was done.
pub(crate) fn publish(
    ports: &Ports<'_>,
    dir: &Path,
    repo: &Repo,
    version: &Version,
    commit: &Commit,
) -> Result<Vec<String>, Error> {
    let mut lines = check(ports.verifier, dir, version, commit)?;
    let local = local_assets(dir, version)?;
    let tag = version.tag();
    let mut found: Vec<Release> = ports
        .store
        .releases(repo)?
        .into_iter()
        .filter(|r| r.tag == tag)
        .collect();
    if found.len() > 1 {
        return Err(Error::Failed(format!(
            "{} releases have the tag {tag}: nothing was changed",
            found.len()
        )));
    }
    let held: BTreeSet<String> = match found.pop() {
        Some(release) => {
            let what = if release.draft {
                format!("the draft {tag}")
            } else {
                format!("release {tag}")
            };
            if release.commit != commit.as_str() {
                return Err(Error::Failed(format!(
                    "{what} exists at {}, not {commit}: nothing was changed",
                    release.commit
                )));
            }
            let theirs = stored(&release.assets);
            let problem = {
                let mut diffs = differences(&local, &theirs).into_iter();
                // A draft may lack assets (they are uploaded below); a release may not.
                let wrong = if release.draft {
                    diffs.find(|(_, d)| !matches!(d, Difference::Absent))
                } else {
                    diffs.next()
                };
                wrong.map(|(name, difference)| sentence(&what, &tag, name, &difference))
            };
            if let Some(problem) = problem {
                return Err(Error::Failed(problem));
            }
            if !release.draft {
                lines.push(format!(
                    "release {tag} is published with identical assets: nothing was changed"
                ));
                return Ok(lines);
            }
            theirs.into_keys().collect()
        }
        None => {
            ports
                .store
                .create_draft(repo, &tag, commit, &notes(version, commit))?;
            BTreeSet::new()
        }
    };
    for name in local.keys().filter(|n| !held.contains(*n)) {
        ports.store.upload(repo, &tag, &dir.join(name))?;
    }
    let back: BTreeMap<String, Option<String>> = ports
        .store
        .releases(repo)?
        .into_iter()
        .find(|r| (r.tag == tag) & r.draft)
        .map(|r| stored(&r.assets))
        .unwrap_or_default();
    let off = differences(&local, &back);
    if !off.is_empty() {
        let detail: Vec<String> = off
            .iter()
            .map(|(name, difference)| describe(name, difference))
            .collect();
        return Err(Error::Failed(format!(
            "the draft {tag} holds other assets than the checked ones ({}); it was not published",
            detail.join(", ")
        )));
    }
    ports.store.publish(repo, &tag)?;
    lines.push(format!(
        "published {tag} at {commit} with {} assets",
        local.len()
    ));
    Ok(lines)
}

/// The release notes of `version` built from `commit`: what it is, who built and signed it, and
/// the command that verifies a file of it.
pub(crate) fn notes(version: &Version, commit: &Commit) -> String {
    format!(
        "hostkeeper v{version}, built from commit {commit}. Each binary was built with \
         {RELEASE_COMMIT} set to that commit and prints `{}` for --version.\n\
         \n\
         The assets were built and signed by .github/workflows/{ARTIFACTS_WORKFLOW} on main. \
         To verify a file:\n\
         \n\
         gh attestation verify <file> --repo {REPO} --signer-workflow {SIGNER_WORKFLOW} \
         --source-ref {SIGNER_REF} --source-digest {commit}\n",
        stamp_string(version, commit)
    )
}

/// The name and SHA-256 of every asset of `version` in `dir`, in name order.
fn local_assets(dir: &Path, version: &Version) -> Result<BTreeMap<String, String>, Error> {
    asset_names(version, &Platform::RELEASED)
        .into_iter()
        .map(|name| {
            let digest = file_sha256(&dir.join(&name))?;
            Ok::<_, Error>((name, digest))
        })
        .collect()
}

/// The assets a release holds, by name, with the digest the store gives (none when it gives
/// none).
fn stored(assets: &[StoredAsset]) -> BTreeMap<String, Option<String>> {
    assets
        .iter()
        .map(|a| (a.name.clone(), a.sha256.clone()))
        .collect()
}

/// How one asset of a release differs from the checked one.
#[derive(Debug, PartialEq, Eq)]
enum Difference {
    /// Both hold it, with different digests.
    Changed { ours: String, theirs: String },
    /// The release lacks it.
    Absent,
    /// The release holds it and it is not an asset of the version.
    Foreign,
}

/// Every difference between the checked assets and the release's, in name order.
fn differences<'a>(
    local: &'a BTreeMap<String, String>,
    theirs: &'a BTreeMap<String, Option<String>>,
) -> Vec<(&'a str, Difference)> {
    let names: BTreeSet<&String> = local.keys().chain(theirs.keys()).collect();
    names
        .into_iter()
        .filter_map(|name| {
            let difference = match (local.get(name), theirs.get(name)) {
                (Some(ours), Some(his)) if his.as_ref() != Some(ours) => Difference::Changed {
                    ours: ours.clone(),
                    theirs: his.clone().unwrap_or_else(|| "none".to_owned()),
                },
                (Some(_), Some(_)) => return None,
                (Some(_), None) => Difference::Absent,
                (None, _) => Difference::Foreign,
            };
            Some((name.as_str(), difference))
        })
        .collect()
}

/// The error text for a release (`what` is `release <tag>` or `the draft <tag>`) that differs
/// from the checked assets at `name`.
fn sentence(what: &str, tag: &str, name: &str, difference: &Difference) -> String {
    match difference {
        Difference::Changed { ours, theirs } => format!(
            "{what} exists with a different {name}: sha256 {theirs}, not {ours}; nothing was changed"
        ),
        Difference::Absent => format!("{what} exists without {name}; nothing was changed"),
        Difference::Foreign => {
            format!("{what} holds {name}, which is not an asset of {tag}; nothing was changed")
        }
    }
}

/// The short form of a difference, for the read-back error.
fn describe(name: &str, difference: &Difference) -> String {
    match difference {
        Difference::Changed { ours, theirs } => {
            format!("{name} has sha256 {theirs}, not {ours}")
        }
        Difference::Absent => format!("{name} is missing"),
        Difference::Foreign => format!("{name} is not a checked asset"),
    }
}

#[cfg(test)]
mod tests;
