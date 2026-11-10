//! The repository files `--repositories` names (packages-repositories.md sections 3.1 and 3.2),
//! read at the start of `serve` as the starting user (root on a host), before any socket or seed.
//!
//! The directory is judged by the daemon's own loader, [`load_with_keyrings`], as the host will
//! see it at [`REPOSITORIES_DIR`]: a `Signed-By` that names another directory, a key that turns
//! off a check of apt's, a stray file, a link, a file another user owns or the group may write, a
//! keyring apt cannot read at its name, all refuse the start, as the daemon would refuse its
//! first start on such a host. A directory that is missing or holds no entry is refused too: the
//! flag then names nothing an install could use.
//!
//! The seeds carry the files as the loader read them: each `.sources` file's text, and each
//! keyring named by an entry, which the loader opened without following a link, judged on the
//! open file, read up to [`MAX_KEYRING_BYTES`] (1 MiB: every byte of a keyring is carried in
//! every seed as base64, so a larger file is a mistake to refuse at the start) and refused when
//! it is not what apt reads at its name (armor for `.asc`, a binary keyring otherwise). A
//! keyring is not parsed further here: apt reads it on the host.

use std::ffi::OsStr;
use std::path::Path;

#[cfg(doc)]
use hostkeeper_install::repository::config::MAX_KEYRING_BYTES;
use hostkeeper_install::repository::config::{
    REPOSITORIES_DIR, SOURCES_SUFFIX, load_with_keyrings,
};
use hostkeeper_pxe_seed::inputs::RepositoryFiles;

/// The files of `dir`, owned by `owner`: each `.sources` file and keyring, once the loader accepts
/// the directory as the host's [`REPOSITORIES_DIR`]. The error is the line the command prints
/// after its name: `<path>: <reason>`.
///
/// # Errors
/// The loader's refusal naming the file (a keyring that cannot be read, is over
/// [`MAX_KEYRING_BYTES`] or is not of its name's kind among them), a directory that is missing or holds no
/// entry, or a file name a seed cannot carry.
pub(crate) fn read(dir: &Path, owner: u32) -> Result<RepositoryFiles, String> {
    let loaded =
        load_with_keyrings(dir, Path::new(REPOSITORIES_DIR), owner).map_err(|e| e.to_string())?;
    let repositories = &loaded.repositories;
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
    files.extend(loaded.keyrings.iter().filter_map(|(path, bytes)| {
        path.file_name()
            .and_then(OsStr::to_str)
            .map(|file| (file.to_owned(), bytes.clone()))
    }));
    RepositoryFiles::new(files).map_err(|e| format!("{}: {e}", dir.display()))
}
