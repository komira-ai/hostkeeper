//! Apple's signature on an Xcode archive, as `pkgutil --check-signature <archive>` reports it:
//! the status must be `signed Apple Software` (signed by Apple itself, not by a developer whose
//! certificate Apple issued), and the certificate chain the report prints, leaf first, must end at
//! Apple Root CA, matched by name and by its SHA-256 fingerprint.
//!
//! This is the same rule as the `hk xcode-push` check, kept here because the daemon does not
//! depend on `hk`: an archive uploaded from a Linux box, where `pkgutil` does not exist, was not
//! checked there, so the Mac that installs it checks it. The functions are pure: the stage runs
//! `pkgutil` and gives its report.
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

/// The status pkgutil gives an archive Apple itself signed.
pub(super) const SIGNED_BY_APPLE: &str = "signed Apple Software";

/// The name of Apple's root certificate authority, where Apple's own chain ends.
pub(super) const APPLE_ROOT_CA: &str = "Apple Root CA";

/// The SHA-256 fingerprint of Apple Root CA, in upper-case hex: Apple publishes it with the
/// certificate at <https://www.apple.com/certificateauthority/>.
pub(super) const APPLE_ROOT_CA_SHA256: &str =
    "B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024";

/// One certificate of the chain: its name and its SHA-256 fingerprint, upper-case hex with no
/// spaces (empty when the report gives none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Link {
    pub(super) name: String,
    pub(super) sha256: String,
}

/// What the report says: the status, and the chain, leaf first.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Report {
    pub(super) status: Option<String>,
    pub(super) chain: Vec<Link>,
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
pub(super) fn parse(text: &str) -> Report {
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
pub(super) fn verdict(report: &Report) -> Result<(), String> {
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

#[cfg(test)]
#[path = "signature_tests.rs"]
pub(super) mod tests;
