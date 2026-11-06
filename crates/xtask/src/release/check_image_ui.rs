//! `release check-image-ui`: the coordinator's image holds the operator web app at
//! [`IMAGE_UI_DIR`], exactly the files of the commit's `ui/`.
//!
//! The image is read as `compare-images` reads it: the OCI image layout of the tarball, the
//! first manifest of its index, and that manifest's layers. Each layer is a tar, gzip-compressed
//! or not, and the layers are applied in order: an entry replaces what an earlier layer had at
//! its path, and a whiteout (`.wh.<name>`) or an opaque marker (`.wh..wh..opq`) removes what an
//! earlier layer had below it. In the file system that results, [`IMAGE_UI_DIR`] must be a
//! directory holding exactly the directories and files of `ui/`, each file with the bytes of
//! `ui/`'s, every entry owned by root (uid and gid 0), each file of mode 0644 and each directory
//! of mode 0755, so the coordinator's non-root user reads the files and can change none.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use super::Error;
use super::compare_images::{layers, tar_files};
use super::contract::IMAGE_UI_DIR;

/// The mode of each file of the web app in the image.
const FILE_MODE: u32 = 0o644;

/// The mode of each directory of the web app in the image.
const DIR_MODE: u32 = 0o755;

/// The marker of a whiteout entry of a layer.
const WHITEOUT: &str = ".wh.";

/// The name of a layer's opaque marker: what earlier layers had in its directory is gone.
const OPAQUE: &str = ".wh..wh..opq";

/// What is at a path of the image's file system, or of `ui/`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Dir,
    File(Vec<u8>),
    /// A link, a device or anything else that is neither.
    Other,
}

/// An entry of the image's file system.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    kind: Kind,
    mode: u32,
    owner: (u64, u64),
}

/// The image's file system by path, without a leading `/` or `./` and without a trailing `/`.
type Tree = BTreeMap<String, Node>;

/// Passes when the image tarball `image` holds the web app of the directory `ui` at
/// [`IMAGE_UI_DIR`], as the module says; otherwise fails with every difference, one per line.
pub(crate) fn check_image_ui(image: &Path, ui: &Path) -> Result<Vec<String>, Error> {
    let bytes = std::fs::read(image).map_err(Error::io(format!("read {}", image.display())))?;
    let tree =
        image_tree(&bytes).map_err(|why| Error::Failed(format!("{}: {why}", image.display())))?;
    let mut want = BTreeMap::new();
    walk(ui, "", &mut want)?;
    let problems = differences(&tree, &want, ui);
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    let files = want.values().filter(|k| matches!(k, Kind::File(_))).count();
    Ok(vec![format!(
        "{}: {IMAGE_UI_DIR} holds the {files} files of {}, owned by root, files {FILE_MODE:o} and directories {DIR_MODE:o}",
        image.display(),
        ui.display()
    )])
}

/// The file system the layers of the image tarball `bytes` make, applied in order.
fn image_tree(bytes: &[u8]) -> Result<Tree, String> {
    let files = tar_files(bytes)?;
    let mut tree = Tree::new();
    for (n, digest) in layers(&files)?.iter().enumerate() {
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or_else(|| format!("layer {n}: {digest:?} is not a sha256 digest"))?;
        let blob = files
            .get(&format!("blobs/sha256/{hex}"))
            .ok_or_else(|| format!("layer {n}: blobs/sha256/{hex} is missing"))?;
        apply(&mut tree, blob).map_err(|why| format!("layer {n}: {why}"))?;
    }
    Ok(tree)
}

/// Applies the layer `blob` (a tar, gzip-compressed when it starts with gzip's magic bytes) to
/// `tree`.
fn apply(tree: &mut Tree, blob: &[u8]) -> Result<(), String> {
    let unreadable = |_: std::io::Error| "it is not a readable tar archive".to_owned();
    let reader: Box<dyn Read + '_> = if blob.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(blob))
    } else {
        Box::new(blob)
    };
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().map_err(unreadable)? {
        let mut entry = entry.map_err(unreadable)?;
        let raw = entry
            .path()
            .map_err(unreadable)?
            .to_string_lossy()
            .into_owned();
        let path = raw
            .trim_start_matches("./")
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_owned();
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        if name == OPAQUE {
            remove_below(tree, parent);
            continue;
        }
        if let Some(hidden) = name.strip_prefix(WHITEOUT) {
            let gone = if parent.is_empty() {
                hidden.to_owned()
            } else {
                format!("{parent}/{hidden}")
            };
            tree.remove(&gone);
            remove_below(tree, &gone);
            continue;
        }
        let header = entry.header();
        let mode = header.mode().map_err(unreadable)? & 0o7777;
        let owner = (
            header.uid().map_err(unreadable)?,
            header.gid().map_err(unreadable)?,
        );
        let kind = match header.entry_type() {
            tar::EntryType::Directory => Kind::Dir,
            tar::EntryType::Regular => {
                let mut data = Vec::new();
                entry.read_to_end(&mut data).map_err(unreadable)?;
                Kind::File(data)
            }
            _ => Kind::Other,
        };
        tree.insert(path, Node { kind, mode, owner });
    }
    Ok(())
}

/// Removes every path of `tree` below the directory `dir` (the root when it is empty).
fn remove_below(tree: &mut Tree, dir: &str) {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    tree.retain(|path, _| !path.starts_with(&prefix) || path.as_str() == dir);
}

/// The directories and files below `dir`, by their path from the web app's root (`relative`),
/// into `want`.
fn walk(dir: &Path, relative: &str, want: &mut BTreeMap<String, Kind>) -> Result<(), Error> {
    let entries = std::fs::read_dir(dir).map_err(Error::io(format!("read {}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(Error::io(format!("read {}", dir.display())))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let below = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let kind = entry
            .file_type()
            .map_err(Error::io(format!("read {}", path.display())))?;
        if kind.is_dir() {
            want.insert(below.clone(), Kind::Dir);
            walk(&path, &below, want)?;
        } else if kind.is_file() {
            let data =
                std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
            want.insert(below, Kind::File(data));
        } else {
            return Err(Error::Failed(format!(
                "{} is neither a file nor a directory: the web app is files and directories only",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Every way the image's `tree` differs from the web app `want` read from `ui`, in path order.
fn differences(tree: &Tree, want: &BTreeMap<String, Kind>, ui: &Path) -> Vec<String> {
    let root = IMAGE_UI_DIR.trim_start_matches('/');
    let at = |relative: &str| {
        if relative.is_empty() {
            IMAGE_UI_DIR.to_owned()
        } else {
            format!("{IMAGE_UI_DIR}/{relative}")
        }
    };
    let mut problems = Vec::new();
    match tree.get(root) {
        Some(node) if node.kind == Kind::Dir => problems.extend(owner_and_mode(&at(""), node)),
        _ => {
            return vec![format!("the image has no directory {IMAGE_UI_DIR}")];
        }
    }
    let prefix = format!("{root}/");
    let held: BTreeMap<&str, &Node> = tree
        .iter()
        .filter_map(|(path, node)| path.strip_prefix(&prefix).map(|rest| (rest, node)))
        .collect();
    for (relative, kind) in want {
        let path = at(relative);
        let source = ui.join(relative);
        match (kind, held.get(relative.as_str())) {
            (_, None) => problems.push(format!(
                "{path} is missing: {} is not in the image",
                source.display()
            )),
            (Kind::Dir, Some(node)) if node.kind != Kind::Dir => {
                problems.push(format!(
                    "{path} is not a directory, as {} is",
                    source.display()
                ));
            }
            (Kind::File(bytes), Some(node)) if !matches!(&node.kind, Kind::File(held) if held == bytes) =>
            {
                problems.push(format!(
                    "{path} is not a file with the bytes of {}",
                    source.display()
                ));
            }
            (_, Some(node)) => problems.extend(owner_and_mode(&path, node)),
        }
    }
    for relative in held.keys().filter(|r| !want.contains_key(**r)) {
        problems.push(format!(
            "{} is in the image but not in {}",
            at(relative),
            ui.display()
        ));
    }
    problems
}

/// What is wrong with the owner and the mode of `node`, the entry of the image at `path`.
fn owner_and_mode(path: &str, node: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if node.owner != (0, 0) {
        problems.push(format!(
            "{path} is owned by {}:{}, not by root (0:0)",
            node.owner.0, node.owner.1
        ));
    }
    let want = if node.kind == Kind::Dir {
        DIR_MODE
    } else {
        FILE_MODE
    };
    if node.mode != want {
        problems.push(format!("{path} has mode {:04o}, not {want:04o}", node.mode));
    }
    problems
}

#[cfg(test)]
mod tests;
