//! `release check <dir> --version V --commit C`: checks a directory of release assets without
//! changing it, and lists every problem it finds (one line each), in this order:
//!
//! 1. the regular files of the directory are exactly the assets of the version
//!    ([`asset_names`]);
//! 2. `SHA256SUMS` has one `<sha256>  <name>` line per summed asset ([`summed_names`]), and each
//!    digest is that of the file;
//! 3. each `hk-<version>-<platform>.tar.gz` holds exactly two members: the regular file `hk` and
//!    the symbolic link `hkctl` whose target is `hk` ([`hk_tarball_problems`]);
//! 4. the coordinator's image index is the one its two image tarballs make, byte for byte
//!    ([`super::image_index`]): its SHA-256 is the digest of the image a registry serves;
//! 5. each attested asset verifies twice, online and from its bundle, and every attestation
//!    returned names this repository's signer workflow and ref, the pinned commit, and the
//!    SHA-256 of the file. Every verification is asked for at once ([`Verifier::verify_each`]),
//!    so the process runner runs them side by side; the problems are listed in the order of the
//!    assets all the same.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use tar::EntryType;

use super::contract::{
    Commit, HK, HKCTL, Platform, SUMS, Signer, Version, asset_names, attested_names, bundle_of,
    summed_names,
};
use super::image_index;
use super::ports::{Ask, Attestation, PortError, Verifier};
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
    check_hk_tarballs(dir, version, &present, &mut problems);
    check_image_index(dir, version, &present, &mut problems);
    let attested = attested_names(version);
    let names: Vec<&String> = attested.iter().filter(|n| present.contains(*n)).collect();
    check_attestations(verifier, dir, &names, commit, &mut problems)?;
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

/// Step 3: each `hk` tarball holds the program and the link of its old name, nothing else.
/// A tarball that is absent is skipped (step 1 reported that); one that cannot be read is listed.
fn check_hk_tarballs(
    dir: &Path,
    version: &Version,
    present: &BTreeSet<String>,
    problems: &mut Vec<String>,
) {
    for platform in Platform::RELEASED {
        let name = format!("{HK}-{version}-{}.tar.gz", platform.name());
        if !present.contains(&name) {
            continue;
        }
        let path = dir.join(&name);
        let listing = std::fs::File::open(&path)
            .map_err(|e| e.to_string())
            .and_then(|file| members(file).map_err(|e| e.to_string()));
        match listing {
            Ok(found) if found == expected_members() => {}
            Ok(found) => problems.push(format!(
                "{name} does not hold exactly the file `{HK}` and the symbolic link `{HKCTL}` to \
                 `{HK}` (it holds {})",
                found.join(", ")
            )),
            Err(e) => problems.push(format!("{name} is not a readable gzip tar archive: {e}")),
        }
    }
}

/// What an `hk` tarball holds, as [`members`] lists it, sorted.
fn expected_members() -> Vec<String> {
    vec![format!("{HK} (file)"), format!("{HKCTL} (link to {HK})")]
}

/// The members of the gzip tar archive `file`, each as `<path> (file)`, `<path> (link to <target>)`
/// or `<path> (<other type>)`, sorted.
fn members(file: impl Read) -> std::io::Result<Vec<String>> {
    let mut found = Vec::new();
    for entry in tar::Archive::new(GzDecoder::new(file)).entries()? {
        let entry = entry?;
        let path = entry.path()?.display().to_string();
        let kind = match entry.header().entry_type() {
            EntryType::Regular => "file".to_owned(),
            EntryType::Symlink => {
                let target = entry.link_name()?.map(|t| t.display().to_string());
                format!("link to {}", target.unwrap_or_default())
            }
            other => format!("type {}", other.as_byte() as char),
        };
        found.push(format!("{path} ({kind})"));
    }
    found.sort();
    Ok(found)
}

/// Step 5 for the attested assets `names`: each verified online, then from its bundle, all
/// asked for at once; the problems in the order of `names`, the online form first.
fn check_attestations(
    verifier: &dyn Verifier,
    dir: &Path,
    names: &[&String],
    commit: &Commit,
    problems: &mut Vec<String>,
) -> Result<(), Error> {
    let signer = Signer::hostkeeper();
    let paths: Vec<(String, std::path::PathBuf, std::path::PathBuf)> = names
        .iter()
        .map(|name| ((*name).clone(), dir.join(name), dir.join(bundle_of(name))))
        .collect();
    let mut digests = Vec::with_capacity(paths.len());
    let mut asks = Vec::with_capacity(2 * paths.len());
    let mut labels = Vec::with_capacity(2 * paths.len());
    for (name, file, bundle) in &paths {
        let digest = file_sha256(file)?;
        for (form, bundle) in [("online", None), ("offline", Some(bundle.as_path()))] {
            asks.push(Ask {
                signer: &signer,
                file,
                bundle,
                commit,
            });
            labels.push((format!("{name} ({form})"), digests.len()));
        }
        digests.push(digest);
    }
    let answers = verifier.verify_each(&asks);
    if answers.len() != asks.len() {
        problems.push(format!(
            "the verifier answered {} of the {} verifications asked for",
            answers.len(),
            asks.len()
        ));
    }
    for ((label, digest), answer) in labels.iter().zip(answers) {
        judge(
            label,
            answer,
            &digests[*digest],
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
