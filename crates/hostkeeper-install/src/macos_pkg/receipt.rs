//! What macOS records of an installed package (packages-repositories.md section 4.7): its
//! receipt, as `pkgutil --pkg-info-plist <identifier>` prints it and as
//! `/var/db/receipts/<identifier>.plist` holds it (read with no tool run, by the observer); the
//! receipts that list a file (`pkgutil --file-info-plist <path>`); and the files a receipt lists
//! (`pkgutil --only-files --files <identifier>`, relative to its install location).

use plist::Value;

/// What `pkgutil` prints, exiting 1, for an identifier macOS has no receipt of: `No receipt for
/// 'x' found at '/'.`
pub const NO_RECEIPT: &str = "No receipt for";

/// A package's receipt: its identifier, its version, and the logical directory its files are
/// relative to (the volume joined with the install location).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub identifier: String,
    pub version: String,
    pub prefix: String,
}

impl Receipt {
    /// The logical absolute path of a file the receipt lists relative to its prefix.
    pub fn path_of(&self, listed: &str) -> String {
        join(&self.prefix, listed)
    }
}

/// Why an answer of `pkgutil` or a receipt file was not read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptError {
    #[error("is no property list: {0}")]
    Plist(String),
    #[error("has no {0}")]
    Missing(&'static str),
    #[error("is longer than a tool's output may be, so it may be cut")]
    Cut,
}

/// The `pkgutil` arguments of a receipt.
pub fn pkg_info_args(identifier: &str) -> Vec<String> {
    vec!["--pkg-info-plist".into(), identifier.into()]
}

/// The `pkgutil` arguments of the receipts that list a file.
pub fn file_info_args(path: &str) -> Vec<String> {
    vec!["--file-info-plist".into(), path.into()]
}

/// The `pkgutil` arguments of the files a receipt lists.
pub fn files_args(identifier: &str) -> Vec<String> {
    vec!["--only-files".into(), "--files".into(), identifier.into()]
}

/// The `pkgutil` arguments that forget a receipt.
pub fn forget_args(identifier: &str) -> Vec<String> {
    vec!["--forget".into(), identifier.into()]
}

/// The receipt of `pkgutil --pkg-info-plist`'s output (`pkgid`, `pkg-version`, `volume`,
/// `install-location`).
pub fn parse_pkg_info(stdout: &[u8]) -> Result<Receipt, ReceiptError> {
    let dict = dictionary(stdout)?;
    let text = |key: &'static str| {
        dict.get(key)
            .and_then(Value::as_string)
            .ok_or(ReceiptError::Missing(key))
    };
    let volume = dict.get("volume").and_then(Value::as_string).unwrap_or("/");
    Ok(Receipt {
        identifier: text("pkgid")?.to_owned(),
        version: text("pkg-version")?.to_owned(),
        prefix: join(volume, text("install-location")?),
    })
}

/// The receipt file `/var/db/receipts/<identifier>.plist`'s reading (`PackageIdentifier`,
/// `PackageVersion`, `InstallPrefixPath`), binary or XML.
pub fn parse_receipt_file(bytes: &[u8]) -> Result<Receipt, ReceiptError> {
    let dict = dictionary(bytes)?;
    let text = |key: &'static str| {
        dict.get(key)
            .and_then(Value::as_string)
            .ok_or(ReceiptError::Missing(key))
    };
    Ok(Receipt {
        identifier: text("PackageIdentifier")?.to_owned(),
        version: text("PackageVersion")?.to_owned(),
        prefix: join(
            "/",
            dict.get("InstallPrefixPath")
                .and_then(Value::as_string)
                .unwrap_or(""),
        ),
    })
}

/// The logical path of a package's receipt file.
pub fn receipt_file(identifier: &str) -> String {
    format!("/var/db/receipts/{identifier}.plist")
}

/// The identifiers of the receipts `pkgutil --file-info-plist` says list the file: the
/// `pkgid` of each entry of its `path-info`. No `path-info`: no receipt lists it.
pub fn parse_file_info(stdout: &[u8]) -> Result<Vec<String>, ReceiptError> {
    let dict = dictionary(stdout)?;
    let Some(infos) = dict.get("path-info") else {
        return Ok(Vec::new());
    };
    let infos = infos
        .as_array()
        .ok_or(ReceiptError::Missing("path-info array"))?;
    infos
        .iter()
        .map(|info| {
            info.as_dictionary()
                .and_then(|d| d.get("pkgid"))
                .and_then(Value::as_string)
                .map(str::to_owned)
                .ok_or(ReceiptError::Missing("pkgid"))
        })
        .collect()
}

/// The files of `pkgutil --only-files --files`' output, one a line, relative to the prefix.
pub fn parse_files(stdout: &[u8], cut: bool) -> Result<Vec<String>, ReceiptError> {
    if cut {
        return Err(ReceiptError::Cut);
    }
    Ok(String::from_utf8_lossy(stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn dictionary(bytes: &[u8]) -> Result<plist::Dictionary, ReceiptError> {
    let value = Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| ReceiptError::Plist(e.to_string()))?;
    value
        .into_dictionary()
        .ok_or_else(|| ReceiptError::Plist("not a dictionary".into()))
}

/// `rest` below the logical directory `base`, as one logical absolute path.
pub fn join(base: &str, rest: &str) -> String {
    let parts: Vec<&str> = base
        .split('/')
        .chain(rest.split('/'))
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    format!("/{}", parts.join("/"))
}

#[cfg(test)]
#[path = "receipt_tests.rs"]
mod tests;
