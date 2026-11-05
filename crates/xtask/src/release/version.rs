//! The version rule. The version of a commit C is `X.Y.<patch>`: `X.Y` is the text of the
//! version file at C, and the patch is the number of first-parent commits from the commit that
//! last changed the version file (that commit counts as 1) up to and including C.
//!
//! A commit changes the file when its text there differs from its text at its first parent, or
//! when the commit is the root, or when the file appeared there. The count reads only C and its
//! first-parent chain, never the tip of `main` and never a clock, so every job and every re-run
//! for one commit gets one answer.

use super::Error;
use super::contract::{Base, Commit, Version};
use super::ports::GitHistory;

/// The error text for a shallow clone, which cannot count the first-parent history.
pub(crate) const SHALLOW: &str = "a shallow clone cannot count the first-parent history: the jobs that compute the version check out with fetch-depth: 0";

/// The version of `commit`, with the version file at the path `version_file` from the repository
/// root.
pub(crate) fn version(
    git: &dyn GitHistory,
    commit: &Commit,
    version_file: &str,
) -> Result<Version, Error> {
    if git.is_shallow()? {
        return Err(Error::Failed(SHALLOW.to_owned()));
    }
    let chain = git.first_parents(commit)?;
    version_in(git, &chain, version_file)
}

/// The version of the first commit of `chain`: the commit then its first parents, newest first.
pub(crate) fn version_in(
    git: &dyn GitHistory,
    chain: &[Commit],
    version_file: &str,
) -> Result<Version, Error> {
    let (text, index) = walk(git, chain, version_file)?;
    let base = text
        .parse::<Base>()
        .map_err(|e| Error::Failed(format!("{version_file} at {}: {e}", chain[0])))?;
    Ok(base.with_patch(index as u64 + 1))
}

/// The index in `chain` of the commit that last changed the version file.
pub(crate) fn base_of(
    git: &dyn GitHistory,
    chain: &[Commit],
    version_file: &str,
) -> Result<usize, Error> {
    walk(git, chain, version_file).map(|(_, index)| index)
}

/// The text of the version file at the first commit of `chain`, and the index of the commit that
/// last changed it. Each commit's file is read once.
fn walk(
    git: &dyn GitHistory,
    chain: &[Commit],
    version_file: &str,
) -> Result<(String, usize), Error> {
    let tip = chain
        .first()
        .ok_or_else(|| Error::Failed("no commit to count".to_owned()))?;
    let first = git
        .file_at(tip, version_file)?
        .ok_or_else(|| Error::Failed(format!("{version_file} does not exist at {tip}")))?;
    let mut current = Some(first.clone());
    let mut index = 0;
    // The text at the tip exists, so `current` is a text at every step; past the root the file
    // is none, which differs, so the loop returns at the root at the latest.
    loop {
        let before = match chain.get(index + 1) {
            Some(parent) => git.file_at(parent, version_file)?,
            None => None,
        };
        if before != current {
            return Ok((first, index));
        }
        current = before;
        index += 1;
    }
}

#[cfg(test)]
mod tests;
