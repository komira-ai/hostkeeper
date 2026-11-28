//! The fake's package files on a host, for the tests of the files a first install keeps: with a
//! host root placed ([`super::FakeHost::place_files`]), an install writes the package's files
//! under it, over whatever was there, and refuses, as dpkg does, a file another package owns;
//! a purge deletes them, whatever their content, and the directories it made that are left
//! empty. `dpkg-query --search` answers from the installed packages' trees and the owners a
//! test declares (`State::owners`). Without a placed root, nothing is written or deleted.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_install::repository::{Run, ToolOutput};

use super::{Deb, Node, State, out};

/// The model of `package` at `version`.
fn deb_of<'a>(s: &'a State, package: &str, version: &str) -> Option<&'a Deb> {
    s.debs
        .iter()
        .find(|deb| (deb.package == package) & (deb.version == version))
}

/// The packages that own the logical `path`: a declared owner, then each installed package whose
/// tree holds it.
fn owners_of(s: &State, path: &str) -> Vec<String> {
    let rel = path.trim_start_matches('/');
    let mut owners: Vec<String> = s.owners.get(path).cloned().into_iter().collect();
    for (package, inst) in &s.installed {
        let holds = deb_of(s, package, &inst.version)
            .is_some_and(|deb| matches!(deb.tree.get(rel), Some(Node::File { .. })));
        if holds && !owners.contains(package) {
            owners.push(package.clone());
        }
    }
    owners
}

/// The install's unpack of `package` at `version`: `Err` is dpkg's refusal of a file another
/// package owns, with nothing written; else the files of the version installed before are
/// replaced by this one's.
pub fn unpack(s: &State, package: &str, version: &str) -> Result<(), ToolOutput> {
    let (Some(root), Some(deb)) = (s.files.as_ref(), deb_of(s, package, version)) else {
        return Ok(());
    };
    for (path, node) in &deb.tree {
        let logical = format!("/{path}");
        let other = owners_of(s, &logical).into_iter().find(|o| o != package);
        if let (Node::File { .. }, Some(other)) = (node, other) {
            return Err(out(
                100,
                &format!("Unpacking {package} ({version}) ...\n"),
                &format!(
                    "dpkg: error processing archive {package}.deb (--unpack):\n trying to \
                     overwrite '{logical}', which is also in package {other}\nE: Sub-process \
                     /usr/bin/dpkg returned an error code (1)\n"
                ),
            ));
        }
    }
    if let Some(old) = s
        .installed
        .get(package)
        .and_then(|inst| deb_of(s, package, &inst.version))
    {
        delete(root, old);
    }
    for (path, node) in &deb.tree {
        let at = root.join(path);
        match node {
            Node::Dir(mode) => {
                drop(fs::create_dir_all(&at));
                drop(fs::set_permissions(&at, fs::Permissions::from_mode(*mode)));
            }
            Node::File { mode, text } => {
                if let Some(parent) = at.parent() {
                    drop(fs::create_dir_all(parent));
                }
                drop(fs::remove_file(&at));
                drop(fs::write(&at, text));
                drop(fs::set_permissions(&at, fs::Permissions::from_mode(*mode)));
            }
            Node::Link(_) | Node::Fifo => {}
        }
    }
    Ok(())
}

/// The purge of `package`: the installed version's files deleted, then its directories left
/// empty, deepest first.
pub fn purge(s: &State, package: &str) {
    let (Some(root), Some(deb)) = (
        s.files.as_ref(),
        s.installed
            .get(package)
            .and_then(|inst| deb_of(s, package, &inst.version)),
    ) else {
        return;
    };
    delete(root, deb);
}

/// Deletes `deb`'s files under `root`, then its directories that are left empty.
fn delete(root: &Path, deb: &Deb) {
    for (path, node) in &deb.tree {
        if let Node::File { .. } = node {
            drop(fs::remove_file(root.join(path)));
        }
    }
    for (path, node) in deb.tree.iter().rev() {
        if let Node::Dir(_) = node {
            drop(fs::remove_dir(root.join(path)));
        }
    }
}

/// `dpkg-query --search <path>...`: `<packages>: <path>` for each path a package owns, `dpkg-query:
/// no path found matching pattern <path>` on standard error for each other, exit 1 when there
/// was one.
pub fn search(s: &State, run: &Run) -> ToolOutput {
    let (mut found, mut missing) = (String::new(), String::new());
    for path in run.args.iter().skip(1) {
        match owners_of(s, path).as_slice() {
            [] => missing.push_str(&format!(
                "dpkg-query: no path found matching pattern {path}\n"
            )),
            owners => found.push_str(&format!("{}: {path}\n", owners.join(", "))),
        }
    }
    out(i32::from(!missing.is_empty()), &found, &missing)
}
