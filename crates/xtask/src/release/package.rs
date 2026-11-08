//! `release package`: the package of one service, the archive of design sections 4.7 and 5.5,
//! built the same bytes every time for one commit and version. There are three services
//! ([`Service`]): hostkeeper's own package, from a unit file given with `--unit`; `hello`, the
//! example service of `examples/hello`, whose manifest and units this module's [`hello`] child
//! renders from one description for each platform's kind of unit; and `mdm`, the MDM server
//! stack (docs/design/two-programs.md section 6.2), whose manifest and units are
//! `hostkeeper_mdm_core::package`'s and whose pinned tools [`mdm`] takes out of the archives
//! given with `--archives`, each checked against its pin first. The mdm package is built for
//! linux-x86_64 only. Hello's package has a failing
//! build, `--fail-ready`: the same package at the version `<V>-fail-ready`, differing from hello's
//! only in that version (the manifest's), in the unit's `ExecStart` and in the plists'
//! `ProgramArguments`, which gain `--fail-ready`.
//!
//! The archive is a ustar tar, compressed by [`compress`], holding exactly these entries in this
//! order. Hostkeeper: the directory `bin/`, the file `bin/hostkeeper` (mode 0555), the directory
//! `files/` and the file `files/trusted_root.json` (0444), the file `manifest.toml` (0444), and on
//! Linux the directory `units/` and the file `units/hostkeeper.service` (0444). The trusted root
//! is the Sigstore trusted root given with `--trusted-root`, byte for byte: the release builds
//! it from the root committed at `packaging/sigstore/trusted_root.json`. The daemon checks every
//! package's attestation against it where the package is unpacked
//! (`<packages-dir>/hostkeeper/current/files/trusted_root.json`), so the manifest places no file
//! and each update of hostkeeper brings the root its release was built with. Hello has `bin/hello`, and under `units/` on Linux
//! `hello@.service`, on macOS `hello.a.plist` and `hello.b.plist` (a package holds one kind of
//! unit). Mdm has `bin/mdm` and, beside it, the binaries of its pinned tools (`caddy`,
//! `depserver`, `depsyncer`, `lego`, `nanohub`, `scepserver`, 0555), and its units under `units/`
//! (0444). Directories are 0555. Hostkeeper's macos-arm64 package has no `units/` and its manifest
//! no `[units]`: on a Mac the daemon's launchd job is the plist the installer pkg places
//! (`macos_pkg`), and the manifest's plist labels are `<service>` or `<service>.<more>`, which the
//! daemon's label, `dev.hostkeeper.hostkeeper`, is not. Every header has uid and gid 0, empty user
//! and group names and, as its time, the committer time of the commit built, so the bytes depend
//! on the inputs alone. The archive is written to the name [`Package::package_name`] gives, with
//! beside it a file of the same name plus [`DIGEST_SUFFIX`] holding `<sha256>  <package name>`
//! and a line break.
//!
//! Hostkeeper's package refuses `--fail-ready`, a missing `--trusted-root`, a missing `--unit` on
//! Linux and any `--unit` on macOS; hello's refuses `--unit` and `--trusted-root`, and builds for
//! every platform; mdm's refuses `--unit`, `--trusted-root`, `--fail-ready`, a missing
//! `--archives` and any platform but linux-x86_64, and only mdm's takes `--archives`. Before the
//! archive is built,
//! every systemd unit is checked (design section 4.7: a unit names package files only through the
//! service's own `current`; hello's unit starts its binary through that `current`, mdm's start a
//! binary of `bin/` through it, and
//! hostkeeper's starts [`SELF_UPDATE_CURRENT`], the binary its own update switches) and the entry
//! list is checked against the rules of section 5.5 that apply to a list this module builds
//! ([`check_entries`]).
//!
//! The child [`nfpm_stage`] is `release nfpm-stage`: the pinned `gh`, the Sigstore trusted root
//! and the two static programs of the daemon (`hostkeeper`, `hostkeeper-pxe`), checked and laid
//! out for the deb, the rpm and the Arch package that `packaging/linux/nfpm.yaml` builds;
//! [`linux_check`] is `release check-linux-pkg`, which reads a built deb, rpm, Arch package or
//! tarball back and refuses an ELF program that is not static and any declared dependency, and
//! with a staged directory checks that the package installs the staged files; [`elf`] is the
//! ELF reader they share.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::Error;
use super::contract::{Commit, DIGEST_SUFFIX, Package, Platform, Service, Version};
use super::ports::GitHistory;

mod elf;
mod hello;
pub(super) mod linux_check;
mod mdm;
pub(super) mod nfpm_stage;

/// Why a Linux package of hostkeeper needs `--unit`.
const LINUX_NEEDS_UNIT: &str =
    "a Linux package carries the systemd unit units/hostkeeper.service: pass --unit";

/// Why a macOS package of hostkeeper takes no `--unit`.
const MACOS_TAKES_NO_UNIT: &str = "the macos-arm64 package carries no unit: the daemon's launchd job is the plist of the installer pkg (release macos-pkg)";

/// Why hostkeeper's package and index take no `--fail-ready`: only hello has that flag to give
/// its units.
pub(super) const FAIL_READY_ONLY_HELLO: &str =
    "--fail-ready is an option of the hello package; hostkeeper has no such flag";

/// Why hello's package takes no `--unit`.
const UNIT_REFUSED: &str =
    "the hello package takes no --unit: its units are rendered for each platform";

/// Why hostkeeper's package needs `--trusted-root`.
pub(super) const NEEDS_TRUSTED_ROOT: &str = "the hostkeeper package carries the Sigstore trusted root files/trusted_root.json: pass --trusted-root";

/// Why hello's package takes no `--trusted-root`.
pub(super) const TRUSTED_ROOT_REFUSED: &str =
    "the hello package takes no --trusted-root: only hostkeeper's package carries the trusted root";

/// Why `--archives` is refused for hostkeeper and hello.
const ARCHIVES_ONLY_MDM: &str =
    "--archives is an option of the mdm package: only it carries pinned third-party tools";

/// Why the mdm package needs `--archives`.
const MDM_NEEDS_ARCHIVES: &str = "the mdm package carries the pinned tools of hostkeeper-mdm-core's pins.txt: pass --archives with the directory `release mdm-fetch` filled";

/// Why mdm's package takes no `--unit`, `--trusted-root` or `--fail-ready`.
const MDM_REFUSED: &str = "the mdm package takes no --unit, --trusted-root or --fail-ready: its units are hostkeeper-mdm-core's, and it has no failing build";

/// Why mdm's package is built for linux-x86_64 alone.
const MDM_PLATFORM: &str =
    "the mdm package is built for linux-x86_64 only: the pinned tools are linux-amd64 builds";

/// Where hostkeeper's package holds the Sigstore trusted root: the daemon reads it there, under
/// the version directory `current` names.
const TRUSTED_ROOT_ENTRY: &str = "files/trusted_root.json";

/// What `release package` builds a package from.
pub(crate) struct Request {
    pub(crate) service: Service,
    pub(crate) platform: Platform,
    pub(crate) version: Version,
    pub(crate) commit: Commit,
    /// The service's binary for the platform.
    pub(crate) binary: PathBuf,
    /// The systemd unit of the hostkeeper package, for a Linux platform; none for macos-arm64.
    /// Hello's units are rendered, so it has none.
    pub(crate) unit: Option<PathBuf>,
    /// Hello's units start it with `--fail-ready`, so its `/readyz` answers 503 (a build that
    /// fails verify, for the farm's rollback test).
    pub(crate) fail_ready: bool,
    /// The Sigstore trusted root of the hostkeeper package, as `gh attestation trusted-root`
    /// prints it: `files/trusted_root.json` in the package. Hello's package carries none.
    pub(crate) trusted_root: Option<PathBuf>,
    /// The directory of the pinned archives of the mdm package (`release mdm-fetch`); none for
    /// the others.
    pub(crate) archives: Option<PathBuf>,
    /// The directory the package and its digest file are written to.
    pub(crate) out: PathBuf,
}

/// The `[hook]` tables of the manifest: the one place the hook values are set. The manifest
/// loader requires a hook with bounded steps, so hostkeeper's own package carries one: the
/// package's own binary, `hostkeeper update-hook` (`hostkeeper_daemon::update_hook`), whose every
/// step does nothing, because hostkeeper's self-update is compiled into the daemon (design section
/// 6.6). A daemon runs it for every install and update of hostkeeper. The manifest says so in a
/// comment.
const HOOK: &str = r#"# Every step of this hook does nothing: hostkeeper's self-update is compiled into the
# daemon. The table is here because every manifest has a bounded hook.
[hook]
command = ["bin/hostkeeper", "update-hook"]
verify_deadline_secs = 600

[hook.timeout_secs]
prepare = 300
cutover = 300
verify = 300
rollback = 300
cleanup = 300
"#;

/// The unit file the hostkeeper package holds, under `units/`.
const UNIT_FILE: &str = "hostkeeper.service";

/// Where the host links the package: every unit names package files through `current`.
const PACKAGES: &str = "/var/lib/hostkeeper/packages/";

/// What [`PACKAGES`] must be followed by wherever a unit of `service` names it.
fn current(service: &str) -> String {
    format!("{service}/current/")
}

/// The binary of `service` in its package, through `current`.
fn binary_path(service: &str) -> String {
    format!("{PACKAGES}{}bin/{service}", current(service))
}

/// What hostkeeper's own unit starts: `current` of the binary directory of its own update, under
/// the daemon's default state directory (`hostkeeper_daemon::selfupdate::binary_dir`, which the
/// packaging test holds this to). The self-update stages each new binary there and switches this
/// link, and its fallback switches it back (design section 6.6), so the unit always starts the
/// binary the update chose; the package's own `bin/hostkeeper` is what the update stages.
pub(crate) const SELF_UPDATE_CURRENT: &str = "/var/lib/updater/state/hostkeeper/bin/current";

/// The program the unit of `service` must start: [`SELF_UPDATE_CURRENT`] for hostkeeper, a
/// binary of `bin/` through mdm's `current` for mdm (its units start the pinned tools), the
/// service's binary through its `current` ([`binary_path`]) for any other.
enum Started {
    /// Exactly this program.
    Exactly(String),
    /// A file directly in this directory (the text ends with `/`).
    Under(String),
}

impl Started {
    fn of(service: &str) -> Started {
        if service == Service::Hostkeeper.name() {
            Started::Exactly(SELF_UPDATE_CURRENT.to_owned())
        } else if service == Service::Mdm.name() {
            Started::Under(format!("{PACKAGES}{}bin/", current(service)))
        } else {
            Started::Exactly(binary_path(service))
        }
    }

    /// Whether `program` is one this allows.
    fn allows(&self, program: &str) -> bool {
        match self {
            Started::Exactly(want) => program == want,
            Started::Under(dir) => program
                .strip_prefix(dir.as_str())
                .is_some_and(|name| !name.is_empty() & !name.contains('/')),
        }
    }

    /// What a refusal says the unit lacks.
    fn wanted(&self) -> String {
        match self {
            Started::Exactly(want) => format!("naming {want}"),
            Started::Under(dir) => format!("naming a program in {dir}"),
        }
    }
}

/// The top-level names a package may hold (design section 5.5).
const TOP_LEVEL: [&str; 4] = ["manifest.toml", "bin", "units", "files"];

/// The modes an entry may have: executable or not, never writable.
const MODES: [u32; 2] = [0o555, 0o444];

/// One entry of the archive.
struct Entry {
    /// The path; a directory's ends with `/`.
    name: String,
    directory: bool,
    mode: u32,
    data: Vec<u8>,
}

impl Entry {
    fn file(name: &str, mode: u32, data: Vec<u8>) -> Entry {
        Entry {
            name: name.to_owned(),
            directory: false,
            mode,
            data,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            directory: true,
            mode: 0o555,
            data: Vec::new(),
        }
    }
}

/// A unit file of the package, with where its bytes came from.
struct Unit {
    /// The file name under `units/`.
    file: String,
    /// What an error names the unit by: the path given with `--unit`, or the entry's path.
    origin: String,
    data: Vec<u8>,
}

/// What a service's package holds besides its binary: no units for hostkeeper on macOS, the
/// trusted root for hostkeeper only, and more binaries for mdm only.
struct Contents {
    manifest: String,
    units: Vec<Unit>,
    /// The bytes of `files/trusted_root.json`.
    trusted_root: Option<Vec<u8>>,
    /// The other files of `bin/`, by name: mdm's pinned tools.
    binaries: Vec<(String, Vec<u8>)>,
}

/// The oldest daemon that takes hostkeeper's own package: [`hostkeeper_steps::OLDEST_READER`],
/// the oldest release that reads every file this release writes, the one constant the daemon
/// judges a release of hostkeeper by too (docs/design/two-programs.md, section 4.2). A daemon is
/// always older than the release of hostkeeper it is ordered, so the package cannot ask for its
/// own version: every daemon would answer the order `daemon_too_old` and no host would ever
/// update hostkeeper.
pub(crate) const OLDEST_READER: Version = Version {
    major: hostkeeper_steps::OLDEST_READER.major,
    minor: hostkeeper_steps::OLDEST_READER.minor,
    patch: hostkeeper_steps::OLDEST_READER.patch,
};

/// The manifest of hostkeeper's package for `version` built for `platform`: it needs a daemon of
/// [`OLDEST_READER`] or later, or of its own version when that is older (a build of the
/// workspace's unreleased version), names the platform the daemon checks against its own, and
/// with `units` runs [`UNIT_FILE`].
fn manifest(version: &Version, platform: Platform, units: bool) -> String {
    let floor = (*version).min(OLDEST_READER);
    let platform = platform.name();
    let units = if units {
        format!("\n[units]\nrun = [\"{UNIT_FILE}\"]\n")
    } else {
        String::new()
    };
    format!(
        "schema = 1\nname = \"hostkeeper\"\nversion = \"{version}\"\nplatform = \"{platform}\"\nmin_hostkeeper = \"{floor}\"\n\n{HOOK}{units}"
    )
}

/// The one function that chooses the compression of the package. It goes with
/// `contract::PACKAGE_SUFFIX`: a change of one is a change of the other.
pub(crate) fn compress(tar: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(tar, ruzstd::encoding::CompressionLevel::Fastest)
}

/// Where the units of a package come from.
enum Source<'a> {
    /// The file given with `--unit`.
    File(&'a Path),
    /// None: hostkeeper's package for macos-arm64.
    NoUnit,
    /// Rendered for the platform.
    Rendered,
    /// The mdm package's: its units and the tools out of the archives in this directory.
    Mdm(&'a Path),
}

/// The package the request builds and where its units come from, or why the request asks for
/// something the service's package does not take.
fn source(request: &Request) -> Result<(Package, Source<'_>), Error> {
    let refuse = |why: &str| Err(Error::Failed(why.to_owned()));
    let mac = request.platform == Platform::MacosArm64;
    let root = request.trusted_root.is_some();
    match (request.service, request.unit.as_deref()) {
        (Service::Mdm, _) => mdm_source(request),
        _ if request.archives.is_some() => refuse(ARCHIVES_ONLY_MDM),
        (Service::Hostkeeper, _) if request.fail_ready => refuse(FAIL_READY_ONLY_HELLO),
        (Service::Hostkeeper, _) if !root => refuse(NEEDS_TRUSTED_ROOT),
        (Service::Hello, _) if root => refuse(TRUSTED_ROOT_REFUSED),
        (Service::Hostkeeper, Some(_)) if mac => refuse(MACOS_TAKES_NO_UNIT),
        (Service::Hostkeeper, None) if mac => Ok((Package::Hostkeeper, Source::NoUnit)),
        (Service::Hostkeeper, None) => refuse(LINUX_NEEDS_UNIT),
        (Service::Hostkeeper, Some(unit)) => Ok((Package::Hostkeeper, Source::File(unit))),
        (Service::Hello, Some(_)) => refuse(UNIT_REFUSED),
        (Service::Hello, None) if request.fail_ready => {
            Ok((Package::HelloFailReady, Source::Rendered))
        }
        (Service::Hello, None) => Ok((Package::Hello, Source::Rendered)),
    }
}

/// [`source`] for the mdm package: its units and the directory of its archives, or why the
/// request asks for something it does not take.
fn mdm_source(request: &Request) -> Result<(Package, Source<'_>), Error> {
    let refuse = |why: &str| Err(Error::Failed(why.to_owned()));
    let other_input = request.unit.is_some() | request.trusted_root.is_some() | request.fail_ready;
    match request.archives.as_deref() {
        _ if other_input => refuse(MDM_REFUSED),
        _ if request.platform != Platform::LinuxX86_64 => refuse(MDM_PLATFORM),
        None => refuse(MDM_NEEDS_ARCHIVES),
        Some(dir) => Ok((Package::Mdm, Source::Mdm(dir))),
    }
}

/// Builds the package of `request` and its digest file; the lines to print.
pub(crate) fn package(git: &dyn GitHistory, request: &Request) -> Result<Vec<String>, Error> {
    package_dated(request, || Ok(git.commit_time(&request.commit)?))
}

/// [`package`], with the archive's times from `mtime` (asked for after the binary and the units
/// are read and checked) in place of the commit's time in git.
pub(crate) fn package_dated(
    request: &Request,
    mtime: impl FnOnce() -> Result<u64, Error>,
) -> Result<Vec<String>, Error> {
    package_pinned(request, mtime, super::mdm_fetch::built_in_pins)
}

/// [`package_dated`], with the mdm package's pins from `pins` (asked for only when the package is
/// mdm's) in place of hostkeeper-mdm-core's built-in pins.
fn package_pinned(
    request: &Request,
    mtime: impl FnOnce() -> Result<u64, Error>,
    pins: impl FnOnce() -> Result<hostkeeper_mdm_core::pins::Pins, Error>,
) -> Result<Vec<String>, Error> {
    let (kind, source) = source(request)?;
    let name = request.service.name();
    let binary = read(&request.binary)?;
    let trusted_root = request.trusted_root.as_deref().map(read).transpose()?;
    let contents = match source {
        Source::File(path) => Contents {
            manifest: manifest(&request.version, request.platform, true),
            units: vec![Unit {
                file: UNIT_FILE.to_owned(),
                origin: path.display().to_string(),
                data: read(path)?,
            }],
            trusted_root,
            binaries: Vec::new(),
        },
        Source::NoUnit => Contents {
            manifest: manifest(&request.version, request.platform, false),
            units: Vec::new(),
            trusted_root,
            binaries: Vec::new(),
        },
        Source::Rendered => hello::contents(
            request.platform,
            &kind.version_text(&request.version),
            request.fail_ready,
        )?,
        Source::Mdm(dir) => mdm::contents(
            &pins()?,
            dir,
            &kind.version_text(&request.version),
            &request.version.min(OLDEST_READER).to_string(),
        )?,
    };
    for unit in contents
        .units
        .iter()
        .filter(|u| u.file.ends_with(".service"))
    {
        check_unit(name, &unit.origin, &unit.data)?;
    }
    let entries = entries(name, binary, contents);
    check_entries(&entries)?;
    let mtime = mtime()?;
    let bytes = compress(&archive(&entries, mtime)?);
    let name = kind.package_name(&request.version, request.platform);
    let hex = hex::encode(Sha256::digest(&bytes));
    std::fs::create_dir_all(&request.out)
        .map_err(Error::io(format!("create {}", request.out.display())))?;
    write(&request.out.join(&name), &bytes)?;
    let digest = format!("{hex}  {name}\n");
    write(
        &request.out.join(format!("{name}{DIGEST_SUFFIX}")),
        digest.as_bytes(),
    )?;
    Ok(vec![
        format!("{name}: sha256 {hex}"),
        format!("sha256={hex}"),
    ])
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(Error::io(format!("read {}", path.display())))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    std::fs::write(path, bytes).map_err(Error::io(format!("write {}", path.display())))
}

/// Refuses a unit of `service` that is not UTF-8 text, has no `ExecStart=` whose program is the
/// one it must start ([`started_program`]), or names a package file by any other path than the
/// service's `current` (design section 4.7). `origin` names the unit in the refusal.
fn check_unit(service: &str, origin: &str, unit: &[u8]) -> Result<(), Error> {
    let text = std::str::from_utf8(unit)
        .map_err(|_| Error::Failed(format!("{origin} is not UTF-8 text")))?;
    let started = Started::of(service);
    let starts_it = text.lines().any(|l| {
        l.strip_prefix("ExecStart=")
            .and_then(|command| command.split_whitespace().next())
            .is_some_and(|program| started.allows(program))
    });
    if !starts_it {
        return Err(Error::Failed(format!(
            "{origin} has no ExecStart line {}",
            started.wanted()
        )));
    }
    let current = current(service);
    if text
        .split(PACKAGES)
        .skip(1)
        .any(|rest| !rest.starts_with(&current))
    {
        return Err(Error::Failed(format!(
            "{origin} names a package file by a path other than {PACKAGES}{current}"
        )));
    }
    Ok(())
}

/// The entries of the package of `service`, sorted by name: `units/` only with a unit, `files/`
/// only with a trusted root.
fn entries(service: &str, binary: Vec<u8>, contents: Contents) -> Vec<Entry> {
    let mut entries = vec![
        Entry::dir("bin/"),
        Entry::file(&format!("bin/{service}"), 0o555, binary),
        Entry::file("manifest.toml", 0o444, contents.manifest.into_bytes()),
    ];
    entries.extend(
        contents
            .binaries
            .into_iter()
            .map(|(name, bytes)| Entry::file(&format!("bin/{name}"), 0o555, bytes)),
    );
    if let Some(root) = contents.trusted_root {
        entries.push(Entry::dir("files/"));
        entries.push(Entry::file(TRUSTED_ROOT_ENTRY, 0o444, root));
    }
    if !contents.units.is_empty() {
        entries.push(Entry::dir("units/"));
    }
    entries.extend(
        contents
            .units
            .into_iter()
            .map(|u| Entry::file(&format!("units/{}", u.file), 0o444, u.data)),
    );
    entries.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
    entries
}

/// Why `path` (a name without its closing `/`) cannot be an entry of a package, if it cannot.
fn path_problem(path: &str) -> Option<&'static str> {
    if path.starts_with('/') {
        Some("is an absolute path")
    } else if path.split('/').any(str::is_empty) {
        Some("has an empty component")
    } else if path.split('/').any(|c| c == "..") {
        Some("has a .. component")
    } else {
        None
    }
}

/// The shape rules of design section 5.5 that apply to a list this module builds: top-level
/// names only `manifest.toml`, `bin/`, `units/` and `files/`; no absolute path, no `..`, no empty
/// component; no duplicate name and none equal after ASCII case folding; modes exactly 0555 or
/// 0444. (Links and devices cannot be listed: an [`Entry`] is a file or a directory.)
fn check_entries(entries: &[Entry]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for entry in entries {
        let name = &entry.name;
        let path = name.trim_end_matches('/');
        let top = path.split_once('/').map_or(path, |(top, _)| top);
        if let Some(why) = path_problem(path) {
            problems.push(format!("{name}: {why}"));
        } else if !TOP_LEVEL.contains(&top) {
            problems.push(format!(
                "{name}: top-level name is not manifest.toml, bin/, units/ or files/"
            ));
        }
        if !seen.insert(path.to_ascii_lowercase()) {
            problems.push(format!(
                "{name}: the same name as an earlier entry, ignoring ASCII case"
            ));
        }
        if !MODES.contains(&entry.mode) {
            problems.push(format!("{name}: mode {:o} is not 555 or 444", entry.mode));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Refused(problems))
    }
}

/// The tar of `entries`, every header owned by root, with no names, and dated `mtime`.
fn archive(entries: &[Entry], mtime: u64) -> Result<Vec<u8>, Error> {
    let what = "build the package archive";
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(if entry.directory {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_mode(entry.mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(mtime);
        header.set_size(entry.data.len() as u64);
        builder
            .append_data(&mut header, &entry.name, entry.data.as_slice())
            .map_err(Error::io(what))?;
    }
    builder.into_inner().map_err(Error::io(what))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod trusted_root_tests;
#[cfg(test)]
mod unit_tests;
