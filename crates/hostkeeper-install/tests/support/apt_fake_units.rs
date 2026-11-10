//! The fake's `systemctl show`: what systemd says of a unit, read from the watched unit
//! directory with a link's semantics and from the trees of the installed packages.

use std::fs;
use std::path::PathBuf;

use hostkeeper_install::repository::{Run, ToolOutput};

use super::{Deb, State, out};

/// `systemctl show --property=LoadState,FragmentPath -- <unit>` as systemd answers it: a file or
/// a link in the unit directory `units` (where it is on disk, how the host names it) comes first,
/// and a link there whose target is gone leaves the unit not found; else the unit file an
/// installed package's tree holds in `usr/lib/systemd/system` or `lib/systemd/system`; else not
/// found.
pub fn show(units: Option<(PathBuf, String)>, s: &State, run: &Run) -> ToolOutput {
    let unit = run.args.last().cloned().unwrap_or_default();
    let answer =
        |load: &str, path: &str| out(0, &format!("LoadState={load}\nFragmentPath={path}\n"), "");
    if let Some((disk, spelled)) = units {
        let at = disk.join(&unit);
        if fs::symlink_metadata(&at).is_ok() {
            return match fs::metadata(&at) {
                Ok(_) => answer("loaded", &format!("{spelled}/{unit}")),
                Err(_) => answer("not-found", ""),
            };
        }
    }
    let installed = |deb: &&Deb| {
        s.installed
            .get(&deb.package)
            .is_some_and(|i| (i.version == deb.version) & (i.state == 'i'))
    };
    let from_package = s.debs.iter().filter(installed).find_map(|deb| {
        ["usr/lib/systemd/system", "lib/systemd/system"]
            .iter()
            .map(|dir| format!("{dir}/{unit}"))
            .find(|path| deb.tree.contains_key(path))
    });
    match from_package {
        Some(path) => answer("loaded", &format!("/{path}")),
        None => answer("not-found", ""),
    }
}
