//! Which archive `hk xcode add` takes (docs/design/xcode-one-action.md section 2.2).
//!
//! `hk` picks a file up on its own only when its name is `Xcode`, then `_` or `-`, then a version
//! (decimal numbers joined by dots), then `.xip`, and nothing else: `Xcode_27.2.xip`,
//! `Xcode-26.1.1.xip`. A name with anything more is never picked up, only given as a path. When
//! versions are compared, trailing `.0` parts do not count, so `27.2` is `27.2.0`. The name is only
//! a hint: the version read from the expanded app decides.
//!
//! - [`name_version`] and [`named`]: the version a name carries, as written and as numbers; off
//!   macOS, where the archive cannot be expanded to read its version, the name is where it comes
//!   from when none is given;
//! - [`in_downloads`]: the file named for a version in `~/Downloads` or in
//!   `~/Downloads/hk-xcode-push/<version>/`, the newest by modification time;
//! - [`newest`]: the file directly in `~/Downloads` named for the highest version, then the newest
//!   by modification time;
//! - [`download`]: `xcodes download <version> --directory ~/Downloads/hk-xcode-push/<version>`
//!   in the operator's own session, then the file in that directory named for the version.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::SystemTime;

use crate::World;
use crate::xcode_push::archive::is_version;
use crate::xcode_push::{Mac, command, failed, say};
use crate::{Flow, PROGRAM};

/// The directory of `$HOME` that archives are downloaded to and looked for in.
pub(crate) const DOWNLOADS: &str = "Downloads";

/// The directory of `~/Downloads` that `xcodes download` is told to use, one per version.
const DOWNLOAD_DIR: &str = "hk-xcode-push";

/// The numbers of the version `text`, with the trailing zeros dropped; `None` when it is not a
/// version or a number does not fit.
pub(crate) fn parts(text: &str) -> Option<Vec<u64>> {
    if !is_version(text.as_bytes()) {
        return None;
    }
    let mut numbers: Vec<u64> = text
        .split('.')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    while numbers.last() == Some(&0) {
        numbers.pop();
    }
    Some(numbers)
}

/// The version an archive's file name is named for, as written in it (`27.2`); `None` when the
/// name is not `Xcode`, `_` or `-`, a version and `.xip`.
pub(crate) fn name_version(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("Xcode")?;
    let rest = rest.strip_prefix(['_', '-'])?;
    let version = rest.strip_suffix(".xip")?;
    parts(version).map(|_| version)
}

/// The version an archive's file name is named for, as [`parts`] reads it; `None` when the name
/// is not `Xcode`, `_` or `-`, a version and `.xip`.
pub(crate) fn named(name: &str) -> Option<Vec<u64>> {
    parts(name_version(name)?)
}

/// A file named like an archive.
struct Candidate {
    path: PathBuf,
    version: Vec<u64>,
    modified: SystemTime,
}

/// The files directly in `dir` that are named like an archive; none when it cannot be read.
fn candidates(dir: &Path) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = named(&entry.file_name().to_string_lossy())?;
            let meta = std::fs::metadata(entry.path()).ok()?;
            meta.is_file().then(|| Candidate {
                path: entry.path(),
                version,
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            })
        })
        .collect()
}

/// The newest by modification time among `found` (the later name when two are as new).
fn newest_of(found: impl Iterator<Item = Candidate>) -> Option<PathBuf> {
    found
        .max_by(|a, b| {
            a.modified
                .cmp(&b.modified)
                .then_with(|| a.path.cmp(&b.path))
        })
        .map(|c| c.path)
}

/// The archive named for `version` in `~/Downloads` or in `~/Downloads/hk-xcode-push/<version>`,
/// the newest by modification time.
pub(crate) fn in_downloads(downloads: &Path, version: &str) -> Option<PathBuf> {
    let wanted = parts(version)?;
    let dirs = [
        downloads.to_path_buf(),
        downloads.join(DOWNLOAD_DIR).join(version),
    ];
    newest_of(
        dirs.iter()
            .flat_map(|dir| candidates(dir))
            .filter(|c| c.version == wanted),
    )
}

/// The archive directly in `~/Downloads` named for the highest version, then the newest by
/// modification time.
pub(crate) fn newest(downloads: &Path) -> Option<PathBuf> {
    let found = candidates(downloads);
    let highest = found.iter().map(|c| c.version.clone()).max()?;
    newest_of(found.into_iter().filter(|c| c.version == highest))
}

/// What a missing `xcodes` is told with: the two ways out.
fn missing(xcodes: &Path, version: &str) -> String {
    format!(
        "{} is not installed, so Xcode {version} cannot be downloaded here; either install it \
         (brew install xcodesorg/made/xcodes), or download the archive in a browser from Apple's \
         developer downloads page (https://developer.apple.com/download/all/) into ~/Downloads \
         and run {PROGRAM} xcode add {version} again",
        xcodes.display()
    )
}

/// Downloads `version` with `xcodes` in the operator's own session (its standard streams are this
/// process's, so that a two-factor prompt is answered at the terminal) into
/// `~/Downloads/hk-xcode-push/<version>`; the archive in it named for the version, the newest.
pub(crate) fn download(
    w: &mut World<'_>,
    mac: &Mac,
    downloads: &Path,
    version: &str,
) -> Flow<PathBuf> {
    let dir = downloads.join(DOWNLOAD_DIR).join(version);
    std::fs::create_dir_all(&dir)
        .map_err(|error| failed(&format!("cannot make {} ({error})", dir.display())))?;
    say(
        w,
        &format!(
            "downloading Xcode {version} with {} into {}",
            mac.xcodes.display(),
            dir.display()
        ),
    );
    let mut xcodes = command(&mac.xcodes);
    xcodes
        .arg("download")
        .arg(version)
        .arg("--directory")
        .arg(&dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let what = format!("{} download {version}", mac.xcodes.display());
    match xcodes.status() {
        Ok(status) if status.success() => {}
        Ok(status) => return Err(failed(&format!("{what} failed ({status})"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(failed(&missing(&mac.xcodes, version)));
        }
        Err(error) => {
            return Err(failed(&format!(
                "cannot run {} ({error})",
                mac.xcodes.display()
            )));
        }
    }
    let wanted = parts(version);
    newest_of(
        candidates(&dir)
            .into_iter()
            .filter(|c| Some(&c.version) == wanted.as_ref()),
    )
    .ok_or_else(|| {
        failed(&format!(
            "{what} left no Xcode_{version}.xip or Xcode-{version}.xip in {}",
            dir.display()
        ))
    })
}
