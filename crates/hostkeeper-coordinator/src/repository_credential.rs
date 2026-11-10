//! The credential file of a private repository (docs/design/hostgroups.md, section 6.6): the one
//! line the coordinator sends its origin as the `Authorization` header (`Bearer <token>`,
//! `Basic <base64>`), so any scheme an origin takes works.
//!
//! [`read`] judges the file each time it is called, and `Repositories.Put` and every read of the
//! origin call it, so a rotated file needs no restart. The file is a regular file the
//! coordinator's own user owns, with mode `0600` or `0400`, not followed if it is a link and not
//! opened if it is a FIFO (`hostkeeper_wire::secret`, the rule the operator token file is held
//! to); it is 1 to [`MAX_BYTES`] bytes: one line of printable ASCII, with a final newline
//! allowed. An error names the rule it breaks, never the path or a byte of the content.

use std::path::Path;

use hostkeeper_wire::secret::{SecretError, read_secret};

/// The most bytes the file may hold.
pub(crate) const MAX_BYTES: u64 = 4096;

/// What names the file in an error: not its path.
const FLAG: &str = "credential_file";

/// Why a credential file is unusable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CredentialError {
    /// The file cannot be opened or read, or is not a regular file of the coordinator's user
    /// with mode `0600` or `0400`.
    #[error("{0}")]
    File(#[from] SecretError),
    /// The file is empty, longer than [`MAX_BYTES`], or not one line of printable ASCII.
    #[error("the credential file must hold one line of 1 to 4096 printable ASCII characters")]
    Line,
}

/// The line of the credential file at `path`, without its final newline.
///
/// # Errors
/// [`CredentialError`].
pub(crate) fn read(path: &str) -> Result<String, CredentialError> {
    let bytes = read_secret(Path::new(path), FLAG, MAX_BYTES)?;
    let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let fits = !line.is_empty()
        && bytes.len() as u64 <= MAX_BYTES
        && line.iter().all(|b| (0x20..0x7f).contains(b));
    if !fits {
        return Err(CredentialError::Line);
    }
    Ok(String::from_utf8_lossy(line).into_owned())
}
