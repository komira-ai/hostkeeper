//! The archive of `hk xcode-push`: where it comes from (`xcodes download`, or the file given), its
//! SHA-256 and size, and the version and build of the app in it.

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest as _, Sha256};

use super::{Mac, command, failed, finish, say};
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

/// The archive of `version`, downloaded by `xcodes` into `$HOME/Downloads/hk-xcode-push/<version>`
/// in the operator's own session: its standard streams are this process's, so that a two-factor
/// prompt is answered at the terminal. The directory must then hold exactly one `.xip`.
pub(crate) fn download(w: &mut World<'_>, mac: &Mac, version: &str) -> Flow<PathBuf> {
    let home = match &w.home {
        Some(home) if !home.as_os_str().is_empty() => home.clone(),
        _ => {
            return Err(failed(
                "hk xcode-push downloads into $HOME/Downloads, and $HOME is not set",
            ));
        }
    };
    let dir = home.join("Downloads").join("hk-xcode-push").join(version);
    std::fs::create_dir_all(&dir)
        .map_err(|error| failed(&format!("cannot make {} ({error})", dir.display())))?;
    say(
        w,
        &format!(
            "downloading Xcode {version} with {} into {}",
            mac.xcodes.display(),
            dir.display()
        ),
    );
    let mut xcodes = command(&mac.xcodes);
    xcodes
        .arg("download")
        .arg(version)
        .arg("--directory")
        .arg(&dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    finish(
        xcodes,
        &format!("{} download {version}", mac.xcodes.display()),
    )?;
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|error| failed(&format!("cannot read {} ({error})", dir.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "xip"))
        .collect();
    found.sort();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(failed(&format!(
            "{} download {version} left no .xip in {}",
            mac.xcodes.display(),
            dir.display()
        ))),
        _ => Err(failed(&format!(
            "{} holds more than one .xip; name the one to push, or leave one",
            dir.display()
        ))),
    }
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
/// start to its end once, in pieces, never whole. The error says why it could not be read.
pub(crate) fn digest_of(file: &mut File) -> Result<(String, u64), String> {
    let mut sha = Sha256::new();
    let mut size: u64 = 0;
    let mut piece = vec![0_u8; 1 << 20];
    loop {
        let n = file
            .read(&mut piece)
            .map_err(|error| format!("cannot read the archive ({error})"))?;
        if n == 0 {
            break;
        }
        sha.update(piece.get(..n).unwrap_or_default());
        size = size.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
    }
    Ok((hex(&sha.finalize()), size))
}

/// [`digest_of`] as the failure that ends a push.
pub(crate) fn digest(file: &mut File) -> Flow<(String, u64)> {
    digest_of(file).map_err(|why| failed(&why))
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
/// a push stopped by a signal left there is removed by the next.
pub(crate) const EXPANSION: &str = ".hk-xcode-push-expansion";

/// The version and the build of the Xcode in the archive at `path`: `xip --expand` makes the app
/// in [`EXPANSION`] beside the archive (on its volume, so that no other disk fills), removed
/// first if it is there, and the version is the app's `CFBundleShortVersionString`
/// (`Contents/Info.plist`) and the build its `ProductBuildVersion` (`Contents/version.plist`).
/// The directory is removed afterwards.
pub(crate) fn release(w: &mut World<'_>, mac: &Mac, path: &Path) -> Flow<(String, String)> {
    let archive = std::path::absolute(path)
        .map_err(|error| failed(&format!("cannot resolve {} ({error})", path.display())))?;
    let beside = archive.parent().unwrap_or(Path::new("/"));
    let dir = beside.join(EXPANSION);
    // What an earlier push left: it is not there, or it is removed, or making it fails below.
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
    drop(expansion);
    Ok((version, build))
}
