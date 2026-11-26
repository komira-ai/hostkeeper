//! The archive of `hk xcode add`: its SHA-256 and size, and the version and build of the app in
//! it, which `xip --expand` makes and `codesign` verifies. Which file it is, and how it is
//! downloaded, is `crate::xcode_add::find`.

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest as _, Sha256};

use super::{Mac, command, failed, finish, say};
use crate::xcode_record::Output;
use crate::{Flow, World};

/// The most bytes of a build: as the coordinator's store takes it.
const MAX_BUILD_BYTES: usize = 32;

/// Whether `text` is an Xcode version: decimal numbers joined by dots, `27.2` or `26.1.1`.
pub(crate) fn is_version(text: &[u8]) -> bool {
    !text.is_empty()
        && text
            .split(|b| *b == b'.')
            .all(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
}

/// Whether `text` is an Xcode build: 1 to 32 ASCII letters, digits and dots, `27B52`.
pub(crate) fn is_build(text: &[u8]) -> bool {
    (1..=MAX_BUILD_BYTES).contains(&text.len())
        && text.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'.')
}

/// The archive at `path`, open for reading: it must be a file. The error says why it is not.
pub(crate) fn open_file(path: &Path) -> Result<File, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot read {} ({error})", path.display()))?;
    if file.metadata().is_ok_and(|meta| meta.is_file()) {
        Ok(file)
    } else {
        Err(format!("{} is not a file", path.display()))
    }
}

/// The archive at `path`, open for reading: it must be a file.
pub(crate) fn open(path: &Path) -> Flow<File> {
    open_file(path).map_err(|why| failed(&why))
}

/// The lower-case hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of the archive `file`, in lower-case hex, and its size in bytes: read from its
/// start to its end once, in blocks of `block` bytes, never whole; `seen` is told how many bytes
/// are through after each block. The error says why it could not be read.
pub(crate) fn digest_blocks(
    file: &mut File,
    block: usize,
    mut seen: impl FnMut(u64),
) -> Result<(String, u64), String> {
    let mut sha = Sha256::new();
    let mut size: u64 = 0;
    let mut piece = vec![0_u8; block];
    loop {
        let n = file
            .read(&mut piece)
            .map_err(|error| format!("cannot read the archive ({error})"))?;
        if n == 0 {
            break;
        }
        sha.update(piece.get(..n).unwrap_or_default());
        size = size.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        seen(size);
    }
    Ok((hex(&sha.finalize()), size))
}

/// [`digest_blocks`] in blocks of 1 MiB, with nothing said on the way.
pub(crate) fn digest_of(file: &mut File) -> Result<(String, u64), String> {
    digest_blocks(file, 1 << 20, |_| {})
}

/// The directory an archive is expanded into: removed, with all it holds, when it is dropped.
pub(crate) struct Expansion(pub(crate) PathBuf);

impl Drop for Expansion {
    fn drop(&mut self) {
        // What cannot be removed is left; the push has its answer either way.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The value of `key` in the property list `file`, as `plutil -extract <key> raw` prints it.
fn plist_value(mac: &Mac, file: &Path, key: &str) -> Flow<String> {
    let mut plutil = command(&mac.plutil);
    plutil
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(file)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    let what = format!("{} -extract {key} {}", mac.plutil.display(), file.display());
    let value = finish(plutil, &what)?;
    Ok(String::from_utf8_lossy(&value).trim().to_owned())
}

/// The name of the directory beside the archive that it is expanded into: one name, so that what
/// a run stopped by a signal left there is removed by the next.
pub(crate) const EXPANSION: &str = ".hk-xcode-push-expansion";

/// What the expanded app says: the version and the build it is, and `codesign` on it.
pub(crate) struct Release {
    pub(crate) version: String,
    pub(crate) build: String,
    pub(crate) codesign: Output,
}

/// The version and the build of the Xcode in the archive at `path`: `xip --expand` makes the app
/// in [`EXPANSION`] beside the archive (on its volume, so that no other disk fills), removed
/// first if it is there, and the version is the app's `CFBundleShortVersionString`
/// (`Contents/Info.plist`) and the build its `ProductBuildVersion` (`Contents/version.plist`).
/// `codesign --verify --deep --strict` must pass on the app. The directory is removed afterwards.
pub(crate) fn release(w: &mut World<'_>, mac: &Mac, path: &Path) -> Flow<Release> {
    let archive = std::path::absolute(path)
        .map_err(|error| failed(&format!("cannot resolve {} ({error})", path.display())))?;
    let beside = archive.parent().unwrap_or(Path::new("/"));
    let dir = beside.join(EXPANSION);
    // What an earlier run left: it is not there, or it is removed, or making it fails below.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir)
        .map_err(|error| failed(&format!("cannot make {} ({error})", dir.display())))?;
    let expansion = Expansion(dir);
    say(
        w,
        &format!(
            "reading the version and the build: {} --expand into {}",
            mac.xip.display(),
            expansion.0.display()
        ),
    );
    let mut xip = command(&mac.xip);
    xip.arg("--expand")
        .arg(&archive)
        .current_dir(&expansion.0)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    finish(
        xip,
        &format!("{} --expand {}", mac.xip.display(), archive.display()),
    )?;
    let apps: Vec<PathBuf> = std::fs::read_dir(&expansion.0)
        .map_err(|error| failed(&format!("cannot read {} ({error})", expansion.0.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"))
        .collect();
    let [app] = apps.as_slice() else {
        return Err(failed(&format!(
            "{} holds {} apps, not one Xcode",
            archive.display(),
            apps.len()
        )));
    };
    let contents = app.join("Contents");
    let version = plist_value(
        mac,
        &contents.join("Info.plist"),
        "CFBundleShortVersionString",
    )?;
    let build = plist_value(mac, &contents.join("version.plist"), "ProductBuildVersion")?;
    if !is_version(version.as_bytes()) || !is_build(build.as_bytes()) {
        return Err(failed(&format!(
            "{} says version {version:?} and build {build:?}, which are not an Xcode release's",
            app.display()
        )));
    }
    let codesign = sign_check(mac, app)?;
    drop(expansion);
    Ok(Release {
        version,
        build,
        codesign,
    })
}

/// `codesign --verify --deep --strict` on `app`: what it printed; the failure when it does not
/// exit 0 or cannot be run.
fn sign_check(mac: &Mac, app: &Path) -> Flow<Output> {
    let mut codesign = command(&mac.codesign);
    codesign
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .stdin(Stdio::null());
    let what = format!(
        "{} --verify --deep --strict {}",
        mac.codesign.display(),
        app.display()
    );
    let out = codesign
        .output()
        .map_err(|error| failed(&format!("cannot run {} ({error})", mac.codesign.display())))?;
    let done = Output {
        exit: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    if out.status.success() {
        return Ok(done);
    }
    let said = done.stderr.lines().next().unwrap_or_default();
    Err(failed(&format!("{what} failed ({}): {said}", out.status)))
}
