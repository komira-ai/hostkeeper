//! `release mdm-fetch --dir D`: the build-time fetch of the mdm package (docs/design/two-programs.md
//! section 6.2). The pinned archive of each tool the package carries
//! (`hostkeeper_mdm_core::package::TOOLS`) is downloaded over HTTPS into `D`, under the file
//! name of its pin's URL, and checked against the pin's SHA-256 (`hostkeeper-mdm-core`'s
//! `pins.txt`); `release package --service mdm --archives D` then checks each again and takes
//! the binaries out. Nothing downloaded is run.
//!
//! For each pin, in the order of the pins:
//!
//! - an archive already in `D` with the pin's SHA-256 is kept and not downloaded again;
//! - otherwise it is downloaded to `<archive>.download` beside it ([`Download`], `curl` for real:
//!   [`CurlCli`]), then renamed to `<archive>` when its SHA-256 is the pin's, or to
//!   `<archive>.rejected` when it is not, which fails the verb naming the file, the digest it has
//!   and the pin's. A download that fails fails the verb too, and leaves no `<archive>`.
//!
//! Every pin is tried; the verb fails at the end with one line per archive that failed, and
//! prints one line per archive otherwise.

use std::path::Path;

use hostkeeper_mdm_core::package::TOOLS;
use hostkeeper_mdm_core::pins::{Pins, PinsError};

use super::ports::{Download, Exec, PortError};
use super::{Error, file_sha256};

/// The program that downloads.
const CURL: &str = "curl";

/// What a download adds to the archive's name until it is checked.
const PART_SUFFIX: &str = ".download";

/// What a download whose SHA-256 is not the pin's is kept as, beside the archives.
pub(crate) const REJECTED_SUFFIX: &str = ".rejected";

/// Downloads over HTTPS with `curl` through an [`Exec`]: no `.curlrc` (`-q`), HTTPS only, also
/// on a redirect (GitHub's release downloads redirect once to their storage), TLS 1.2 or later,
/// an HTTP error a failure, a transient failure (a 504, also when curl reports it as a receive
/// error) retried 5 times 5 seconds apart within 300 seconds, and the URL after `--`.
pub(crate) struct CurlCli<E> {
    exec: E,
}

impl<E> CurlCli<E> {
    pub(crate) fn new(exec: E) -> CurlCli<E> {
        CurlCli { exec }
    }
}

/// The arguments `curl` is run with to write `url` to `file`.
fn curl_args(url: &str, file: &str) -> Vec<String> {
    [
        "-q",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--fail",
        "--retry",
        "5",
        "--retry-delay",
        "5",
        "--retry-max-time",
        "300",
        "--retry-all-errors",
        "--silent",
        "--show-error",
        "--location",
        "--max-redirs",
        "5",
        "--output",
        file,
        "--",
        url,
    ]
    .map(str::to_owned)
    .to_vec()
}

impl<E: Exec> Download for CurlCli<E> {
    fn download(&self, url: &str, file: &Path) -> Result<(), PortError> {
        let what = format!("{CURL} {url}");
        if !url.starts_with("https://") {
            return Err(PortError::new(what, "not an https:// URL"));
        }
        let file = file
            .to_str()
            .ok_or_else(|| PortError::new(&what, "the output path is not UTF-8"))?;
        let ran = self
            .exec
            .spawn(CURL, &curl_args(url, file))
            .map_err(|e| PortError::new(&what, e.to_string()))?;
        match ran.success {
            true => Ok(()),
            false => Err(PortError::new(
                what,
                String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
            )),
        }
    }
}

/// The pins compiled into hostkeeper-mdm-core (its `pins.txt`), which this verb and the mdm
/// package build from.
pub(crate) fn built_in_pins() -> Result<Pins, Error> {
    Pins::built_in().map_err(pins_failed)
}

/// Why the built-in pins are not read (a test of hostkeeper-mdm-core holds that they are).
fn pins_failed(e: PinsError) -> Error {
    Error::Failed(format!("the pins of hostkeeper-mdm-core: {e}"))
}

/// The verb, with the pins compiled into hostkeeper-mdm-core.
pub(crate) fn fetch(download: &dyn Download, dir: &Path) -> Result<Vec<String>, Error> {
    fetch_with(download, &built_in_pins()?, dir)
}

/// The verb, with the pins `pins`.
pub(crate) fn fetch_with(
    download: &dyn Download,
    pins: &Pins,
    dir: &Path,
) -> Result<Vec<String>, Error> {
    std::fs::create_dir_all(dir).map_err(Error::io(format!("create {}", dir.display())))?;
    let mut lines = Vec::new();
    let mut problems = Vec::new();
    for tool in TOOLS {
        let pin = pins.get(tool);
        let want = pin.sha256_hex();
        let path = dir.join(pin.archive());
        if path.is_file() && file_sha256(&path)? == want {
            lines.push(format!("{}: kept, sha256 {want}", path.display()));
            continue;
        }
        let part = dir.join(format!("{}{PART_SUFFIX}", pin.archive()));
        if let Err(e) = download.download(&pin.url.to_string(), &part) {
            // Whatever a failed download left is never read: the next run downloads again.
            let _ = std::fs::remove_file(&part);
            problems.push(format!("{}: {e}", path.display()));
            continue;
        }
        let got = file_sha256(&part)?;
        if got == want {
            rename(&part, &path)?;
            lines.push(format!("{}: downloaded, sha256 {want}", path.display()));
        } else {
            let rejected = dir.join(format!("{}{REJECTED_SUFFIX}", pin.archive()));
            rename(&part, &rejected)?;
            problems.push(format!(
                "{}: its SHA-256 is {got}, the pin of {tool} is {want}; kept as {}",
                pin.url,
                rejected.display()
            ));
        }
    }
    match problems.is_empty() {
        true => Ok(lines),
        false => Err(Error::Refused(problems)),
    }
}

fn rename(from: &Path, to: &Path) -> Result<(), Error> {
    std::fs::rename(from, to).map_err(Error::io(format!(
        "rename {} to {}",
        from.display(),
        to.display()
    )))
}

#[cfg(test)]
mod tests;
