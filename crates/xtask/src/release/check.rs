//! `release check <dir> --version V --commit C`: checks a directory of release assets without
//! changing it, and lists every problem it finds (one line each), in this order:
//!
//! 1. the regular files of the directory are exactly the assets of the version
//!    ([`asset_names`]);
//! 2. `SHA256SUMS` has one `<sha256>  <name>` line per summed asset ([`summed_names`]), and each
//!    digest is that of the file;
//! 3. `hkctl-<version>` is a bash script whose one version line is the stamp of the version and
//!    the pinned commit (`hkctl::problems`);
//! 4. the coordinator's image index is the one its two image tarballs make, byte for byte
//!    ([`super::image_index`]): its SHA-256 is the digest of the image a registry serves;
//! 5. each attested asset verifies twice, online and from its bundle, and every attestation
//!    returned names this repository's signer workflow and ref, the pinned commit, and the
//!    SHA-256 of the file.

use std::collections::BTreeSet;
use std::path::Path;

use super::contract::{
    Commit, Platform, SUMS, Signer, Version, asset_names, attested_names, bundle_of, hkctl_name,
    summed_names,
};
use super::hkctl;
use super::image_index;
use super::ports::{Attestation, PortError, Verifier};
use super::{Error, file_sha256};

/// Checks the assets in `dir` against `version`, `commit` and `verifier`. Every problem is in
/// the [`Error::Refused`] it returns; success reports how much was checked.
pub(crate) fn check(
    verifier: &dyn Verifier,
    dir: &Path,
    version: &Version,
    commit: &Commit,
) -> Result<Vec<String>, Error> {
    let expected = asset_names(version, &Platform::RELEASED);
    let present = regular_files(dir)?;
    let mut problems = Vec::new();
    check_names(version, &expected, &present, &mut problems);
    check_sums(dir, version, &present, &mut problems)?;
    check_hkctl(dir, (version, commit), &present, &mut problems)?;
    check_image_index(dir, version, &present, &mut problems);
    let attested = attested_names(version);
    for name in attested.iter().filter(|n| present.contains(*n)) {
        check_attestations(verifier, dir, name, commit, &mut problems)?;
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "release check: the {} assets of v{version} match {SUMS}, and {} attestations of commit \
         {commit} verified online and from their bundles",
        expected.len(),
        2 * attested.len()
    )])
}

/// The names of the regular files of `dir`. Anything else in it (a directory, a link) is not an
/// asset and is not listed.
fn regular_files(dir: &Path) -> Result<BTreeSet<String>, Error> {
    let entries = std::fs::read_dir(dir)
        .and_then(|read| read.collect::<Result<Vec<_>, _>>())
        .map_err(Error::io(format!("read {}", dir.display())))?;
    Ok(entries
        .iter()
        .filter(|e| e.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect())
}

/// Step 4: the image index is the one the two image tarballs make. It is skipped when the index
/// or a tarball is absent (step 1 reported that).
fn check_image_index(
    dir: &Path,
    version: &Version,
    present: &BTreeSet<String>,
    problems: &mut Vec<String>,
) {
    let needed = [
        image_index::index_name(version),
        image_index::image_name(version, Platform::LinuxX86_64),
        image_index::image_name(version, Platform::LinuxArm64),
    ];
    if needed.iter().all(|n| present.contains(n))
        && let Err(why) = image_index::check_index(dir, version)
    {
        problems.push(why.to_string());
    }
}

/// Step 1: the files present are exactly the assets of the version.
fn check_names(
    version: &Version,
    expected: &[String],
    present: &BTreeSet<String>,
    problems: &mut Vec<String>,
) {
    let expected: BTreeSet<&String> = expected.iter().collect();
    let have: BTreeSet<&String> = present.iter().collect();
    problems.extend(expected.difference(&have).map(|n| format!("missing {n}")));
    problems.extend(
        have.difference(&expected)
            .map(|n| format!("{n} is not an asset of v{version}")),
    );
}

/// One line of `SHA256SUMS` as its digest and name, none when it is not
/// `<64 lowercase hex>  <name>`.
pub(crate) fn sums_line(line: &str) -> Option<(&str, &str)> {
    let (digest, name) = line.split_once("  ")?;
    let hex = (digest.len() == 64)
        & digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (hex & !name.is_empty()).then_some((digest, name))
}

/// Step 2: `SHA256SUMS` lists exactly the summed assets with the digests of their files. It is
/// skipped when the file is absent (step 1 reported that).
fn check_sums(
    dir: &Path,
    version: &Version,
    present: &BTreeSet<String>,
    problems: &mut Vec<String>,
) -> Result<(), Error> {
    if !present.contains(SUMS) {
        return Ok(());
    }
    let path = dir.join(SUMS);
    let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut listed = Vec::new();
    for (index, line) in text.lines().enumerate() {
        match sums_line(line) {
            Some(entry) => listed.push(entry),
            None => problems.push(format!(
                "{SUMS} line {} is not '<sha256>  <name>'",
                index + 1
            )),
        }
    }
    let summed = summed_names(version);
    let want: BTreeSet<&str> = summed.iter().map(String::as_str).collect();
    let have: BTreeSet<&str> = listed.iter().map(|(_, name)| *name).collect();
    problems.extend(
        want.difference(&have)
            .map(|n| format!("{SUMS} does not list {n}")),
    );
    problems.extend(
        have.difference(&want)
            .map(|n| format!("{SUMS} lists {n}, which is not a summed asset of v{version}")),
    );
    for (digest, name) in listed {
        if present.contains(name) {
            let actual = file_sha256(&dir.join(name))?;
            if actual != digest {
                problems.push(format!(
                    "{name}: its SHA-256 is {actual}, {SUMS} says {digest}"
                ));
            }
        }
    }
    Ok(())
}

/// Step 3: `hkctl-<version>` is the script `release hkctl` writes for the version and commit. It
/// is skipped when the asset is absent (step 1 reported that).
fn check_hkctl(
    dir: &Path,
    (version, commit): (&Version, &Commit),
    present: &BTreeSet<String>,
    problems: &mut Vec<String>,
) -> Result<(), Error> {
    let name = hkctl_name(version);
    if !present.contains(&name) {
        return Ok(());
    }
    let path = dir.join(&name);
    let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
    problems.extend(hkctl::problems(
        &String::from_utf8_lossy(&bytes),
        version,
        commit,
    ));
    Ok(())
}

/// Step 5 for one attested asset: verified online, then from its bundle.
fn check_attestations(
    verifier: &dyn Verifier,
    dir: &Path,
    name: &str,
    commit: &Commit,
    problems: &mut Vec<String>,
) -> Result<(), Error> {
    let file = dir.join(name);
    let bundle = dir.join(bundle_of(name));
    let signer = Signer::hostkeeper();
    let digest = file_sha256(&file)?;
    let forms = [("online", None), ("offline", Some(bundle.as_path()))];
    for (form, bundle) in forms {
        let label = format!("{name} ({form})");
        judge(
            &label,
            verifier.verify(&signer, &file, bundle, commit),
            &digest,
            (&signer, commit),
            problems,
        );
    }
    Ok(())
}

/// What is wrong with the answer of the verifier for one form of one asset, one line each: every
/// attestation must name `signer`'s workflow, a ref one of its globs matches, the source commit
/// `commit` and `digest`.
pub(crate) fn judge(
    label: &str,
    answer: Result<Vec<Attestation>, PortError>,
    digest: &str,
    (signer, commit): (&Signer, &Commit),
    problems: &mut Vec<String>,
) {
    let attestations = match answer {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => return problems.push(format!("{label}: no attestation")),
        Err(e) => return problems.push(format!("{label}: {e}")),
    };
    for a in &attestations {
        if a.signer_workflow != signer.workflow {
            problems.push(format!(
                "{label}: signed by {}, not {}",
                a.signer_workflow, signer.workflow
            ));
        }
        if !signer.signers.allows_ref(&a.source_ref) {
            problems.push(format!(
                "{label}: attests the ref {}, not {}",
                a.source_ref,
                signer.refs_text()
            ));
        }
        if a.source_commit != commit.as_str() {
            problems.push(format!(
                "{label}: attests the commit {}, not the pin {commit}",
                a.source_commit
            ));
        }
        if !a.subjects.iter().any(|s| s == digest) {
            problems.push(format!(
                "{label}: does not name the file's SHA-256 {digest}"
            ));
        }
    }
}

#[cfg(test)]
mod tests;
