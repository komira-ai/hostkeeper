//! The output file of a verb's `key=value` answers, and the SHA-256 of a file.

use std::io::{Read, Write};
use std::path::Path;

use super::Error;

/// Appends `lines` (`key=value`) to `output`, when there is one, and returns them to print.
pub(crate) fn answer(output: Option<&Path>, lines: Vec<String>) -> Result<Vec<String>, Error> {
    if let Some(path) = output {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(Error::io(format!("append to {}", path.display())))?;
    }
    Ok(lines)
}

/// The SHA-256 of the file at `path`, 64 lowercase hex digits, read in one streaming pass.
pub(crate) fn file_sha256(path: &Path) -> Result<String, Error> {
    use sha2::Digest;
    let what = format!("read {}", path.display());
    let mut file = std::fs::File::open(path).map_err(Error::io(&what))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(Error::io(&what))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
