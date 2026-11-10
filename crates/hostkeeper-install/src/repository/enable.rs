//! The enablement of a repository package's `run` units (packages-repositories.md section 5):
//! each `activate` writes the `WantedBy=` and `RequiredBy=` links of every unit whose unit file
//! is in the package's tree and not yet enabled, records them, and takes away the recorded links
//! of a unit the version no longer runs, so those units start at host boot. `remove` takes every
//! recorded link away. A link already there that hostkeeper did not write (the package's own
//! scripts, an operator) is left as it is and never recorded, so it is never removed. A run
//! unit the tree gives no link for ([`unlinked`]) is started by hostkeeper itself.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::{Path, PathBuf};

use super::tree::{UNIT_DIRS, install_links, unit_file};

/// The service manager's directory of enablement links, relative to the host's root.
pub const SYSTEMD_DIR: &str = "etc/systemd/system";

/// The links the `run` units of `tree` ask for: each link's absolute path on the host, and the
/// unit file it points at, absolute, where the package installs it. A unit with no unit file in
/// the tree, or no link to ask for, adds none.
pub fn wanted(tree: &Path, run: &[String]) -> io::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for unit in run {
        let Some(rel) = unit_file(tree, unit) else {
            continue;
        };
        let text = fs::read(tree.join(&rel))?;
        for link in install_links(unit, &String::from_utf8_lossy(&text)) {
            out.insert(format!("/{SYSTEMD_DIR}/{link}"), format!("/{rel}"));
        }
    }
    Ok(out)
}

/// The `run` units, in order, for which `tree` gives no enablement link: hostkeeper starts them
/// itself. A unit with no unit file in the tree, or whose unit file has no `[Install]` target
/// that is a unit's name, is one. A tree that cannot be read gives no link either.
pub fn unlinked(tree: &Path, run: &[String]) -> Vec<String> {
    run.iter()
        .filter(|unit| {
            let Some(rel) = unit_file(tree, unit) else {
                return true;
            };
            fs::read(tree.join(rel))
                .map(|text| install_links(unit, &String::from_utf8_lossy(&text)).is_empty())
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// The links of `wanted` not on the host yet: the ones an `apply` would write.
pub fn missing(root: &Path, wanted: &BTreeMap<String, String>) -> BTreeSet<String> {
    wanted
        .keys()
        .filter(|link| fs::symlink_metadata(real(root, link)).is_err())
        .cloned()
        .collect()
}

/// `path`, an absolute path of the host, under `root`.
fn real(root: &Path, path: &str) -> PathBuf {
    root.join(path.trim_start_matches('/'))
}

/// Whether the link at `real` points at `target`.
fn points_at(real: &Path, target: &str) -> bool {
    fs::read_link(real).is_ok_and(|found| found == Path::new(target))
}

/// Writes the links of `wanted` that are missing, takes away the links of `recorded` that are
/// not wanted any more, and answers the links hostkeeper owns now: each wanted link it wrote
/// now or had recorded and still finds.
pub fn apply(
    root: &Path,
    wanted: &BTreeMap<String, String>,
    recorded: &BTreeSet<String>,
) -> io::Result<BTreeSet<String>> {
    for gone in recorded.iter().filter(|link| !wanted.contains_key(*link)) {
        remove_link(root, gone)?;
    }
    let mut owned = BTreeSet::new();
    for (link, target) in wanted {
        let path = real(root, link);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                if recorded.contains(link) && points_at(&path, target) {
                    owned.insert(link.clone());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o755)
                    .create(path.parent().unwrap_or(root))?;
                symlink(target, &path)?;
                owned.insert(link.clone());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(owned)
}

/// Whether `target`, a link's target, is the unit file `unit` where a package installs it: what
/// hostkeeper writes, so a link pointing anywhere else is not its own.
fn is_unit_target(target: &Path, unit: &str) -> bool {
    UNIT_DIRS
        .iter()
        .any(|dir| target == Path::new(&format!("/{dir}/{unit}")))
}

/// Takes away the recorded link `link` while it is still the link hostkeeper writes, to the
/// unit file it is named for; anything else at its path (a file, a link an operator pointed
/// elsewhere) is not hostkeeper's and stays.
pub fn remove_link(root: &Path, link: &str) -> io::Result<()> {
    let path = real(root, link);
    let unit = Path::new(link)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    match fs::symlink_metadata(&path) {
        Ok(meta)
            if meta.file_type().is_symlink()
                && fs::read_link(&path).is_ok_and(|target| is_unit_target(&target, unit)) =>
        {
            fs::remove_file(&path)
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
