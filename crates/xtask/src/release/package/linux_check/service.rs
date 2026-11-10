//! `release check-linux-pkg --service <hello|mdm>`: a service's `.deb` (`release deb`), read back
//! against what this repository's own builder writes ([`crate::release::native`]). These are
//! checks of xtask's build, not rules hostkeeper holds a package to: hostkeeper installs any
//! package its order pins, and the manifest names the package's hook. The check holds:
//!
//! - its `Package` is the service's name;
//! - every entry is owned by root and group root, has no setuid, setgid or sticky bit, and no
//!   group or other may write it;
//! - the program of each unit's `ExecStart=` in `/usr/lib/systemd/system/` is a regular file of
//!   the package;
//! - it declares no dependency.
//!
//! What the package holds besides, maintainer scripts, paths, unit names and enablement, is not
//! checked. Every problem is one line. The static rule of hostkeeper's own packages is not
//! applied: a service's programs are built as its archive package's are.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Contents, Installed, Kind, REGULAR, SYMLINK, TYPE_BITS, bad, read};
use crate::release::Error;
use crate::release::contract::Service;
use crate::release::native::SYSTEMD_UNITS;

/// Why `--service` and `--staged` are not given together.
pub(crate) const SERVICE_AND_STAGED: &str =
    "--service checks a service's .deb and --staged hostkeeper's packages: give one of them";

/// Why hostkeeper's packages are not checked with `--service`.
const HOSTKEEPER: &str =
    "--service checks the .deb of hello or mdm; hostkeeper's packages are checked with --staged";

/// Whether an entry is a regular file.
fn is_file(file: &Installed) -> bool {
    file.mode & TYPE_BITS == REGULAR
}

/// The programs the `ExecStart=` lines of the unit text `text` start: the first word of each,
/// without systemd's prefixes (`-`, `+`, `!`, `@`, `:`).
fn exec_starts(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("ExecStart="))
        .filter_map(|command| command.split_whitespace().next())
        .map(|program| {
            program
                .trim_start_matches(['-', '+', '!', '@', ':'])
                .to_owned()
        })
        .collect()
}

/// The problems of the files `found` of a `.deb`, one line each. A directory's path
/// is taken without its closing `/`.
fn file_problems(found: &BTreeMap<String, Installed>) -> Vec<String> {
    let files: BTreeMap<&str, &Installed> = found
        .iter()
        .map(|(path, file)| match path.trim_end_matches('/') {
            "" => ("/", file),
            trimmed => (trimmed, file),
        })
        .collect();
    let mut out = Vec::new();
    for (&path, &file) in &files {
        if (file.owner.as_str(), file.group.as_str()) != ("root", "root") {
            out.push(format!(
                "{path}: owned by {}:{}, not root:root",
                file.owner, file.group
            ));
        }
        if file.mode & 0o7000 != 0 {
            out.push(format!(
                "{path}: mode {:o} has a setuid, setgid or sticky bit",
                file.mode & 0o7777
            ));
        }
        if file.mode & TYPE_BITS != SYMLINK && file.mode & 0o022 != 0 {
            out.push(format!(
                "{path}: mode {:o} lets a group or others write it",
                file.mode & 0o7777
            ));
        }
    }
    let unit_dir = format!("{SYSTEMD_UNITS}/");
    for (path, file) in files
        .iter()
        .filter(|(p, f)| p.starts_with(&unit_dir) && is_file(f))
    {
        for program in exec_starts(&String::from_utf8_lossy(&file.data)) {
            let packaged = files.get(program.as_str()).is_some_and(|f| is_file(f));
            if !packaged {
                out.push(format!(
                    "{path}: ExecStart= runs {program}, which is not a file of the package"
                ));
            }
        }
    }
    out
}

/// The problems of the deb `contents` of `service`, one line each.
fn problems(service: &str, contents: &Contents) -> Result<Vec<String>, Error> {
    let control = match (contents.kind, &contents.control) {
        (Kind::Deb, Some(control)) => control,
        _ => return Err(bad("--service reads a .deb, and this is not one")),
    };
    let mut out = Vec::new();
    if control.package.as_deref() != Some(service) {
        out.push(format!(
            "control: Package is {:?}, not {service:?}",
            control.package.as_deref().unwrap_or_default()
        ));
    }
    out.extend(file_problems(&contents.files));
    out.extend(
        contents
            .depends
            .iter()
            .map(|dependency| format!("declares the dependency {dependency}")),
    );
    Ok(out)
}

/// Checks the `.deb` at `package` as one of `service`'s: the lines to print.
pub(crate) fn check_service(package: &Path, service: Service) -> Result<Vec<String>, Error> {
    if service == Service::Hostkeeper {
        return Err(Error::Failed(HOSTKEEPER.to_owned()));
    }
    let service = service.name();
    let bytes = std::fs::read(package).map_err(Error::io(format!("read {}", package.display())))?;
    let contents = read(package, &bytes)?;
    let name = package.display();
    let problems = problems(service, &contents)?;
    if !problems.is_empty() {
        return Err(Error::Refused(
            problems
                .into_iter()
                .map(|p| format!("{name}: {p}"))
                .collect(),
        ));
    }
    let units = contents
        .files
        .iter()
        .filter(|(p, f)| p.starts_with(&format!("{SYSTEMD_UNITS}/")) && is_file(f))
        .count();
    Ok(vec![
        format!("{name}: Package is {service}"),
        format!("{name}: every entry is root's, writable by root alone"),
        format!("{name}: {units} units in {SYSTEMD_UNITS}/, each starting a file of the package"),
        format!("{name}: declares no dependency"),
    ])
}

#[cfg(test)]
mod tests;
