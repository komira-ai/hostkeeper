//! The outputs of a run (`--output`): each path the Command names in its `output_paths` is a file
//! the farm's result must hold, which is read from its CAS and written into the checkout at the
//! same path, so a workflow's next step finds the file where a build on the runner would have
//! left it.
//!
//! An output is written to a temporary name beside its path while it streams in (a large binary
//! is never held whole), held to the digest the result names (its SHA-256 and its size), made
//! executable when the result says it is (mode 0755, else 0644), and only then renamed onto its
//! path. A path the result does not hold as a file (it is missing, a directory or a symlink), a
//! result that names a file nobody asked for, and a blob that is not its digest each fail the
//! run, before any file is renamed into place.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::client::{Farm, FarmError};
use super::reapi::v2::{ActionResult, Digest};

/// The suffix of an output's temporary name while it is written.
pub(crate) const PART: &str = ".mac-farm-part";

/// Why the outputs could not be written.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OutputError {
    #[error("the farm's result has no file {0} (the run asked for it with --output)")]
    Missing(String),
    #[error("the farm's result has {0} as a {1}, and --output takes files only")]
    NotAFile(String, &'static str),
    #[error("the farm's result has the file {0}, which no --output names")]
    Unasked(String),
    #[error("the farm's result names {0} twice")]
    Twice(String),
    #[error("the output {path} has no digest in the farm's result")]
    NoDigest { path: String },
    #[error("cannot {what}: {why}")]
    Io { what: String, why: String },
    #[error(
        "the output {path} read from the farm is not its digest: {got}, the result says {want}"
    )]
    Digest {
        path: String,
        got: String,
        want: String,
    },
    #[error("the output {path} could not be read from the farm: {farm}")]
    Farm { path: String, farm: FarmError },
}

fn io(what: String) -> impl FnOnce(std::io::Error) -> OutputError {
    move |e| OutputError::Io {
        what,
        why: e.to_string(),
    }
}

/// The digest, executable bit and path of each output `wanted` names in `result`, checked
/// against what the result holds (see the module).
///
/// # Errors
/// [`OutputError`] for the first output the result does not hold as one file, or a file it holds
/// that no output names.
pub(crate) fn files(
    wanted: &[String],
    result: &ActionResult,
) -> Result<BTreeMap<String, (Digest, bool)>, OutputError> {
    let mut held = BTreeMap::new();
    for file in &result.output_files {
        let digest = file.digest.clone().ok_or_else(|| OutputError::NoDigest {
            path: file.path.clone(),
        })?;
        if !wanted.contains(&file.path) {
            return Err(OutputError::Unasked(file.path.clone()));
        }
        if held
            .insert(file.path.clone(), (digest, file.is_executable))
            .is_some()
        {
            return Err(OutputError::Twice(file.path.clone()));
        }
    }
    #[allow(
        deprecated,
        reason = "a server of REAPI before 2.1 answers a directory output in output_directories \
                  and a symlink in output_file_symlinks; 2.1 and later in output_symlinks"
    )]
    let others = result
        .output_directories
        .iter()
        .map(|d| (d.path.as_str(), "directory"))
        .chain(
            result
                .output_symlinks
                .iter()
                .chain(&result.output_file_symlinks)
                .chain(&result.output_directory_symlinks)
                .map(|s| (s.path.as_str(), "symlink")),
        );
    for (path, kind) in others {
        if wanted.iter().any(|w| w == path) {
            return Err(OutputError::NotAFile(path.to_owned(), kind));
        }
    }
    for path in wanted {
        if !held.contains_key(path) {
            return Err(OutputError::Missing(path.clone()));
        }
    }
    Ok(held)
}

/// A file that hashes what is written to it.
struct Hashing {
    file: std::fs::File,
    hash: Sha256,
    size: u64,
}

impl Write for Hashing {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.hash.update(&buf[..n]);
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// The temporary name of the output at `path`.
pub(crate) fn part(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(PART);
    PathBuf::from(name)
}

/// Reads the output `rel` (`digest`, executable or not) from `farm` into `root`, below its
/// temporary name; the temporary file, held to `digest`.
async fn fetch(
    farm: &Farm,
    root: &Path,
    rel: &str,
    digest: &Digest,
    executable: bool,
) -> Result<PathBuf, OutputError> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = root.join(rel);
    let parent = path.parent().unwrap_or(root);
    std::fs::create_dir_all(parent).map_err(io(format!("create {}", parent.display())))?;
    let part = part(&path);
    let file = std::fs::File::create(&part).map_err(io(format!("create {}", part.display())))?;
    let mut out = Hashing {
        file,
        hash: Sha256::new(),
        size: 0,
    };
    farm.read_into(digest, &mut out)
        .await
        .map_err(|farm| OutputError::Farm {
            path: rel.to_owned(),
            farm,
        })?;
    out.flush()
        .map_err(io(format!("write {}", part.display())))?;
    let got = format!("{}/{}", hex::encode(out.hash.finalize()), out.size);
    let want = format!("{}/{}", digest.hash, digest.size_bytes);
    if got != want {
        return Err(OutputError::Digest {
            path: rel.to_owned(),
            got,
            want,
        });
    }
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(&part, std::fs::Permissions::from_mode(mode))
        .map_err(io(format!("set the mode of {}", part.display())))?;
    Ok(part)
}

/// Writes each of `files` (what [`files`] gave) from `farm` into `root` at its path; a line for
/// each. Every output is read and checked before the first is renamed onto its path, so a
/// failure leaves no output of this run in place (a temporary file of one may be left).
///
/// # Errors
/// [`OutputError`] for the first output that cannot be read, is not its digest, or cannot be
/// written.
pub(crate) async fn download(
    farm: &Farm,
    root: &Path,
    files: &BTreeMap<String, (Digest, bool)>,
) -> Result<Vec<String>, OutputError> {
    let mut parts = Vec::new();
    for (rel, (digest, executable)) in files {
        parts.push((
            rel,
            digest,
            fetch(farm, root, rel, digest, *executable).await?,
        ));
    }
    let mut lines = Vec::new();
    for (rel, digest, part) in parts {
        let path = root.join(rel);
        std::fs::rename(&part, &path).map_err(io(format!("rename {}", part.display())))?;
        lines.push(format!(
            "mac-farm: wrote the output {rel} ({} bytes)",
            digest.size_bytes
        ));
    }
    Ok(lines)
}
