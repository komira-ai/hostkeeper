//! `release check-linux-pkg --service <hello|mdm>`: a service's `.deb` (`release deb`), read back
//! against the rules of docs/design/packages-native.md section 10.2 ([`crate::release::native`]):
//!
//! - its control archive holds `control`, and besides it only `md5sums` and `conffiles`: no
//!   `preinst`, `postinst`, `prerm`, `postrm`, `config`, `triggers` or any other member, so dpkg
//!   runs nothing of the package's own;
//! - its `Package` is the service's name;
//! - every entry of its data archive is at a path of the service's (`native::deb_path_problem`):
//!   its units are plain files named for the service in `/usr/lib/systemd/system/`, and nothing
//!   is under `/etc/systemd/`, so no unit is enabled by the install;
//! - every entry is owned by root and group root, has no setuid, setgid or sticky bit, and no
//!   group or other may write it;
//! - its hook is `/usr/lib/<service>/hostkeeper-hook`, a regular file of mode 0755;
//! - the program of each unit's `ExecStart=` is a regular file of the package;
//! - it declares no dependency.
//!
//! Every problem is one line. The static rule of hostkeeper's own packages is not applied: a
//! service's programs are built on the release runner.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Contents, DIRECTORY, Installed, Kind, REGULAR, SYMLINK, TYPE_BITS, bad, read};
use crate::release::Error;
use crate::release::contract::Service;
use crate::release::native::{EntryKind, HOOK, PROGRAM_MODE, SYSTEMD_UNITS, deb_path_problem};

/// Why `--service` and `--staged` are not given together.
pub(crate) const SERVICE_AND_STAGED: &str =
    "--service checks a service's .deb and --staged hostkeeper's packages: give one of them";

/// Why hostkeeper's packages are not checked with `--service`.
const HOSTKEEPER: &str =
    "--service checks the .deb of hello or mdm; hostkeeper's packages are checked with --staged";

/// The members a control archive may hold.
const CONTROL_MEMBERS: [&str; 3] = ["control", "md5sums", "conffiles"];

/// The kind of an entry, for the path rule.
fn kind(file: &Installed) -> EntryKind {
    match file.mode & TYPE_BITS {
        REGULAR => EntryKind::File,
        SYMLINK => EntryKind::Link,
        DIRECTORY => EntryKind::Dir,
        _ => EntryKind::Other,
    }
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

/// The problems of the files `found` of a `.deb` of `service`, one line each. A directory's path
/// is taken without its closing `/`.
fn file_problems(service: &str, found: &BTreeMap<String, Installed>) -> Vec<String> {
    let files: BTreeMap<&str, &Installed> = found
        .iter()
        .map(|(path, file)| match path.trim_end_matches('/') {
            "" => ("/", file),
            trimmed => (trimmed, file),
        })
        .collect();
    let mut out = Vec::new();
    for (&path, &file) in &files {
        if let Some(why) = deb_path_problem(service, path, kind(file)) {
            out.push(format!("{path}: {why}"));
        }
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
        if kind(file) != EntryKind::Link && file.mode & 0o022 != 0 {
            out.push(format!(
                "{path}: mode {:o} lets a group or others write it",
                file.mode & 0o7777
            ));
        }
    }
    let hook = format!("/usr/lib/{service}/{HOOK}");
    match files.get(hook.as_str()) {
        None => out.push(format!(
            "{hook}: not in the package: the hook is at that path"
        )),
        Some(file) if kind(file) != EntryKind::File => {
            out.push(format!("{hook}: not a regular file"));
        }
        Some(file) if file.mode & 0o7777 != PROGRAM_MODE => out.push(format!(
            "{hook}: mode {:o}, not {PROGRAM_MODE:o}",
            file.mode & 0o7777
        )),
        Some(_) => {}
    }
    let unit_dir = format!("{SYSTEMD_UNITS}/");
    for (path, file) in files
        .iter()
        .filter(|(p, f)| p.starts_with(&unit_dir) && kind(f) == EntryKind::File)
    {
        for program in exec_starts(&String::from_utf8_lossy(&file.data)) {
            let packaged = files
                .get(program.as_str())
                .is_some_and(|f| kind(f) == EntryKind::File);
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
    for member in &control.members {
        if !CONTROL_MEMBERS.contains(&member.as_str()) {
            out.push(format!(
                "control.tar.gz holds {member}: a service's .deb holds only control, md5sums \
                 and conffiles there, and no maintainer script"
            ));
        }
    }
    if control.package.as_deref() != Some(service) {
        out.push(format!(
            "control: Package is {:?}, not {service:?}",
            control.package.as_deref().unwrap_or_default()
        ));
    }
    out.extend(file_problems(service, &contents.files));
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
        .filter(|(p, f)| p.starts_with(&format!("{SYSTEMD_UNITS}/")) && kind(f) == EntryKind::File)
        .count();
    Ok(vec![
        format!("{name}: control.tar.gz holds no maintainer script"),
        format!("{name}: every file is at a path of {service}'s, root's, writable by root alone"),
        format!("{name}: the hook /usr/lib/{service}/{HOOK} is a regular file, 755"),
        format!(
            "{name}: {units} units in {SYSTEMD_UNITS}/, none enabled, each starting a file of the package"
        ),
        format!("{name}: declares no dependency"),
    ])
}

#[cfg(test)]
mod tests;
