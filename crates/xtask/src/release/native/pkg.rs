//! The `.pkg` of hello and of its failing build: `release macos-pkg --service hello` builds it and
//! `release check-macos-pkg --service hello` reads it back.
//!
//! The file is `hello-<version>-macos-arm64.pkg` (`<version>-fail-ready` for the failing build),
//! a distribution pkg: `pkgbuild` builds the component `hello.pkg` from the payload with no
//! scripts directory (hostkeeper does not require that of a package; it is how this build is
//! made, and the check holds the build to it), so the component has no script, and `productbuild` wraps it as the one
//! package of the product, unsigned (packages-repositories.md section 4.7: the release machinery
//! signs a service's `.pkg`). The component's identifier is [`IDENTIFIER`], its version the
//! version text, its install location `/`, and every entry is owned by root (`--ownership
//! recommended`).
//!
//! The payload is exactly [`super::hello::pkg_files`] and the directories above them, each 0755:
//! the program in `/Library/Application Support/hello/`, and the two jobs' plists in
//! `/Library/LaunchDaemons/`, each with `Disabled` true.
//!
//! The check takes the directory `pkgutil --expand` made of the product: it holds exactly the
//! file `Distribution`, naming the one component under the identifier and the version with no
//! installer script or check, and the directory `hello.pkg`, which holds exactly `Bom`,
//! `PackageInfo` and `Payload` (no `Scripts`). `PackageInfo` names the identifier, the version
//! and the install location `/` and no script. `Payload` holds exactly the entries above with
//! their modes and bytes, each owned by root and of group wheel or admin; an entry outside the
//! build's files is named as such; each plist parses with its job's
//! label, `Disabled` true, and a program that is a file of the payload.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::DIR_MODE;
use super::hello::{self, File, LAUNCH_DAEMONS, NAME, PKG_DIR};
use crate::release::Error;
use crate::release::contract::{Platform, Version};
use crate::release::file_sha256;
use crate::release::macos_pkg::distribution::product_problems;
use crate::release::macos_pkg::{
    CpioEntry, layer, payload_path, pkg_info_attributes, read_payload,
};
use crate::release::ports::BuildPkg;

/// The component's package identifier: the name `pkgutil` knows the receipt by.
pub(crate) const IDENTIFIER: &str = "dev.hostkeeper.hello";

/// The component pkg's file name, in the stage and in the expanded product.
pub(crate) const COMPONENT: &str = "hello.pkg";

/// The file of the product beside the component.
const DISTRIBUTION: &str = "Distribution";

/// The files of an expanded component: no `Scripts`.
const COMPONENT_FILES: [&str; 3] = ["Bom", "PackageInfo", "Payload"];

/// The groups an entry may have: wheel (0) and admin (80).
const GROUPS: [u32; 2] = [0, 80];

/// The file-type bits of a cpio mode, and the two types a payload holds.
const TYPE_BITS: u32 = 0o170_000;
const TYPE_DIR: u32 = 0o040_000;
const TYPE_FILE: u32 = 0o100_000;

/// The file name of the `.pkg` at the version text `version`.
pub(crate) fn pkg_name(version: &str) -> String {
    format!("{NAME}-{version}-{}.pkg", Platform::MacosArm64.name())
}

/// The version text of the build: the release's, or `<version>-fail-ready`.
pub(crate) fn version_text(version: &Version, fail_ready: bool) -> String {
    let package = match fail_ready {
        true => crate::release::contract::Package::HelloFailReady,
        false => crate::release::contract::Package::Hello,
    };
    package.version_text(version)
}

/// What one entry of the payload must be.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Want {
    Dir,
    File { mode: u32, bytes: Vec<u8> },
}

/// Every entry of the payload by its path from `/` without the leading `/` (`""` is `/`).
fn payload(files: Vec<File>) -> BTreeMap<String, Want> {
    let mut want = BTreeMap::new();
    want.insert(String::new(), Want::Dir);
    for file in files {
        let path = file.path.trim_start_matches('/').to_owned();
        let mut parent = path.as_str();
        while let Some((dir, _)) = parent.rsplit_once('/') {
            want.insert(dir.to_owned(), Want::Dir);
            parent = dir;
        }
        want.insert(
            path,
            Want::File {
                mode: file.mode,
                bytes: file.data,
            },
        );
    }
    want
}

/// What `release macos-pkg --service hello` builds the pkg from.
pub(crate) struct Request {
    pub(crate) version: Version,
    pub(crate) fail_ready: bool,
    /// The macos-arm64 hello program.
    pub(crate) binary: PathBuf,
    /// A directory to lay the payload out in (`root`) and to write the component to; it must not
    /// exist.
    pub(crate) stage: PathBuf,
    /// The directory the product is written to.
    pub(crate) out: PathBuf,
}

/// Lays the payload out under `request.stage`'s `root`, builds the component from it with no
/// scripts, and wraps it as the product in `request.out`; the lines to print.
pub(crate) fn build(pkgbuild: &dyn BuildPkg, request: &Request) -> Result<Vec<String>, Error> {
    let binary = std::fs::read(&request.binary)
        .map_err(Error::io(format!("read {}", request.binary.display())))?;
    let files = hello::pkg_files(binary, request.fail_ready)?;
    let stage = &request.stage;
    if std::fs::symlink_metadata(stage).is_ok() {
        return Err(Error::Failed(format!(
            "{} exists: the pkg's tree is laid out in a new directory, so it holds nothing else",
            stage.display()
        )));
    }
    let make_dir = crate::release::macos_pkg::make_dir;
    make_dir(stage)?;
    let root = stage.join("root");
    for (path, want) in payload(files) {
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
    let version = version_text(&request.version, request.fail_ready);
    let component = stage.join(COMPONENT);
    pkgbuild.build_pkg(&root, None, IDENTIFIER, &version, &component)?;
    let name = pkg_name(&version);
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
            Some(value) => {
                problems.push(format!("PackageInfo: {name} is {value:?}, not {want:?}"));
            }
            None => problems.push(format!("PackageInfo has no {name}")),
        }
    }
    for script in ["preinstall", "postinstall"] {
        if text.contains(&format!("<{script}")) {
            problems.push(format!(
                "PackageInfo names a {script} script: a service's pkg has no script"
            ));
        }
    }
}

/// What is wrong with the plist of the job `label`, one line each: it parses as a dictionary
/// with that label, `Disabled` true, and a program that is a file of the payload (`files`, the
/// payload's file paths from `/`).
fn plist_problems(
    path: &str,
    label: &str,
    bytes: &[u8],
    files: &[String],
    problems: &mut Vec<String>,
) {
    let what = format!("/{path}");
    let value = match plist::Value::from_reader_xml(bytes) {
        Ok(value) => value,
        Err(e) => return problems.push(format!("{what} does not parse as a plist: {e}")),
    };
    let Some(dict) = value.as_dictionary() else {
        return problems.push(format!("{what} is not a plist dictionary"));
    };
    let got = dict.get("Label").and_then(plist::Value::as_string);
    if got != Some(label) {
        problems.push(format!("{what}: the label is {got:?}, not {label:?}"));
    }
    if dict.get("Disabled").and_then(plist::Value::as_boolean) != Some(true) {
        problems.push(format!(
            "{what}: Disabled is not true, so launchd would load the job at boot before \
             hostkeeper enables it"
        ));
    }
    let program = dict
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .and_then(|a| a.first())
        .and_then(plist::Value::as_string);
    if !program.is_some_and(|p| files.iter().any(|f| *f == p.trim_start_matches('/'))) {
        problems.push(format!(
            "{what}: the job runs {program:?}, which is not a file of the payload"
        ));
    }
}

/// What is wrong with the payload `entries` against `want`, one line each.
fn payload_problems(
    entries: &[CpioEntry],
    mut want: BTreeMap<String, Want>,
    problems: &mut Vec<String>,
) {
    let files: Vec<String> = want
        .iter()
        .filter(|(_, w)| matches!(w, Want::File { .. }))
        .map(|(p, _)| p.clone())
        .collect();
    for entry in entries {
        let path = payload_path(&entry.name);
        let what = format!("/{path}");
        let (kind, mode) = (entry.mode & TYPE_BITS, entry.mode & !TYPE_BITS);
        let Some(wanted) = want.remove(path) else {
            problems.push(format!(
                "{what} ({:?}) is in the payload: not a file of hello's pkg",
                entry.name
            ));
            continue;
        };
        if entry.uid != 0 || !GROUPS.contains(&entry.gid) {
            problems.push(format!(
                "{what}: owned by uid {} and gid {}, not root and wheel or admin",
                entry.uid, entry.gid
            ));
        }
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
        if let Want::File { bytes, .. } = &wanted {
            if entry.data != *bytes {
                problems.push(format!("{what}: not the bytes release macos-pkg writes"));
            }
            if let Some(label) = path
                .strip_prefix(&format!("{}/", LAUNCH_DAEMONS.trim_start_matches('/')))
                .and_then(|n| n.strip_suffix(".plist"))
            {
                plist_problems(path, label, &entry.data, &files, problems);
            }
        }
    }
    problems.extend(
        want.keys()
            .map(|path| format!("missing /{path} in the payload")),
    );
}

/// The `release check-macos-pkg --service hello` verb: checks the product `pkgutil --expand`
/// expanded into `expanded` against the build `version` (with `fail_ready`) and the program
/// `binary`, listing every problem.
pub(crate) fn check(
    expanded: &Path,
    version: &Version,
    fail_ready: bool,
    binary: &Path,
) -> Result<Vec<String>, Error> {
    let version = version_text(version, fail_ready);
    let bytes = std::fs::read(binary).map_err(Error::io(format!("read {}", binary.display())))?;
    let want = payload(hello::pkg_files(bytes, fail_ready)?);
    let mut problems = Vec::new();
    let only = format!(
        "an expanded product of hello's pkg holds only the file {DISTRIBUTION} and the directory \
         {COMPONENT}"
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
        let only = "an expanded component of a service's pkg holds only the files Bom, \
                    PackageInfo and Payload: no Scripts, since it has no script";
        let parts = layer(&component, &COMPONENT_FILES, &[], only, &mut problems)?;
        if parts.iter().any(|p| p == "PackageInfo") {
            let text = read(component.join("PackageInfo"))?;
            package_info_problems(&String::from_utf8_lossy(&text), &version, &mut problems);
        }
        if parts.iter().any(|p| p == "Payload") {
            match read_payload(&component)? {
                Err(why) => problems.push(why),
                Ok(entries) => payload_problems(&entries, want, &mut problems),
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "check-macos-pkg: {} is a product of the one component {COMPONENT} ({IDENTIFIER} \
         {version}), with no script, which installs exactly {PKG_DIR}/bin/{NAME} and the \
         disabled jobs {}, owned by root",
        expanded.display(),
        hello::SLOTS
            .map(|slot| format!("{LAUNCH_DAEMONS}/{NAME}.{slot}.plist"))
            .join(" and ")
    )])
}

#[cfg(test)]
mod tests;
