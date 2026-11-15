//! `release macos-bootstrap-pkg` and `release check-macos-bootstrap-pkg`: the bootstrap pkg that
//! Apple Business installs on a new Mac (docs/design/mac-bootstrap.md, section 8),
//! `hostkeeper-bootstrap-<v>-macos-arm64.pkg` ([`pkg_name`]).
//!
//! The pkg installs exactly two files and the directories above them, every entry owned by root
//! and wheel, every directory 0755:
//!
//! - the bootstrap program at [`BINARY_PATH`], mode 0755;
//! - its launchd plist at [`PLIST_PATH`], mode 0644, the file
//!   `packaging/macos-bootstrap/dev.hostkeeper.bootstrap.plist` byte for byte, which runs the
//!   program at load and again only after a run that failed.
//!
//! It carries no config and no secret: the farm's two values reach the Mac by a Blueprint's
//! managed preferences or a settings file, so the one pkg, with its one digest and attestation, is
//! the same for every farm. Its one script is `postinstall`, the file
//! `packaging/macos-bootstrap/postinstall` byte for byte (mode 0755), which loads the job when
//! launchd has not loaded it and never boots a loaded one out. The component's identifier is
//! [`IDENTIFIER`] and its version the release's.
//!
//! The file is a distribution pkg: `pkgbuild` builds the component pkg (the payload and the
//! script) and `productbuild` wraps it as the one package of the product, unsigned; the release
//! signs it with the Developer ID Installer identity, as it signs hostkeeper's pkg.
//!
//! [`check`] reads a product that `pkgutil --expand` expanded: the directory holds exactly the
//! file `Distribution` and the directory of the component ([`COMPONENT`]), which holds exactly
//! `Bom`, `PackageInfo`, `Payload` and the directory `Scripts`; `PackageInfo` names the identifier,
//! the version, the install location `/` and the `postinstall` alone; `Scripts` holds the
//! postinstall's bytes; and `Payload` holds exactly the entries above with their modes, owners
//! and bytes, the plist parsing with the label, the program and the restart rule that this module
//! names.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::contract::{Version, bootstrap_installer_name};
use super::macos_pkg::distribution::product_problems;
use super::macos_pkg::{
    CpioEntry, layer, make_dir, payload_path, pkg_info_attributes, read_payload,
};
use super::ports::BuildPkg;
use super::{Error, file_sha256};

/// The component's package identifier, the launchd label of the job it installs.
pub(crate) const IDENTIFIER: &str = "dev.hostkeeper.bootstrap";
/// The component pkg's file name, in the stage and in the expanded product.
pub(crate) const COMPONENT: &str = "hostkeeper-bootstrap.pkg";
/// The program's path from `/`, without the leading `/`.
const BINARY_PATH: &str = "usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap";
/// The plist's path from `/`, without the leading `/`.
const PLIST_PATH: &str = "Library/LaunchDaemons/dev.hostkeeper.bootstrap.plist";
/// The log file the job writes, which the plist names.
const LOG_PATH: &str = "/var/log/hostkeeper-bootstrap.log";
/// The plist, committed with the pkg's layout.
const PLIST: &str =
    include_str!("../../../../packaging/macos-bootstrap/dev.hostkeeper.bootstrap.plist");
/// The one script, committed with the pkg's layout.
const POSTINSTALL: &str = include_str!("../../../../packaging/macos-bootstrap/postinstall");
/// The script's name in the scripts directory and in the expanded component.
const SCRIPT: &str = "postinstall";
/// The mode of the script, the program and directories.
const SCRIPT_MODE: u32 = 0o755;
const BINARY_MODE: u32 = 0o755;
const PLIST_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;
/// The files of an expanded component, and its scripts directory.
const COMPONENT_FILES: [&str; 3] = ["Bom", "PackageInfo", "Payload"];
const SCRIPTS_DIR: &str = "Scripts";
const DISTRIBUTION: &str = "Distribution";
/// The file-type bits of a cpio mode, and the two types a payload holds.
const TYPE_BITS: u32 = 0o170_000;
const TYPE_DIR: u32 = 0o040_000;
const TYPE_FILE: u32 = 0o100_000;
/// The element `pkgbuild` writes in `PackageInfo` for a `postinstall` in the scripts directory.
const SCRIPT_ELEMENT: &str = "<postinstall file=\"./postinstall\"";

/// The file name of the pkg at `version`: `hostkeeper-bootstrap-<v>-macos-arm64.pkg`.
pub(crate) fn pkg_name(version: &Version) -> String {
    bootstrap_installer_name(version)
}

/// What one entry of the payload must be.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Want {
    Dir,
    File { mode: u32, bytes: Vec<u8> },
}

/// Every entry of the payload by its path from `/` (`""` is `/`).
fn payload(binary: Vec<u8>) -> BTreeMap<String, Want> {
    let mut want = BTreeMap::new();
    want.insert(String::new(), Want::Dir);
    let files = [
        (BINARY_PATH, BINARY_MODE, binary),
        (PLIST_PATH, PLIST_MODE, PLIST.as_bytes().to_vec()),
    ];
    for (path, mode, bytes) in files {
        let mut parent = path;
        while let Some((dir, _)) = parent.rsplit_once('/') {
            want.insert(dir.to_owned(), Want::Dir);
            parent = dir;
        }
        want.insert(path.to_owned(), Want::File { mode, bytes });
    }
    want
}

/// What `release macos-bootstrap-pkg` builds the pkg from.
pub(crate) struct Request {
    pub(crate) version: Version,
    /// The macos-arm64 bootstrap program.
    pub(crate) binary: PathBuf,
    /// A directory to lay the payload and the script out in and to write the component to; it
    /// must not exist.
    pub(crate) stage: PathBuf,
    /// The directory the product is written to.
    pub(crate) out: PathBuf,
}

/// Lays the payload out under `request.stage`'s `root` and the script under its `scripts`, builds
/// the component and wraps it as the product in `request.out`; the lines to print.
pub(crate) fn build(pkgbuild: &dyn BuildPkg, request: &Request) -> Result<Vec<String>, Error> {
    let binary = std::fs::read(&request.binary)
        .map_err(Error::io(format!("read {}", request.binary.display())))?;
    let stage = &request.stage;
    if std::fs::symlink_metadata(stage).is_ok() {
        return Err(Error::Failed(format!(
            "{} exists: the pkg's tree is laid out in a new directory, so it holds nothing else",
            stage.display()
        )));
    }
    make_dir(stage)?;
    let root = stage.join("root");
    let scripts = stage.join("scripts");
    make_dir(&scripts)?;
    let script = scripts.join(SCRIPT);
    std::fs::write(&script, POSTINSTALL)
        .map_err(Error::io(format!("write {}", script.display())))?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(SCRIPT_MODE))
        .map_err(Error::io(format!("set the mode of {}", script.display())))?;
    for (path, want) in payload(binary) {
        let at: PathBuf = root.join(&path).components().collect();
        let mode = match &want {
            Want::Dir => {
                std::fs::create_dir(&at).map_err(Error::io(format!("create {}", at.display())))?;
                DIR_MODE
            }
            Want::File { mode, bytes } => {
                std::fs::write(&at, bytes).map_err(Error::io(format!("write {}", at.display())))?;
                *mode
            }
        };
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode))
            .map_err(Error::io(format!("set the mode of {}", at.display())))?;
    }
    std::fs::create_dir_all(&request.out)
        .map_err(Error::io(format!("create {}", request.out.display())))?;
    let version = request.version.to_string();
    let component = stage.join(COMPONENT);
    pkgbuild.build_pkg(&root, Some(&scripts), IDENTIFIER, &version, &component)?;
    let name = pkg_name(&request.version);
    let product = request.out.join(&name);
    pkgbuild.build_product(&component, IDENTIFIER, &version, &product)?;
    let hex = file_sha256(&product)?;
    Ok(vec![format!("{name}: sha256 {hex}")])
}

/// What is wrong with the `PackageInfo` text `text` of the build `version`, one line each.
fn package_info_problems(text: &str, version: &str, problems: &mut Vec<String>) {
    let Some(attributes) = pkg_info_attributes(text) else {
        return problems.push("PackageInfo has no readable <pkg-info> element".to_owned());
    };
    for (name, want) in [
        ("identifier", IDENTIFIER),
        ("version", version),
        ("install-location", "/"),
    ] {
        match attributes.get(name) {
            Some(value) if value == want => {}
            Some(value) => problems.push(format!("PackageInfo: {name} is {value:?}, not {want:?}")),
            None => problems.push(format!("PackageInfo has no {name}")),
        }
    }
    if text.contains("<preinstall") {
        problems.push(
            "PackageInfo names a preinstall script: the pkg's one script is its postinstall"
                .to_owned(),
        );
    }
    if !text.contains(SCRIPT_ELEMENT) {
        problems.push(format!(
            "PackageInfo names no postinstall script ({SCRIPT_ELEMENT}/>): nothing would load the job"
        ));
    }
}

/// What is wrong with the `Scripts` directory `dir`, one line each: it holds exactly the
/// postinstall, with the committed bytes and mode 0755.
fn scripts_problems(dir: &Path, problems: &mut Vec<String>) -> Result<(), Error> {
    let what = format!("read {}", dir.display());
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .and_then(|read| read.collect::<Result<Vec<_>, _>>())
        .map_err(Error::io(&what))?
        .iter()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names.iter().filter(|name| *name != SCRIPT) {
        problems.push(format!(
            "Scripts holds {name:?}: the pkg's one script is {SCRIPT}"
        ));
    }
    if !names.iter().any(|name| name == SCRIPT) {
        problems.push(format!("Scripts holds no {SCRIPT}"));
        return Ok(());
    }
    let path = dir.join(SCRIPT);
    let meta =
        std::fs::symlink_metadata(&path).map_err(Error::io(format!("read {}", path.display())))?;
    let mode = meta.permissions().mode() & !TYPE_BITS;
    if mode != SCRIPT_MODE {
        problems.push(format!(
            "Scripts: {SCRIPT} has mode {mode:o}, not {SCRIPT_MODE:o}"
        ));
    }
    let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
    if bytes != POSTINSTALL.as_bytes() {
        problems.push(format!(
            "Scripts: {SCRIPT} is not packaging/macos-bootstrap/{SCRIPT}"
        ));
    }
    Ok(())
}

/// What is wrong with the plist `bytes` of the payload, one line each: it parses as a dictionary
/// with the label, the installed program as its program, a run at load, a restart only after a
/// failed run, and the log file as its output.
fn plist_problems(bytes: &[u8], problems: &mut Vec<String>) {
    let what = format!("/{PLIST_PATH}");
    let value = match plist::Value::from_reader_xml(bytes) {
        Ok(value) => value,
        Err(e) => return problems.push(format!("{what} does not parse as a plist: {e}")),
    };
    let Some(dict) = value.as_dictionary() else {
        return problems.push(format!("{what} is not a plist dictionary"));
    };
    let text = |key: &str| dict.get(key).and_then(plist::Value::as_string);
    if text("Label") != Some(IDENTIFIER) {
        problems.push(format!(
            "{what}: the label is {:?}, not {IDENTIFIER:?}",
            text("Label")
        ));
    }
    let program = dict
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .and_then(|a| a.first())
        .and_then(plist::Value::as_string);
    let installed = format!("/{BINARY_PATH}");
    if program != Some(installed.as_str()) {
        problems.push(format!(
            "{what}: the job runs {program:?}, not {installed:?}"
        ));
    }
    if dict.get("RunAtLoad").and_then(plist::Value::as_boolean) != Some(true) {
        problems.push(format!(
            "{what}: RunAtLoad is not true, so the bootstrap would not start at boot"
        ));
    }
    let restart = dict
        .get("KeepAlive")
        .and_then(plist::Value::as_dictionary)
        .and_then(|keep| keep.get("SuccessfulExit"))
        .and_then(plist::Value::as_boolean);
    if restart != Some(false) {
        problems.push(format!(
            "{what}: KeepAlive is not {{SuccessfulExit: false}}, so launchd would not start the bootstrap again only after a failed run"
        ));
    }
    for key in ["StandardOutPath", "StandardErrorPath"] {
        if text(key) != Some(LOG_PATH) {
            problems.push(format!(
                "{what}: {key} is {:?}, not {LOG_PATH:?}",
                text(key)
            ));
        }
    }
}

/// What is wrong with the payload `entries` against `want`, one line each.
fn payload_problems(
    entries: &[CpioEntry],
    mut want: BTreeMap<String, Want>,
    problems: &mut Vec<String>,
) {
    for entry in entries {
        let path = payload_path(&entry.name);
        let what = format!("/{path}");
        let Some(wanted) = want.remove(path) else {
            problems.push(format!(
                "{what} ({:?}) is in the payload, which holds only the program, the plist and their directories",
                entry.name
            ));
            continue;
        };
        if (entry.uid, entry.gid) != (0, 0) {
            problems.push(format!(
                "{what}: owned by uid {} and gid {}, not root and wheel (0 and 0)",
                entry.uid, entry.gid
            ));
        }
        let (kind, mode) = (entry.mode & TYPE_BITS, entry.mode & !TYPE_BITS);
        let (want_kind, want_mode) = match &wanted {
            Want::Dir => (TYPE_DIR, DIR_MODE),
            Want::File { mode, .. } => (TYPE_FILE, *mode),
        };
        if kind != want_kind {
            problems.push(format!("{what}: of type {kind:o}, not {want_kind:o}"));
            continue;
        }
        if mode != want_mode {
            problems.push(format!("{what}: mode {mode:o}, not {want_mode:o}"));
        }
        if let Want::File { bytes, .. } = &wanted
            && entry.data != *bytes
        {
            problems.push(format!(
                "{what}: not the bytes release macos-bootstrap-pkg writes"
            ));
        }
        if path == PLIST_PATH {
            plist_problems(&entry.data, problems);
        }
    }
    problems.extend(
        want.keys()
            .map(|path| format!("missing /{path} in the payload")),
    );
}

/// The `release check-macos-bootstrap-pkg` verb: checks the product `pkgutil --expand` expanded
/// into `expanded` against the build `version` and the program `binary`, listing every problem.
pub(crate) fn check(
    expanded: &Path,
    version: &Version,
    binary: &Path,
) -> Result<Vec<String>, Error> {
    let version = version.to_string();
    let bytes = std::fs::read(binary).map_err(Error::io(format!("read {}", binary.display())))?;
    let mut problems = Vec::new();
    let only = format!(
        "an expanded product of the bootstrap pkg holds only the file {DISTRIBUTION} and the \
         directory {COMPONENT}"
    );
    let present = layer(
        expanded,
        &[DISTRIBUTION],
        &[COMPONENT],
        &only,
        &mut problems,
    )?;
    let read =
        |path: PathBuf| std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())));
    if present.iter().any(|p| p == DISTRIBUTION) {
        let text = read(expanded.join(DISTRIBUTION))?;
        product_problems(
            &String::from_utf8_lossy(&text),
            IDENTIFIER,
            COMPONENT,
            &version,
            &mut problems,
        );
    }
    if present.iter().any(|p| p == COMPONENT) {
        let component = expanded.join(COMPONENT);
        let only = "an expanded component of the bootstrap pkg holds only the files Bom, \
                    PackageInfo and Payload and the directory Scripts";
        let parts = layer(
            &component,
            &COMPONENT_FILES,
            &[SCRIPTS_DIR],
            only,
            &mut problems,
        )?;
        let has = |name: &str| parts.iter().any(|p| p == name);
        if has("PackageInfo") {
            let text = read(component.join("PackageInfo"))?;
            package_info_problems(&String::from_utf8_lossy(&text), &version, &mut problems);
        }
        if has(SCRIPTS_DIR) {
            scripts_problems(&component.join(SCRIPTS_DIR), &mut problems)?;
        }
        if has("Payload") {
            match read_payload(&component)? {
                Err(why) => problems.push(why),
                Ok(entries) => payload_problems(&entries, payload(bytes), &mut problems),
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "check-macos-bootstrap-pkg: {} is a product of the one component {COMPONENT} ({IDENTIFIER} \
         {version}), whose one script is {SCRIPT}, which installs exactly /{BINARY_PATH} \
         ({BINARY_MODE:o}) and /{PLIST_PATH} ({PLIST_MODE:o}), owned by root",
        expanded.display()
    )])
}

#[cfg(test)]
mod tests;
