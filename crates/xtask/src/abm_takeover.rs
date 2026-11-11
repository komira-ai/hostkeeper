//! `abm-takeover-token --dep-dir <dir> --name <server> --out <file>`: the one-off reader of the
//! ABM takeover (`docs/design/coordinator-mdm.md`, sections 4.5 and 7), run by the operator on the
//! machine that runs the old MDM stack.
//!
//! NanoDEP does not keep ABM's `.p7m`; it keeps the token decrypted, one file per part, in its
//! `dep_names` directory: `consumer_key.<server>`, `consumer_secret.<server>`,
//! `access_token.<server>`, `access_secret.<server>` and `access_token_expiry.<server>`
//! (RFC 3339). This verb reads those five files and writes the JSON object that
//! `Mdm.PutAbmToken` takes (`consumer_key`, `consumer_secret`, `access_token`, `access_secret`
//! and `access_secret_expiry`) to `--out`, which it creates with mode 0600 and never over a file
//! that is there. A part is the file's text without the whitespace around it; a file that is
//! missing, unreadable or empty is refused, naming the file and never its content.
//!
//! The verb prints one line that names the output file and nothing of what it holds, and changes
//! nothing else: the files it reads are only read, and nothing is sent to anyone. The operator
//! uploads the output in the setup page (or with the signed-in `Mdm.PutAbmToken`) and then
//! removes it. The key and the certificate (`key.<server>`, `cert.<server>`) are uploaded as
//! they are.

#[cfg(test)]
mod tests;

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// The flags of `abm-takeover-token`.
#[derive(Debug, clap::Args)]
pub(crate) struct Flags {
    /// NanoDEP's `dep_names` directory (`/var/lib/hostkeeper-mdm/dep/dep_names`).
    #[arg(long)]
    dep_dir: PathBuf,
    /// The MDM server entry's name, the suffix of each file (`js-mdm`): 1 to 64 letters, digits,
    /// `-` and `_`.
    #[arg(long)]
    name: String,
    /// The file to write, mode 0600, which must not exist.
    #[arg(long)]
    out: PathBuf,
}

/// Why the verb refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// `--name` is not a name the files can carry.
    #[error("--name {0:?} is not 1 to 64 letters, digits, - and _")]
    Name(String),
    /// A part could not be read, or is empty.
    #[error("{path}: {why}")]
    Part {
        /// The file.
        path: String,
        /// What is wrong with it.
        why: String,
    },
    /// The output could not be written.
    #[error("{path}: {why}")]
    Out {
        /// The file.
        path: String,
        /// What went wrong.
        why: String,
    },
}

/// The text of `<dir>/<part>.<name>`, trimmed, which must not be empty.
fn part_of(dir: &Path, part: &str, name: &str) -> Result<String, Error> {
    let path = dir.join(format!("{part}.{name}"));
    let refused = |why: String| Error::Part {
        path: path.display().to_string(),
        why,
    };
    let text = std::fs::read_to_string(&path).map_err(|e| refused(e.kind().to_string()))?;
    let text = text.trim();
    if text.is_empty() {
        return Err(refused("is empty".to_owned()));
    }
    Ok(text.to_owned())
}

/// Runs the verb.
///
/// # Errors
/// [`Error`].
pub(crate) fn run(flags: &Flags) -> Result<Vec<String>, Error> {
    let name = &flags.name;
    let fits = (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !fits {
        return Err(Error::Name(name.clone()));
    }
    let dir = &flags.dep_dir;
    let part = |part: &str| part_of(dir, part, name);
    let token = serde_json::json!({
        "consumer_key": part("consumer_key")?,
        "consumer_secret": part("consumer_secret")?,
        "access_token": part("access_token")?,
        "access_secret": part("access_secret")?,
        "access_secret_expiry": part("access_token_expiry")?,
    });
    write_private(&flags.out, token.to_string().as_bytes()).map_err(|e| Error::Out {
        path: flags.out.display().to_string(),
        why: e.to_string(),
    })?;
    Ok(vec![format!(
        "wrote {} (the five parts of the ABM token, mode 0600): upload it in the setup page, then remove it",
        flags.out.display()
    )])
}

/// Writes `bytes` to a new file `path` that only its owner reads, and flushes it to disk.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
