//! `release macos-pkg` and `release check-macos-pkg`: the installer pkg of a Mac's first install
//! (design sections 4.6 and 5.9), `hostkeeper-<v>-macos-arm64.pkg` ([`installer_name`]).
//!
//! The pkg installs at `/` exactly five files and one empty directory, and the directories
//! that hold them:
//!
//! - the daemon binary at `hostkeeper_launchd::plist::INSTALLED_BINARY`
//!   (`/usr/local/bin/hostkeeper`), mode 0755;
//! - the daemon's launchd plist, `/Library/LaunchDaemons/<label>.plist` with the label
//!   `hostkeeper_launchd::Label::daemon` (`dev.hostkeeper.hostkeeper`), mode 0644, the text
//!   `hostkeeper_launchd::daemon_plist` writes for `DaemonJob::standard`, whose program is the
//!   binary above: `hostkeeper daemon` started from that path seeds the binary directory of
//!   hostkeeper's own update and execs `current` of it (`hostkeeper_daemon::packaged`);
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
//! that mode when they are not there.
//!
//! The pkg has one script, `postinstall` ([`scripts`]), which asks launchd at install
//! whether the daemon's job is loaded (`launchctl print`) and, only when it is not, runs
//! `launchctl bootstrap`, which loads the job the pkg installed; a loaded job is never booted
//! out, so an installer the daemon itself runs does not stop the daemon. It runs the installed binary at no point: the daemon's
//! start does what the script once did (`hostkeeper_daemon::packaged` seeds the binary directory
//! of hostkeeper's own update and execs `current`, `hostkeeper_daemon::mac_config::at_start`
//! writes `/etc/hostkeeper/hostkeeper.toml` from the managed preferences the MDM installed). The
//! pkg carries no config of its own.
//!
//! The file the verbs name is a distribution pkg ([`distribution`]): `pkgbuild` builds the
//! component pkg that holds the payload and the script, and `productbuild` wraps it, as the one
//! package of the product, in the file the MDM installs.
//!
//! The pkg is not signed. Which identity will sign it (a farm installer identity whose root the
//! MDM installs, or Apple's Developer ID Installer) is decided by the device test of
//! `docs/design/hostkeeper-mdm.md` section 7.2 (`cargo xtask mdm-signing-test`). It is attested like every other asset (build provenance by the artifacts
//! workflow), and an MDM installs it by its digest (design section 5.9). The binary in it keeps
//! the ad-hoc code signature the linker gives it, which the macOS unpack check accepts.
//!
//! [`build`] lays the payload's tree and the scripts directory out in a new directory, runs
//! `pkgbuild` over them into the component pkg in that directory, and runs `productbuild` over
//! the component into the output directory, through the [`BuildPkg`] port ([`PkgbuildCli`] is its
//! adapter). [`check`] reads a product archive that `pkgutil --expand` has expanded into a
//! directory: the directory holds exactly the file `Distribution` ([`distribution`] says what it
//! must hold) and the directory of the component ([`COMPONENT`]); the component's directory holds
//! exactly the files `Bom`, `PackageInfo` and `Payload` and the directory `Scripts`, into which
//! `pkgutil --expand` expands the pkg's scripts archive; `PackageInfo` names the identifier, the
//! version, the install location and the `postinstall` and no other script; `Scripts` holds that
//! `postinstall` alone, a regular file of mode 0755; and `Payload`, a gzip-compressed cpio
//! archive (the portable `odc` format) that `pkgutil --expand` does not expand, holds exactly
//! the entries above with their modes, owners and bytes (the binary, `gh` and the trusted root
//! those of the files it is given), the plist parsing as a launchd plist that names the daemon's
//! label, the program the installed binary and the output files, and the newsyslog
//! file rotating exactly those output files.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_launchd::plist::{DAEMON_LOG_DIR, INSTALLED_BINARY};
use hostkeeper_launchd::{DaemonJob, Label, Platform as Launchd, daemon_plist};

use super::contract::{Version, installer_name};
use super::ports::{BuildPkg, Exec, PortError};
use super::{Error, file_sha256};

pub(crate) mod distribution;
mod scripts;

use distribution::{COMPONENT, DISTRIBUTION, distribution_problems, open_tag};

use scripts::{
    POSTINSTALL, SCRIPT_MODE, package_info_scripts_problems, postinstall_text, scripts_problems,
};

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

/// The files of an expanded component pkg, sorted, and its one directory: `pkgutil --expand`
/// leaves a component package's `Payload` an archive and expands its scripts archive into
/// `Scripts`.
const EXPANDED_FILES: [&str; 3] = ["Bom", "PackageInfo", "Payload"];
const EXPANDED_SCRIPTS: &str = "Scripts";

/// The directories of the stage: the payload's tree and the scripts directory.
const STAGE_ROOT: &str = "root";
const STAGE_SCRIPTS: &str = "scripts";

/// What the top directory of an expanded product archive holds: the file `Distribution` and the
/// directory of the component.
const PRODUCT_FILES: [&str; 1] = [DISTRIBUTION];
const PRODUCT_DIRS: [&str; 1] = [COMPONENT];

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
    payload_of(INSTALLED_BINARY)
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
    /// The directory the pkg is laid out in, which must not exist: the payload's tree in `root`,
    /// the scripts directory in `scripts` and the component pkg `pkgbuild` writes ([`COMPONENT`]).
    pub(crate) stage: PathBuf,
    /// The directory the product pkg is written to.
    pub(crate) out: PathBuf,
}

/// Makes the directory `at` with [`DIR_MODE`].
pub(crate) fn make_dir(at: &Path) -> Result<(), Error> {
    std::fs::create_dir(at).map_err(Error::io(format!("create {}", at.display())))?;
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(DIR_MODE))
        .map_err(Error::io(format!("set the mode of {}", at.display())))
}

/// Lays the payload out under `request.stage`'s `root` and the scripts under its `scripts`,
/// builds the component pkg from them into the stage, and wraps it as the product pkg in
/// `request.out`; the lines to print, which name the product with its digest.
pub(crate) fn build(pkgbuild: &dyn BuildPkg, request: &Request) -> Result<Vec<String>, Error> {
    let contents = request.sources.contents()?;
    let stage = &request.stage;
    if std::fs::symlink_metadata(stage).is_ok() {
        return Err(Error::Failed(format!(
            "{} exists: the pkg's tree is laid out in a new directory, so it holds nothing else",
            stage.display()
        )));
    }
    make_dir(stage)?;
    let root = stage.join(STAGE_ROOT);
    let scripts = stage.join(STAGE_SCRIPTS);
    make_dir(&scripts)?;
    let script = scripts.join(POSTINSTALL);
    std::fs::write(&script, postinstall_text())
        .map_err(Error::io(format!("write {}", script.display())))?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(SCRIPT_MODE))
        .map_err(Error::io(format!("set the mode of {}", script.display())))?;
    // In path order, so each directory is made before what it holds; `""` is the payload's root
    // itself (the components drop the `/` that joining an empty path leaves).
    for (path, want) in payload(contents) {
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
    let component = stage.join(COMPONENT);
    let version = request.version.to_string();
    pkgbuild.build_pkg(&root, Some(&scripts), &identifier(), &version, &component)?;
    let name = installer_name(&request.version);
    let product = request.out.join(&name);
    pkgbuild.build_product(&component, &identifier(), &version, &product)?;
    let hex = file_sha256(&product)?;
    Ok(vec![format!("{name}: sha256 {hex}")])
}

/// Runs `pkgbuild`, `productbuild` and `pkgutil --check-signature` through an [`Exec`].
pub(crate) struct PkgbuildCli<E> {
    exec: E,
}

impl<E> PkgbuildCli<E> {
    /// The adapter running `pkgbuild` and `productbuild` through `exec`.
    pub(crate) fn new(exec: E) -> PkgbuildCli<E> {
        PkgbuildCli { exec }
    }
}

/// `path` as UTF-8, for the command line of `program`.
fn utf8(program: &str, path: &Path) -> Result<String, PortError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        PortError::new(
            format!("{program} {}", path.display()),
            "the path is not UTF-8",
        )
    })
}

impl<E: Exec> PkgbuildCli<E> {
    /// Runs `program` with `args`; a failure to start or a failed status is a [`PortError`] that
    /// names the command line.
    fn run(&self, program: &str, args: &[String]) -> Result<(), PortError> {
        let what = format!("{program} {}", args.join(" "));
        let ran = self
            .exec
            .spawn(program, args)
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

impl<E: Exec> BuildPkg for PkgbuildCli<E> {
    fn build_pkg(
        &self,
        root: &Path,
        scripts: Option<&Path>,
        identifier: &str,
        version: &str,
        pkg: &Path,
    ) -> Result<(), PortError> {
        let mut args = vec!["--root".to_owned(), utf8("pkgbuild", root)?];
        if let Some(scripts) = scripts {
            args.push("--scripts".to_owned());
            args.push(utf8("pkgbuild", scripts)?);
        }
        args.extend([
            "--identifier".to_owned(),
            identifier.to_owned(),
            "--version".to_owned(),
            version.to_owned(),
            "--install-location".to_owned(),
            INSTALL_LOCATION.to_owned(),
            "--ownership".to_owned(),
            "recommended".to_owned(),
            utf8("pkgbuild", pkg)?,
        ]);
        self.run("pkgbuild", &args)
    }

    fn build_product(
        &self,
        component: &Path,
        identifier: &str,
        version: &str,
        product: &Path,
    ) -> Result<(), PortError> {
        let args = vec![
            "--package".to_owned(),
            utf8("productbuild", component)?,
            "--identifier".to_owned(),
            identifier.to_owned(),
            "--version".to_owned(),
            version.to_owned(),
            utf8("productbuild", product)?,
        ];
        self.run("productbuild", &args)
    }

    fn check_signature(&self, pkg: &Path) -> Result<String, PortError> {
        let args = vec!["--check-signature".to_owned(), utf8("pkgutil", pkg)?];
        let what = format!("pkgutil {}", args.join(" "));
        let ran = self
            .exec
            .spawn("pkgutil", &args)
            .map_err(|e| PortError::new(&what, e.to_string()))?;
        let stdout = String::from_utf8_lossy(&ran.stdout).into_owned();
        if ran.success {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&ran.stderr);
            Err(PortError::new(
                what,
                format!("{}\n{}", stdout.trim(), stderr.trim())
                    .trim()
                    .to_owned(),
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
pub(crate) fn payload_path(name: &str) -> &str {
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
pub(crate) fn pkg_info_attributes(text: &str) -> Option<BTreeMap<String, String>> {
    let start = text.find("<pkg-info")? + "<pkg-info".len();
    open_tag(&text[start..]).map(|(attributes, ..)| attributes)
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
    package_info_scripts_problems(text, problems);
}

/// What is wrong with the launchd plist `bytes` of the payload, one line each: it must parse as
/// a plist dictionary naming the daemon's label, running the binary of the payload (which, started
/// as `hostkeeper daemon` from that path, seeds the binary directory of hostkeeper's own update
/// and execs `current` of it), and sending its output to the files the newsyslog file rotates, in the log directory of the payload.
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
    if program != Some(INSTALLED_BINARY) {
        problems.push(format!(
            "{what}: the job runs {program:?}, not {INSTALLED_BINARY:?}, the binary the pkg \
             installs, whose start seeds the binary directory of a self-update and runs current"
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

/// The entries of the `Payload` of the expanded pkg `expanded`, a gzip-compressed portable cpio
/// archive; the inner `Err` says why its bytes are not such an archive, the outer one that the
/// file could not be opened.
pub(crate) fn read_payload(expanded: &Path) -> Result<Result<Vec<CpioEntry>, String>, Error> {
    let name = "Payload";
    let path = expanded.join(name);
    let file = std::fs::File::open(&path).map_err(Error::io(format!("read {}", path.display())))?;
    let mut cpio = Vec::new();
    Ok(flate2::read::GzDecoder::new(file)
        .read_to_end(&mut cpio)
        .map_err(|e| format!("{name} is not gzip data: {e}"))
        .and_then(|_| read_odc(&cpio).map_err(|e| format!("{name}: {e}"))))
}

/// One entry of a directory of an expanded pkg: its name and whether it is a regular file and
/// whether it is a directory; a symbolic link is neither (`DirEntry::file_type` does not follow
/// it).
struct Listed {
    name: String,
    file: bool,
    dir: bool,
}

/// The entries of `dir`, sorted by name.
fn list(dir: &Path) -> Result<Vec<Listed>, Error> {
    let mut listed: Vec<Listed> = std::fs::read_dir(dir)
        .and_then(|read| read.collect::<Result<Vec<_>, _>>())
        .map_err(Error::io(format!("read {}", dir.display())))?
        .iter()
        .map(|e| {
            let kind = e.file_type().ok();
            Listed {
                name: e.file_name().to_string_lossy().into_owned(),
                file: kind.is_some_and(|k| k.is_file()),
                dir: kind.is_some_and(|k| k.is_dir()),
            }
        })
        .collect();
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(listed)
}

/// Checks that the directory `dir` of an expanded pkg holds exactly the regular files `files` and
/// the directories `dirs`: every other entry is a problem that says the directory `holds only`
/// `only`, and every one missing is a problem. The names that are there, as they must be.
pub(crate) fn layer(
    dir: &Path,
    files: &[&str],
    dirs: &[&str],
    only: &str,
    problems: &mut Vec<String>,
) -> Result<Vec<String>, Error> {
    let listed = list(dir)?;
    let present = |want: &str| {
        listed.iter().any(|e| {
            (e.name == want) & ((e.file & files.contains(&want)) | (e.dir & dirs.contains(&want)))
        })
    };
    for entry in &listed {
        if !present(&entry.name) {
            problems.push(format!("{}: {only}", entry.name));
        }
    }
    let wanted = files.iter().chain(dirs);
    for missing in wanted.clone().filter(|name| !present(name)) {
        problems.push(format!("missing {missing}"));
    }
    Ok(wanted
        .filter(|name| present(name))
        .map(|name| (*name).to_owned())
        .collect())
}

/// What is wrong with the component pkg expanded into `component`, one line each: its parts, its
/// `PackageInfo`, its scripts and its payload.
fn component_problems(
    component: &Path,
    version: &Version,
    sources: &Sources,
    problems: &mut Vec<String>,
) -> Result<(), Error> {
    let only = "an expanded pkg of hostkeeper holds only the files Bom, PackageInfo and Payload \
                and the directory Scripts";
    let present = layer(
        component,
        &EXPANDED_FILES,
        &[EXPANDED_SCRIPTS],
        only,
        problems,
    )?;
    let has = |name: &str| present.iter().any(|p| p == name);
    if has("PackageInfo") {
        let path = component.join("PackageInfo");
        let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
        package_info_problems(&String::from_utf8_lossy(&bytes), version, problems);
    }
    if has(EXPANDED_SCRIPTS) {
        scripts_problems(&component.join(EXPANDED_SCRIPTS), problems)?;
    }
    if has("Payload") {
        match read_payload(component)? {
            Err(why) => problems.push(why),
            Ok(entries) => {
                let contents = sources.contents()?;
                let read_back: [(String, ReadBack); 2] = [
                    (plist_path(), plist_problems),
                    (newsyslog_path(), newsyslog_problems),
                ];
                for (path, problems_of) in read_back {
                    if let Some(entry) = entries.iter().find(|e| payload_path(&e.name) == path) {
                        problems_of(&entry.data, problems);
                    }
                }
                payload_problems(&entries, payload(contents), sources, problems);
            }
        }
    }
    Ok(())
}

/// The `release check-macos-pkg <expanded> --version V --binary B --gh G --trusted-root R`
/// verb: checks the product archive that `pkgutil --expand` expanded into `expanded` against
/// `version` and the files of `sources`, and lists every problem it finds: the archive holds the
/// `Distribution` and the one component, and the component is the pkg described above.
pub(crate) fn check(
    expanded: &Path,
    version: &Version,
    sources: &Sources,
) -> Result<Vec<String>, Error> {
    let mut problems = Vec::new();
    let only = format!(
        "an expanded product archive of hostkeeper holds only the file {DISTRIBUTION} and the \
         directory {COMPONENT}"
    );
    let present = layer(
        expanded,
        &PRODUCT_FILES,
        &PRODUCT_DIRS,
        &only,
        &mut problems,
    )?;
    let has = |name: &str| present.iter().any(|p| p == name);
    if has(DISTRIBUTION) {
        let path = expanded.join(DISTRIBUTION);
        let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
        distribution_problems(&String::from_utf8_lossy(&bytes), version, &mut problems);
    }
    if has(COMPONENT) {
        component_problems(&expanded.join(COMPONENT), version, sources, &mut problems)?;
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "check-macos-pkg: {} is a product archive of the one component {COMPONENT}, which \
         installs exactly {} ({BINARY_MODE:o}), {} ({PLIST_MODE:o}), {} ({NEWSYSLOG_MODE:o}), \
         {GH_PATH} ({GH_MODE:o}), {TRUSTED_ROOT_PATH} ({TRUSTED_ROOT_MODE:o}) and the directory \
         {} ({DIR_MODE:o}), owned by root and wheel, with the label {}, version {version} and \
         the one script {POSTINSTALL}",
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
pub(crate) mod tests;
