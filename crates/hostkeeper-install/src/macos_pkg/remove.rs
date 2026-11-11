//! `remove` and `retire` (packages-repositories.md section 4.7). `remove` takes the whole
//! service off the host, on uninstall or when a first install fails: closed-world removal, as
//! `dpkg --purge`. Every file and link (no directory) that the receipt or the `Bom` of a kept
//! version lists is deleted whatever its content now, but one a receipt of another identifier
//! also lists, and never through a link in a parent; then each directory hostkeeper's installs
//! created, left empty, deepest first; then the receipt is forgotten. Files the receipt does not
//! list (what the package's scripts made, the service's data) stay. `retire` deletes a version
//! directory that is neither `current` nor `previous`, and never runs a tool.

use std::collections::BTreeSet;
use std::path::Path;

use super::activate::files_of;
use super::{MacosPkg, Order, TREE, io, receipt};
use crate::host::{self, Kind, remove_tree};
use crate::outcome::{Failure, Reason};
use crate::tools::{Tool, may_be_cut};

impl MacosPkg<'_> {
    /// Removes `order`'s service; `kept` are the version directories still on the host
    /// (`current` and `previous`). Run again, it finds nothing left to do.
    pub fn remove(&self, order: &Order, kept: &[&Path]) -> Result<(), Failure> {
        let deadline = order.bounds.remove;
        let ids = *self.host.ids();
        let mut paths = BTreeSet::new();
        for dir in kept {
            let tree = host::walk(&dir.join(TREE), &ids).map_err(|e| io("a kept tree", e))?;
            paths.extend(files_of(&tree));
        }
        let _held = self.lock.hold().map_err(|e| io("the installer lock", e))?;
        let found = self.receipt(order, deadline)?;
        if let Some(found) = &found {
            let out = self.run(
                Tool::Pkgutil,
                &receipt::files_args(&order.identifier),
                deadline,
            )?;
            let files = match out.code {
                Some(0) => receipt::parse_files(&out.stdout, may_be_cut(&out.stdout))
                    .map_err(|e| Failure::new(Reason::IoError, format!("pkgutil --files {e}")))?,
                code => {
                    return Err(Failure::new(
                        Reason::IoError,
                        format!("pkgutil --files exited {code:?}"),
                    ));
                }
            };
            paths.extend(files.iter().map(|file| found.path_of(file)));
        }
        for path in &paths {
            let Some(meta) = self.host.meta(path).map_err(|e| io(path, e))? else {
                continue;
            };
            if meta.kind == Kind::Dir || self.host.through_link(path).map_err(|e| io(path, e))? {
                continue;
            }
            let shared = self
                .owners(path, deadline)?
                .iter()
                .any(|owner| *owner != order.identifier);
            if !shared {
                self.host.delete_file(path).map_err(|e| io(path, e))?;
            }
        }
        let record = self
            .records
            .load(&order.service)
            .map_err(|e| Failure::new(Reason::IoError, e.to_string()))?;
        for dir in record.iter().flat_map(|r| r.created_dirs.iter().rev()) {
            if !self.host.through_link(dir).map_err(|e| io(dir, e))? {
                self.host.delete_empty_dir(dir).map_err(|e| io(dir, e))?;
            }
        }
        if found.is_some() {
            let out = self.run(
                Tool::Pkgutil,
                &receipt::forget_args(&order.identifier),
                deadline,
            )?;
            if out.code != Some(0) {
                return Err(Failure::new(
                    Reason::IoError,
                    format!(
                        "pkgutil --forget exited {:?}: {}",
                        out.code,
                        out.stderr_text().trim()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Deletes the version directory `dir` and its seal (`<dir>.seal` beside it). No tool runs.
    pub fn retire(&self, dir: &Path) -> Result<(), Failure> {
        remove_tree(dir).map_err(|e| io("the version directory", e))?;
        let mut seal = dir.as_os_str().to_owned();
        seal.push(".seal");
        remove_tree(Path::new(&seal)).map_err(|e| io("the version's seal", e))
    }
}
