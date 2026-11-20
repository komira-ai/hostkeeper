//! Apple's signature on an archive, as `pkgutil --check-signature <archive>` reports it: the push
//! goes on only when pkgutil exits 0, the status is `signed Apple Software` (signed by Apple
//! itself, not by a developer whose certificate Apple issued), and the certificate chain it
//! prints, leaf first, ends at Apple Root CA, matched by name and by its SHA-256 fingerprint.
//!
//! The report reads, in part:
//!
//! ```text
//!    Status: signed Apple Software
//!    Certificate Chain:
//!     1. Software Signing
//!        ...
//!     3. Apple Root CA
//!        Expires: ...
//!        SHA256 Fingerprint:
//!            B0 B1 73 0E CB C7 FF 45 05 14 2C 49 F1 29 5E 6E DA 6B CA ED 7E 2C 68 C5
//!            BE 91 B5 A1 10 01 F0 24
//! ```

use std::path::Path;
use std::process::Stdio;

use super::{Mac, command, failed, finish, say};
use crate::{Flow, World};

/// The status pkgutil gives an archive Apple itself signed.
pub(crate) const SIGNED_BY_APPLE: &str = "signed Apple Software";

/// The name of Apple's root certificate authority, where Apple's own chain ends.
pub(crate) const APPLE_ROOT_CA: &str = "Apple Root CA";

/// The SHA-256 fingerprint of Apple Root CA, in upper-case hex: Apple publishes it with the
/// certificate at <https://www.apple.com/certificateauthority/>.
pub(crate) const APPLE_ROOT_CA_SHA256: &str =
    "B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024";

/// One certificate of the chain: its name and its SHA-256 fingerprint, upper-case hex with no
/// spaces (empty when the report gives none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) name: String,
    pub(crate) sha256: String,
}

/// What the report says: the status, and the chain, leaf first.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) status: Option<String>,
    pub(crate) chain: Vec<Link>,
}

/// The name of the chain's entry that `line` starts, `<n>. <name>`.
fn entry(line: &str) -> Option<String> {
    let (number, name) = line.split_once(". ")?;
    let numbered = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
    numbered.then(|| name.trim().to_owned())
}

/// Whether `line` is a row of a fingerprint: hex digits and spaces.
fn is_hex_row(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| b.is_ascii_hexdigit() || b == b' ')
}

/// The status and the chain of a report of `pkgutil --check-signature`.
pub(crate) fn parse(text: &str) -> Report {
    let mut report = Report::default();
    let mut in_fingerprint = false;
    for line in text.lines().map(str::trim) {
        if in_fingerprint && is_hex_row(line) {
            if let Some(link) = report.chain.last_mut() {
                link.sha256.extend(
                    line.chars()
                        .filter(|c| *c != ' ')
                        .map(|c| c.to_ascii_uppercase()),
                );
            }
            continue;
        }
        in_fingerprint = line == "SHA256 Fingerprint:";
        if let Some(status) = line.strip_prefix("Status:") {
            report.status = Some(status.trim().to_owned());
        } else if let Some(name) = entry(line) {
            report.chain.push(Link {
                name,
                sha256: String::new(),
            });
        }
    }
    report
}

/// Whether `report` is Apple's own signature, ending at Apple Root CA; why not, when it is not.
pub(crate) fn verdict(report: &Report) -> Result<(), String> {
    match report.status.as_deref() {
        Some(SIGNED_BY_APPLE) => {}
        Some(other) => {
            return Err(format!(
                "its status is \"{other}\", not \"{SIGNED_BY_APPLE}\""
            ));
        }
        None => return Err("it gives no status".to_owned()),
    }
    let Some(root) = report.chain.last() else {
        return Err("it gives no certificate chain".to_owned());
    };
    if root.name != APPLE_ROOT_CA || root.sha256 != APPLE_ROOT_CA_SHA256 {
        return Err(format!(
            "its chain ends at \"{}\" with the SHA-256 fingerprint \"{}\", not at \
             {APPLE_ROOT_CA} ({APPLE_ROOT_CA_SHA256})",
            root.name, root.sha256
        ));
    }
    Ok(())
}

/// Checks Apple's signature on the archive at `path` with `pkgutil --check-signature`; the
/// failure that ends the push when it is not Apple's own.
pub(crate) fn check(w: &mut World<'_>, mac: &Mac, path: &Path) -> Flow<()> {
    let mut pkgutil = command(&mac.pkgutil);
    pkgutil
        .arg("--check-signature")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    let what = format!(
        "{} --check-signature {}",
        mac.pkgutil.display(),
        path.display()
    );
    let out = finish(pkgutil, &what)?;
    verdict(&parse(&String::from_utf8_lossy(&out))).map_err(|why| {
        failed(&format!(
            "{what} does not report Apple's own signature: {why}"
        ))
    })?;
    say(
        w,
        &format!(
            "{}: {SIGNED_BY_APPLE}, its chain ending at {APPLE_ROOT_CA}",
            path.display()
        ),
    );
    Ok(())
}
