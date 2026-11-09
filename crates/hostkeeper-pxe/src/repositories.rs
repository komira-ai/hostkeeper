//! The repository files `--repositories` names (packages-repositories.md sections 3.1 and 3.2),
//! read at the start of `serve` as the starting user (root on a host), before any socket or seed.
//!
//! The directory is judged by the daemon's own loader, [`load_as`], as the host will see it at
//! [`REPOSITORIES_DIR`]: a `Signed-By` that names another directory, a key that turns off a check
//! of apt's, a stray file, a link, a file another user owns or the group may write, all refuse the
//! start, as the daemon would refuse its first start on such a host. A directory that is missing
//! or holds no entry is refused too: the flag then names nothing an install could use.
//!
//! The files are then read for the seeds: each `.sources` file as the loader saw it, and each
//! keyring named by an entry, opened without following a link and judged on the open file by the
//! loader's rule, up to [`MAX_KEYRING_BYTES`]. A keyring is binary and is not parsed here: apt
//! reads it on the host.

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_install::repository::config::{
    REPOSITORIES_DIR, SOURCES_SUFFIX, load_as, read_file,
};
use hostkeeper_pxe_seed::inputs::RepositoryFiles;

/// The most bytes of one keyring read: 1 MiB. A keyring of an archive is some tens of KiB, and
/// every byte of it is carried in every seed as base64, so a larger file is a mistake to refuse
/// at the start rather than one to send to every machine.
pub const MAX_KEYRING_BYTES: u64 = 1024 * 1024;

/// The files of `dir`, owned by `owner`: each `.sources` file and keyring, once the loader accepts
/// the directory as the host's [`REPOSITORIES_DIR`]. The error is the line the command prints
/// after its name: `<path>: <reason>`.
///
/// # Errors
/// The loader's refusal naming the file, a directory that is missing or holds no entry, a keyring
/// that cannot be read or is over [`MAX_KEYRING_BYTES`], or a file name a seed cannot carry.
pub(crate) fn read(dir: &Path, owner: u32) -> Result<RepositoryFiles, String> {
    let repositories =
        load_as(dir, Path::new(REPOSITORIES_DIR), owner).map_err(|e| e.to_string())?;
    if repositories.is_empty() {
        return Err(format!(
            "{}: the directory is missing or holds no repository entry (a .sources file)",
            dir.display()
        ));
    }
    let entries: Vec<_> = repositories
        .names()
        .into_iter()
        .filter_map(|name| repositories.get(name))
        .collect();
    let mut files: Vec<(String, Vec<u8>)> = entries
        .iter()
        .map(|e| {
            (
                format!("{}{SOURCES_SUFFIX}", e.name),
                e.text.clone().into_bytes(),
            )
        })
        .collect();
    let keyrings: BTreeSet<String> = entries
        .iter()
        .filter_map(|e| e.keyring.file_name()?.to_str().map(str::to_owned))
        .collect();
    for file in keyrings {
        let path = dir.join(&file);
        let bytes = read_file(&path, owner, MAX_KEYRING_BYTES)
            .map_err(|fault| format!("{}: {fault}", path.display()))?;
        files.push((file, bytes));
    }
    RepositoryFiles::new(files).map_err(|e| format!("{}: {e}", dir.display()))
}
