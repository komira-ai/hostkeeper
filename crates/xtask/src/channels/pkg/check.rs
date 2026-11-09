//! `channels pkg check`: the macOS installer as the public host serves it, held to the release's
//! `SHA256SUMS`, before the pointer to it moves.
//!
//! The verb downloads the installer, its `.sigstore.json` bundle and the directory's `SHA256SUMS`
//! from `<url>/pkg/<product>/<version>/` over HTTPS (`curl`, behind [`Host`], so a test answers
//! in place of it and no test reaches a network), and refuses unless the installer has the digest
//! the release's `SHA256SUMS` lists, the directory's `SHA256SUMS` lists the installer with that
//! digest and the bundle with the digest of the bundle that was downloaded. It does not read the
//! installer's signature, which the release made (see [`super`]); this check is what says the
//! bytes a user downloads are the ones the release built and signed.
//!
//! New in this repository; the other `pkg` verbs are the copy of komira-ai/tierstore.

use std::path::{Path, PathBuf};

use super::BUNDLE_SUFFIX;
use crate::channels::apt::strings;
use crate::channels::check::{url_flag, version_flag};
use crate::channels::host::Host;
use crate::channels::sums::{self, SUMS, Sha256};
use crate::channels::{Error, read, spec};

/// What `pkg check` is asked.
pub(crate) struct Request<'a> {
    /// The release's `SHA256SUMS`.
    pub(crate) sums: &'a Path,
    pub(crate) version: &'a str,
    /// The repository host, `https://packages.komira.ai`.
    pub(crate) url: &'a str,
    /// A directory for the downloads.
    pub(crate) work: &'a Path,
    pub(crate) curl: &'a Path,
}

/// Downloads `url` to `work/<name>` and returns the digest of what arrived.
fn download(
    host: &dyn Host,
    r: &Request,
    base: &str,
    name: &str,
) -> Result<(PathBuf, Sha256), Error> {
    let dest = r.work.join(name);
    let url = format!("{base}{name}");
    let got = host.run(
        r.curl,
        &strings(&[
            "--proto",
            "=https",
            "--tlsv1.2",
            "--fail",
            "--silent",
            "--show-error",
            "--output",
            &dest.display().to_string(),
            &url,
        ]),
    )?;
    if !got.ok {
        return Err(Error::Failed(format!(
            "{url} could not be downloaded: {}",
            got.stderr.trim()
        )));
    }
    let digest = Sha256::of(&read(&dest)?);
    Ok((dest, digest))
}

/// The `pkg check` verb: the lines to print.
///
/// # Errors
/// A flag out of its form, a release `SHA256SUMS` that does not list the installer, a download
/// that fails, or one problem per way what the host serves differs from the release.
pub(crate) fn check(host: &dyn Host, r: &Request) -> Result<Vec<String>, Error> {
    version_flag(r.version)?;
    url_flag("--url", r.url)?;
    let installer = spec::HOSTKEEPER.pkg_file(r.version);
    let bundle = format!("{installer}{BUNDLE_SUFFIX}");
    let released = sums::read_sums(r.sums)?
        .into_iter()
        .find(|l| l.name == installer)
        .ok_or_else(|| Error::Failed(format!("{} does not list {installer}", r.sums.display())))?;
    std::fs::create_dir_all(r.work)
        .map_err(Error::spawn(format!("create {}", r.work.display())))?;
    let base = format!("{}/{}", r.url, spec::HOSTKEEPER.pkg_prefix(r.version));
    let (_, served) = download(host, r, &base, &installer)?;
    let (_, served_bundle) = download(host, r, &base, &bundle)?;
    let (sums_file, _) = download(host, r, &base, SUMS)?;
    let text = std::fs::read_to_string(&sums_file)
        .map_err(Error::spawn(format!("read {}", sums_file.display())))?;
    let listed = sums::parse(&text).map_err(|problems| {
        Error::Refused(
            problems
                .into_iter()
                .map(|p| format!("{base}{SUMS}: {p}"))
                .collect(),
        )
    })?;
    let mut problems = Vec::new();
    if served != released.sha256 {
        problems.push(format!(
            "{base}{installer} is {served}, but the release's {SUMS} lists {}",
            released.sha256
        ));
    }
    for (name, digest) in [(&installer, served), (&bundle, served_bundle)] {
        match listed.iter().find(|l| l.name == *name) {
            None => problems.push(format!("{base}{SUMS} does not list {name}")),
            Some(line) if line.sha256 != digest => problems.push(format!(
                "{base}{SUMS} lists {name} as {}, but it is {digest}",
                line.sha256
            )),
            Some(_) => {}
        }
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "check-pkg: {installer} from {base} is {served}, as the release's {SUMS} lists; its bundle and the directory's {SUMS} agree; these are the release's signed bytes"
    )])
}

#[cfg(test)]
mod tests;
