//! `release macos-pkg` and `release check-macos-pkg`: the installer pkg of a Mac's first install
//! (design sections 4.6 and 5.9), `hostkeeper-<v>-macos-arm64.pkg` ([`installer_name`]).
//!
//! The pkg installs at `/` exactly five files and one empty directory, and the directories
//! that hold them:
//!
//! - the daemon binary at `hostkeeper_launchd::DAEMON_PROGRAM` (`/usr/local/bin/hostkeeper`),
//!   mode 0755;
//! - the daemon's launchd plist, `/Library/LaunchDaemons/<label>.plist` with the label
//!   `hostkeeper_launchd::Label::daemon` (`dev.hostkeeper.hostkeeper`), mode 0644, the text
//!   `hostkeeper_launchd::daemon_plist` writes for `DaemonJob::standard`;
//! - the directory of the daemon's output files, `hostkeeper_launchd::DAEMON_LOG_DIR`
//!   (`/var/log/hostkeeper`), empty: launchd opens the job's `StandardOutPath` and
//!   `StandardErrorPath` there but does not make their directory;
//! - the newsyslog entries of those two files, `/etc/newsyslog.d/hostkeeper.conf`, mode 0644
//!   ([`newsyslog_text`]): each file is rotated when it reaches [`LOG_SIZE_KIB`] KiB, with
//!   [`LOG_COUNT`] bzip2-compressed archives kept, and never by time;
//! - the pinned `gh` the daemon checks a package's attestation with, at
//!   `/usr/local/lib/hostkeeper/gh` ([`GH_PATH`]), mode 0755;
//! - the Sigstore trusted root that `gh` checks it against, as `gh attestation trusted-root`
//!   prints it, at `/usr/local/lib/hostkeeper/trusted_root.json` ([`TRUSTED_ROOT_PATH`]), mode
//!   0644.
//!
//! `gh` and the trusted root are at the paths the daemon reads on a Mac
//! (`hostkeeper_daemon::cli::HostFiles`). This verb takes both as files and pins neither: the job
//! that builds the pkg downloads `gh` and prints the trusted root with it, each checked against
//! the SHA-256 pinned in the workflow (`cargo xtask install-tool --sha256`), and nothing is
//! downloaded at install.
//!
//! `/etc` and `/var` are symbolic links into `/private` on macOS, so the payload holds the last
//! two under `private/` ([`payload_of`]), the paths the links lead to: a payload directory named
//! `etc` or `var` would stand in place of the link.
//!
//! Every entry is owned by root and wheel (uid 0, gid 0) and every directory is 0755, the modes
//! macOS gives `/`, `/Library`, `/usr`, `/usr/local`, `/private`, `/private/etc`,
//! `/private/etc/newsyslog.d`, `/private/var` and `/private/var/log`, so installing the pkg
//! changes no system directory; `/usr/local/lib` and `/usr/local/lib/hostkeeper` are made with
//! that mode when they are not there. The pkg has no scripts: nothing runs at install, the daemon is
//! not started, and the config is not written (the operator's bootstrap places `hostkeeper.toml`
//! and loads the job).
//!
//! The pkg is not signed: an installer signature needs an Apple Developer ID Installer
//! identity. It is attested like every other asset (build provenance by the artifacts
//! workflow), and an MDM installs it by its digest (design section 5.9). The binary in it keeps
//! the ad-hoc code signature the linker gives it, which the macOS unpack check accepts.
//!
//! [`build`] lays the tree out in a new directory and runs `pkgbuild` over it through the
//! [`BuildPkg`] port ([`PkgbuildCli`] is its adapter). [`check`] reads a pkg that
//! `pkgutil --expand` has expanded into a directory: the directory holds exactly `Bom`,
//! `PackageInfo` and `Payload`; `PackageInfo` names the identifier, the version and the install
//! location and no script; and `Payload`, a gzip-compressed cpio archive (the portable `odc`
//! format), holds exactly the entries above with their modes, owners and bytes (the binary, `gh`
//! and the trusted root those of the files it is given), the plist
//! parsing as a launchd plist that names the daemon's label, binary and output files, and the
//! newsyslog file rotating exactly those output files.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_launchd::plist::{DAEMON_LOG_DIR, DAEMON_PROGRAM};
use hostkeeper_launchd::{DaemonJob, Label, Platform as Launchd, daemon_plist};

use super::contract::{Version, installer_name};
use super::ports::{BuildPkg, Exec, PortError};
use super::{Error, file_sha256};

/// The mode of each directory of the payload.
const DIR_MODE: u32 = 0o755;

/// The mode of the binary.
const BINARY_MODE: u32 = 0o755;

/// The mode of the plist: launchd refuses a plist that its group or others may write.
const PLIST_MODE: u32 = 0o644;

/// The mode of the newsyslog file: newsyslog reads it as root; it holds nothing secret.
const NEWSYSLOG_MODE: u32 = 0o644;

/// The mode of `gh`, which the daemon runs.
const GH_MODE: u32 = 0o755;

/// The mode of the trusted root, which `gh` reads.
const TRUSTED_ROOT_MODE: u32 = 0o644;

/// Where a Mac keeps the pinned `gh`: under `/usr/local`, since `/usr` is read-only on a Mac.
const GH_PATH: &str = "/usr/local/lib/hostkeeper/gh";

/// Where a Mac keeps the Sigstore trusted root.
const TRUSTED_ROOT_PATH: &str = "/usr/local/lib/hostkeeper/trusted_root.json";

/// The file macOS's newsyslog reads the daemon's rotation entries from.
const NEWSYSLOG_CONF: &str = "/etc/newsyslog.d/hostkeeper.conf";

/// The size in KiB at which newsyslog rotates an output file of the daemon.
const LOG_SIZE_KIB: u32 = 10_240;

/// The number of compressed archives newsyslog keeps of each output file.
const LOG_COUNT: u32 = 5;

/// The mode newsyslog gives the new, empty output file and the archives: the daemon's output
/// names hosts and services, so only root and wheel read it.
const LOG_MODE: &str = "640";

/// The flags of each entry: `N`, no process is signalled (the daemon has no pid file, and launchd
/// holds the files for it); `J`, the archives are bzip2-compressed, as macOS's own entries are.
const LOG_FLAGS: &str = "NJ";

/// The top directories of macOS that are symbolic links into `/private` and that the payload
/// installs under (`/tmp`, the third, holds nothing the pkg installs).
const PRIVATE_TOPS: [&str; 2] = ["etc", "var"];

/// The file-type bits of a cpio mode, and the two types a payload may hold.
const TYPE_BITS: u32 = 0o170_000;
const TYPE_DIR: u32 = 0o040_000;
const TYPE_FILE: u32 = 0o100_000;

/// Where the pkg installs its payload.
const INSTALL_LOCATION: &str = "/";

/// The files of an expanded pkg, sorted: a flat component package with no `Scripts`.
const EXPANDED: [&str; 3] = ["Bom", "PackageInfo", "Payload"];

/// The magic of a portable (`odc`) cpio header, its size, and the name of the last entry.
const ODC_MAGIC: &[u8; 6] = b"070707";
const ODC_HEADER: usize = 76;
const TRAILER: &str = "TRAILER!!!";

/// The pkg's identifier: the daemon's launchd label, a reverse-DNS name.
fn identifier() -> String {
    Label::daemon().as_str().to_owned()
}

/// The payload path, from the install location, of the absolute path `installed`: under
/// `private/` when its top directory is one of macOS's links into `/private`, so the installer
/// writes through the link and never puts a directory in its place.
fn payload_of(installed: &str) -> String {
    let path = installed.trim_start_matches('/');
    let top = path.split('/').next().unwrap_or(path);
    if PRIVATE_TOPS.contains(&top) {
        format!("private/{path}")
    } else {
        path.to_owned()
    }
}

/// The path of the binary from the install location: `usr/local/bin/hostkeeper`.
fn binary_path() -> String {
    payload_of(DAEMON_PROGRAM)
}

/// The path of `gh` from the install location: `usr/local/lib/hostkeeper/gh`.
fn gh_path() -> String {
    payload_of(GH_PATH)
}

/// The path of the trusted root from the install location.
fn trusted_root_path() -> String {
    payload_of(TRUSTED_ROOT_PATH)
}

/// The path of the daemon's log directory from the install location:
/// `private/var/log/hostkeeper`.
fn log_dir_path() -> String {
    payload_of(DAEMON_LOG_DIR)
}

/// The path of the newsyslog file from the install location:
/// `private/etc/newsyslog.d/hostkeeper.conf`.
fn newsyslog_path() -> String {
    payload_of(NEWSYSLOG_CONF)
}

/// The daemon's two output files by the plist key that names each: the standard output and the
/// standard error of `DaemonJob::standard`, the job the plist is written for.
fn outputs() -> [(&'static str, String); 2] {
    let job = DaemonJob::standard();
    [
        ("StandardOutPath", job.stdout.display().to_string()),
        ("StandardErrorPath", job.stderr.display().to_string()),
    ]
}

/// The text of the newsyslog file: one entry for each of the daemon's output files, the paths
/// the plist names, rotated by size only (`*` in the time field).
fn newsyslog_text() -> String {
    let mut text = String::from(
        "# The hostkeeper daemon's standard output and standard error, the StandardOutPath and\n\
         # StandardErrorPath of its launchd job. Installed by the hostkeeper pkg.\n\
         # logfilename\t[owner:group]\tmode\tcount\tsize\twhen\tflags\n",
    );
    for (_, path) in outputs() {
        text.push_str(&format!(
            "{path}\troot:wheel\t{LOG_MODE}\t{LOG_COUNT}\t{LOG_SIZE_KIB}\t*\t{LOG_FLAGS}\n"
        ));
    }
    text
}

/// The path of the plist from the install location:
/// `Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist`.
fn plist_path() -> String {
    let mac = Launchd::macos();
    let dir = mac.unit_dir.to_string_lossy().into_owned();
    format!(
        "{}/{}{}",
        dir.trim_start_matches('/'),
        Label::daemon(),
        mac.unit_suffix
    )
}

/// The text of the daemon's plist, as hostkeeper-launchd writes it. The standard job's paths and
/// label are constants that the writer accepts, so the error is built eagerly and no code path
/// exists only for it.
fn plist_text() -> Result<String, Error> {
    let unwritable =
        Error::Failed("the daemon plist of DaemonJob::standard cannot be written".to_owned());
    daemon_plist(&DaemonJob::standard()).ok().ok_or(unwritable)
}

/// The files the pkg is made from, as the verbs are given them.
pub(crate) struct Sources {
    /// The macos-arm64 hostkeeper binary.
    pub(crate) binary: PathBuf,
    /// The pinned darwin-arm64 `gh`.
    pub(crate) gh: PathBuf,
    /// The Sigstore trusted root, as the pinned `gh attestation trusted-root` prints it.
    pub(crate) trusted_root: PathBuf,
}

impl Sources {
    /// The bytes of the files of the payload; `Err` naming the source that cannot be read.
    fn contents(&self) -> Result<Contents, Error> {
        let read = |path: &Path| {
            std::fs::read(path).map_err(Error::io(format!("read {}", path.display())))
        };
        Ok(Contents {
            binary: read(&self.binary)?,
            plist: plist_text()?.into_bytes(),
            gh: read(&self.gh)?,
            trusted_root: read(&self.trusted_root)?,
        })
    }

    /// What a payload file at `path` that holds other bytes than its source is said to differ
    /// from.
    fn source_of(&self, path: &str) -> String {
        let given = [
            (binary_path(), &self.binary),
            (gh_path(), &self.gh),
            (trusted_root_path(), &self.trusted_root),
        ];
        let built = [
            (plist_path(), "the daemon plist hostkeeper-launchd writes"),
            (
                newsyslog_path(),
                "the newsyslog entries of the daemon's output files",
            ),
        ];
        given
            .into_iter()
            .find(|(at, _)| at == path)
            .map(|(_, source)| format!("the bytes of {}", source.display()))
            .or_else(|| {
                built
                    .into_iter()
                    .find(|(at, _)| at == path)
                    .map(|(_, what)| what.to_owned())
            })
            .unwrap_or_default()
    }
}

/// The bytes of each file of the payload that is not built here.
struct Contents {
    binary: Vec<u8>,
    plist: Vec<u8>,
    gh: Vec<u8>,
    trusted_root: Vec<u8>,
}

/// What one entry of the payload must be.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Want {
    Dir,
    File { mode: u32, bytes: Vec<u8> },
}

/// Every entry of the payload by its path from the install location (`""` is the install
/// location itself): the five files, the log directory and each directory that holds them.
fn payload(contents: Contents) -> BTreeMap<String, Want> {
    let mut want = BTreeMap::new();
    let files = [
        (binary_path(), BINARY_MODE, contents.binary),
        (plist_path(), PLIST_MODE, contents.plist),
        (
            newsyslog_path(),
            NEWSYSLOG_MODE,
            newsyslog_text().into_bytes(),
        ),
        (gh_path(), GH_MODE, contents.gh),
        (
            trusted_root_path(),
            TRUSTED_ROOT_MODE,
            contents.trusted_root,
        ),
    ];
    let entries = files
        .into_iter()
        .map(|(path, mode, bytes)| (path, Want::File { mode, bytes }))
        .chain([(log_dir_path(), Want::Dir)]);
    for (path, entry) in entries {
        let mut parent = path.as_str();
        while let Some((dir, _)) = parent.rsplit_once('/') {
            want.insert(dir.to_owned(), Want::Dir);
            parent = dir;
        }
        want.insert(path, entry);
    }
    want.insert(String::new(), Want::Dir);
    want
}

/// What `release macos-pkg` builds the pkg from.
pub(crate) struct Request {
    pub(crate) version: Version,
    /// The binary, `gh` and the trusted root the pkg installs.
    pub(crate) sources: Sources,
    /// The directory the pkg's tree is laid out in; it must not exist.
    pub(crate) stage: PathBuf,
    /// The directory the pkg is written to.
    pub(crate) out: PathBuf,
}

/// Lays the payload out under `request.stage` and builds the pkg from it into `request.out`;
/// the lines to print.
pub(crate) fn build(pkgbuild: &dyn BuildPkg, request: &Request) -> Result<Vec<String>, Error> {
    let contents = request.sources.contents()?;
    let stage = &request.stage;
    if std::fs::symlink_metadata(stage).is_ok() {
        return Err(Error::Failed(format!(
            "{} exists: the pkg's tree is laid out in a new directory, so it holds nothing else",
            stage.display()
        )));
    }
    // In path order, so each directory is made before what it holds; `""` is the stage itself
    // (the components drop the `/` that joining an empty path leaves).
    for (path, want) in payload(contents) {
        let at: PathBuf = stage.join(&path).components().collect();
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
    let name = installer_name(&request.version);
    let pkg = request.out.join(&name);
    pkgbuild.build_pkg(stage, &identifier(), &request.version, &pkg)?;
    let hex = file_sha256(&pkg)?;
    Ok(vec![format!("{name}: sha256 {hex}")])
}

/// Runs `pkgbuild` through an [`Exec`].
pub(crate) struct PkgbuildCli<E> {
    exec: E,
}

impl<E> PkgbuildCli<E> {
    /// The adapter running `pkgbuild` through `exec`.
    pub(crate) fn new(exec: E) -> PkgbuildCli<E> {
        PkgbuildCli { exec }
    }
}

/// `path` as UTF-8, for a command line.
fn utf8(path: &Path) -> Result<String, PortError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        PortError::new(
            format!("pkgbuild {}", path.display()),
            "the path is not UTF-8",
        )
    })
}

impl<E: Exec> BuildPkg for PkgbuildCli<E> {
    fn build_pkg(
        &self,
        root: &Path,
        identifier: &str,
        version: &Version,
        pkg: &Path,
    ) -> Result<(), PortError> {
        let args = vec![
            "--root".to_owned(),
            utf8(root)?,
            "--identifier".to_owned(),
            identifier.to_owned(),
            "--version".to_owned(),
            version.to_string(),
            "--install-location".to_owned(),
            INSTALL_LOCATION.to_owned(),
            "--ownership".to_owned(),
            "recommended".to_owned(),
            utf8(pkg)?,
        ];
        let what = format!("pkgbuild {}", args.join(" "));
        let ran = self
            .exec
            .spawn("pkgbuild", &args)
            .map_err(|e| PortError::new(&what, e.to_string()))?;
        if ran.success {
            Ok(())
        } else {
            Err(PortError::new(
                what,
                String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
            ))
        }
    }
}

/// One entry of a cpio archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CpioEntry {
    pub(crate) name: String,
    pub(crate) mode: u32,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) data: Vec<u8>,
}

/// The number written as `len` octal digits at `at` of `bytes`, which holds them.
fn octal(bytes: &[u8], at: usize, len: usize) -> Result<u64, String> {
    let field = &bytes[at..at + len];
    let digits = field.iter().all(|b| matches!(b, b'0'..=b'7'));
    let text = std::str::from_utf8(field).ok().filter(|_| digits);
    text.and_then(|t| u64::from_str_radix(t, 8).ok())
        .ok_or_else(|| format!("the header field at byte {at} is not {len} octal digits"))
}

/// The bytes `bytes[at..at + len]`, or why the archive ends before them.
fn take(bytes: &[u8], at: usize, len: u64, what: &str) -> Result<Vec<u8>, String> {
    usize::try_from(len)
        .ok()
        .and_then(|len| at.checked_add(len))
        .and_then(|end| bytes.get(at..end))
        .map(<[u8]>::to_vec)
        .ok_or_else(|| format!("the archive ends inside the {what} that starts at byte {at}"))
}

/// The entries of the portable (`odc`) cpio archive `bytes`, up to its trailer; anything after
/// the trailer (the padding of a block) is not read.
pub(crate) fn read_odc(bytes: &[u8]) -> Result<Vec<CpioEntry>, String> {
    let mut entries = Vec::new();
    let mut at = 0;
    loop {
        if bytes.len() - at < ODC_HEADER {
            return Err(format!(
                "the archive ends at byte {} without its trailer",
                bytes.len()
            ));
        }
        if &bytes[at..at + 6] != ODC_MAGIC {
            return Err(format!(
                "the header at byte {at} starts {:?}, not the odc magic 070707",
                String::from_utf8_lossy(&bytes[at..at + 6])
            ));
        }
        let field = |offset: usize, len: usize| octal(bytes, at + offset, len);
        let mode = field(18, 6)?;
        let uid = field(24, 6)?;
        let gid = field(30, 6)?;
        let name_size = field(59, 6)?;
        let file_size = field(65, 11)?;
        let name_at = at + ODC_HEADER;
        let name = take(bytes, name_at, name_size, "name")?;
        let name = match name.split_last() {
            Some((0, name)) => String::from_utf8(name.to_vec())
                .map_err(|_| format!("the name at byte {name_at} is not UTF-8"))?,
            _ => return Err(format!("the name at byte {name_at} does not end in NUL")),
        };
        if name == TRAILER {
            return Ok(entries);
        }
        let data_at = name_at + name.len() + 1;
        let data = take(bytes, data_at, file_size, "data")?;
        at = data_at + data.len();
        // Six octal digits hold at most 0o777777, so each fits a u32.
        let small = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        entries.push(CpioEntry {
            name,
            mode: small(mode),
            uid: small(uid),
            gid: small(gid),
            data,
        });
    }
}

/// The path of a payload entry from the install location: `.` is `""`, `./x` is `x`; any other
/// name is kept, and so matches no entry of the payload.
fn payload_path(name: &str) -> &str {
    match name {
        "." => "",
        _ => name.strip_prefix("./").unwrap_or(name),
    }
}

/// How a payload path is shown: from `/`.
fn shown(path: &str) -> String {
    format!("/{path}")
}

/// The attributes of the `<pkg-info ...>` element of `text`, or none when it has no such
/// element or its attributes are not `name="value"` pairs.
fn pkg_info_attributes(text: &str) -> Option<BTreeMap<String, String>> {
    let start = text.find("<pkg-info")? + "<pkg-info".len();
    let mut rest = &text[start..];
    let mut found = BTreeMap::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with('>') | rest.starts_with("/>") {
            return Some(found);
        }
        let (name, after) = rest.split_once("=\"")?;
        let (value, after) = after.split_once('"')?;
        let name = name.trim();
        let well_formed = !name.is_empty()
            & name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'-' | b'_' | b':'));
        if !well_formed {
            return None;
        }
        found.insert(name.to_owned(), value.to_owned());
        rest = after;
    }
}

/// What is wrong with the `PackageInfo` text `text` of the pkg of `version`, one line each.
fn package_info_problems(text: &str, version: &Version, problems: &mut Vec<String>) {
    let Some(attributes) = pkg_info_attributes(text) else {
        return problems.push("PackageInfo has no readable <pkg-info> element".to_owned());
    };
    let wanted = [
        ("identifier", identifier()),
        ("version", version.to_string()),
        ("install-location", INSTALL_LOCATION.to_owned()),
    ];
    for (name, want) in wanted {
        match attributes.get(name) {
            Some(value) if *value == want => {}
            Some(value) => {
                problems.push(format!("PackageInfo: {name} is {value:?}, not {want:?}"));
            }
            None => problems.push(format!("PackageInfo has no {name}")),
        }
    }
    let action = attributes.get("postinstall-action").map(String::as_str);
    if action.is_some_and(|a| a != "none") {
        problems.push(format!(
            "PackageInfo: postinstall-action is {:?}, not \"none\"",
            action.unwrap_or_default()
        ));
    }
    for script in ["<preinstall", "<postinstall"] {
        if text.contains(script) {
            problems.push(format!(
                "PackageInfo names a script ({script}>): the pkg runs nothing at install"
            ));
        }
    }
}

/// What is wrong with the launchd plist `bytes` of the payload, one line each: it must parse as
/// a plist dictionary naming the daemon's label, running the binary of the payload, and sending
/// its output to the files the newsyslog file rotates, in the log directory of the payload.
fn plist_problems(bytes: &[u8], problems: &mut Vec<String>) {
    let what = shown(&plist_path());
    let value = match plist::Value::from_reader_xml(bytes) {
        Ok(value) => value,
        Err(e) => return problems.push(format!("{what} does not parse as a plist: {e}")),
    };
    let Some(dict) = value.as_dictionary() else {
        return problems.push(format!("{what} is not a plist dictionary"));
    };
    let label = dict.get("Label").and_then(plist::Value::as_string);
    if label != Some(Label::daemon().as_str()) {
        problems.push(format!(
            "{what}: the label is {label:?}, not {:?}",
            Label::daemon().as_str()
        ));
    }
    let program = dict
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .and_then(|a| a.first())
        .and_then(plist::Value::as_string);
    if program != Some(DAEMON_PROGRAM) {
        problems.push(format!(
            "{what}: the job runs {program:?}, not the pkg's binary {DAEMON_PROGRAM:?}"
        ));
    }
    for (key, want) in outputs() {
        let got = dict.get(key).and_then(plist::Value::as_string);
        if got != Some(want.as_str()) {
            problems.push(format!(
                "{what}: {key} is {got:?}, not {want:?}, the file the pkg's newsyslog entry \
                 rotates in the directory the pkg makes"
            ));
        }
    }
}

/// What is wrong with the newsyslog file `bytes` of the payload: the paths of its entries (the
/// first field of each line that is neither blank nor a `#` comment) must be exactly the
/// daemon's `StandardOutPath` and `StandardErrorPath`, in that order.
fn newsyslog_problems(bytes: &[u8], problems: &mut Vec<String>) {
    let text = String::from_utf8_lossy(bytes);
    let named: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() & !line.starts_with('#'))
        .map(|line| line.split_whitespace().next().unwrap_or(line))
        .collect();
    let want: Vec<String> = outputs().into_iter().map(|(_, path)| path).collect();
    if named != want {
        problems.push(format!(
            "{}: rotates {named:?}, not the daemon's StandardOutPath and StandardErrorPath \
             {want:?}",
            shown(&newsyslog_path())
        ));
    }
}

/// A read-back of one file of the payload: what is wrong with its bytes, one line each.
type ReadBack = fn(&[u8], &mut Vec<String>);

/// What is wrong with the entries of the payload against `want`, one line each.
fn payload_problems(
    entries: &[CpioEntry],
    mut want: BTreeMap<String, Want>,
    sources: &Sources,
    problems: &mut Vec<String>,
) {
    for entry in entries {
        let path = payload_path(&entry.name);
        let what = shown(path);
        let Some(wanted) = want.remove(path) else {
            problems.push(format!(
                "{what} ({:?}) is in the payload, which holds only the binary, the plist, the \
                 newsyslog file, the log directory, gh, the trusted root and their directories, \
                 each once",
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
            let want_word = if want_kind == TYPE_DIR {
                "a directory"
            } else {
                "a regular file"
            };
            problems.push(format!("{what}: of type {kind:o}, not {want_word}"));
            continue;
        }
        if mode != want_mode {
            problems.push(format!("{what}: mode {mode:o}, not {want_mode:o}"));
        }
        if let Want::File { bytes, .. } = &wanted
            && entry.data != *bytes
        {
            problems.push(format!("{what}: not {}", sources.source_of(path)));
        }
    }
    problems.extend(
        want.keys()
            .map(|path| format!("missing {} in the payload", shown(path))),
    );
}

/// The `release check-macos-pkg <expanded> --version V --binary B --gh G --trusted-root R`
/// verb: checks the pkg that `pkgutil --expand` expanded into `expanded` against `version` and
/// the files of `sources`, and lists every problem it finds.
pub(crate) fn check(
    expanded: &Path,
    version: &Version,
    sources: &Sources,
) -> Result<Vec<String>, Error> {
    let what = format!("read {}", expanded.display());
    let mut names: Vec<(String, bool)> = std::fs::read_dir(expanded)
        .and_then(|read| read.collect::<Result<Vec<_>, _>>())
        .map_err(Error::io(&what))?
        .iter()
        .map(|e| {
            let file = e.file_type().is_ok_and(|kind| kind.is_file());
            (e.file_name().to_string_lossy().into_owned(), file)
        })
        .collect();
    names.sort();
    let mut problems = Vec::new();
    for (name, file) in &names {
        if !EXPANDED.contains(&name.as_str()) | !file {
            problems.push(format!(
                "{name}: an expanded pkg of hostkeeper holds only the files Bom, PackageInfo and \
                 Payload (no Scripts)"
            ));
        }
    }
    let present = |want: &str| names.iter().any(|(name, file)| (name == want) & file);
    for missing in EXPANDED.iter().filter(|n| !present(n)) {
        problems.push(format!("missing {missing}"));
    }
    if present("PackageInfo") {
        let path = expanded.join("PackageInfo");
        let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
        package_info_problems(&String::from_utf8_lossy(&bytes), version, &mut problems);
    }
    if present("Payload") {
        let path = expanded.join("Payload");
        let file =
            std::fs::File::open(&path).map_err(Error::io(format!("read {}", path.display())))?;
        let mut cpio = Vec::new();
        let read = flate2::read::GzDecoder::new(file)
            .read_to_end(&mut cpio)
            .map_err(|e| format!("Payload is not gzip data: {e}"))
            .and_then(|_| read_odc(&cpio).map_err(|e| format!("Payload: {e}")));
        match read {
            Err(why) => problems.push(why),
            Ok(entries) => {
                let contents = sources.contents()?;
                let read_back: [(String, ReadBack); 2] = [
                    (plist_path(), plist_problems),
                    (newsyslog_path(), newsyslog_problems),
                ];
                for (path, problems_of) in read_back {
                    if let Some(entry) = entries.iter().find(|e| payload_path(&e.name) == path) {
                        problems_of(&entry.data, &mut problems);
                    }
                }
                payload_problems(&entries, payload(contents), sources, &mut problems);
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "check-macos-pkg: {} installs exactly {} ({BINARY_MODE:o}), {} ({PLIST_MODE:o}), {} \
         ({NEWSYSLOG_MODE:o}), {GH_PATH} ({GH_MODE:o}), {TRUSTED_ROOT_PATH} \
         ({TRUSTED_ROOT_MODE:o}) and the directory {} ({DIR_MODE:o}), owned by root and wheel, \
         with the label {}, version {version} and no scripts",
        expanded.display(),
        shown(&binary_path()),
        shown(&plist_path()),
        shown(&newsyslog_path()),
        shown(&log_dir_path()),
        Label::daemon()
    )])
}

#[cfg(test)]
mod check_tests;
#[cfg(test)]
mod tests;
