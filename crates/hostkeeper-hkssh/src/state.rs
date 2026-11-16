//! The operator's state directory (`--state-dir`, mode 0700): the key `hkssh` made, and per host
//! the certificate held, what is known of it, and the pinned host-key fingerprints. Nothing in
//! it is a token.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use crate::error::Error;

/// The longest host id taken; ids are far shorter.
const MAX_HOST_ID: usize = 64;

/// Whether `id` is a host id that is safe as a file name and as an argument of a program: 1 to
/// 64 of ASCII letters, digits, `.`, `_` and `-`, not starting with `.` or `-`.
pub fn check_host_id(id: &str) -> Result<(), Error> {
    let plain = !id.is_empty()
        && id.len() <= MAX_HOST_ID
        && !id.starts_with(['.', '-'])
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if plain {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "{id:?} is not a host id: 1 to {MAX_HOST_ID} letters, digits, '.', '_' or '-'"
        )))
    }
}

/// What is kept of one certificate: enough to reuse it while it is valid and to revoke it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    pub serial: u64,
    /// The last second it is valid.
    pub expires: u64,
    pub login_user: String,
    pub key_id: String,
    /// The public key it was made for: `<type> <base64>`.
    pub public_key: String,
}

impl CertInfo {
    fn encode(&self) -> String {
        format!(
            "serial={}\nexpires={}\nlogin_user={}\nkey_id={}\npublic_key={}\n",
            self.serial, self.expires, self.login_user, self.key_id, self.public_key
        )
    }

    /// The record of `text`, or `None` when it is not one `encode` wrote.
    fn decode(text: &str) -> Option<CertInfo> {
        let field = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
        };
        Some(CertInfo {
            serial: field("serial")?.parse().ok()?,
            expires: field("expires")?.parse().ok()?,
            login_user: field("login_user")?.to_owned(),
            key_id: field("key_id")?.to_owned(),
            public_key: field("public_key")?.to_owned(),
        })
    }
}

/// The state directory.
#[derive(Debug, Clone)]
pub struct State {
    dir: PathBuf,
}

impl State {
    pub fn new(dir: PathBuf) -> State {
        State { dir }
    }

    /// The private key `hkssh` makes when the operator gives none and the agent holds none.
    pub fn key(&self) -> PathBuf {
        self.dir.join("id_ed25519")
    }

    /// The public key of the agent's key, which `ssh -i` takes to pick that key from the agent.
    pub fn agent_public(&self) -> PathBuf {
        self.dir.join("agent.pub")
    }

    /// The certificate of `host`, in the file `ssh` reads as `CertificateFile`.
    pub fn cert_file(&self, host: &str) -> PathBuf {
        self.dir.join("hosts").join(format!("{host}-cert.pub"))
    }

    fn info_file(&self, host: &str) -> PathBuf {
        self.dir.join("hosts").join(format!("{host}.details"))
    }

    /// The pinned fingerprints of `host`, one per line.
    pub fn pins_file(&self, host: &str) -> PathBuf {
        self.dir.join("hosts").join(format!("{host}.pins"))
    }

    /// Creates the directory and its `hosts` directory with mode 0700 when they are missing.
    pub fn ensure(&self) -> Result<(), Error> {
        for dir in [self.dir.clone(), self.dir.join("hosts")] {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .map_err(|source| Error::Write { path: dir, source })?;
        }
        Ok(())
    }

    /// Writes `text` to `path` with mode 0600, whole or not at all (a temporary file renamed).
    pub fn write(&self, path: &Path, text: &str) -> Result<(), Error> {
        let fail = |source| Error::Write {
            path: path.to_owned(),
            source,
        };
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(fail)?;
        file.write_all(text.as_bytes()).map_err(fail)?;
        fs::rename(&temporary, path).map_err(fail)
    }

    /// The certificate record of `host`, `None` when there is none or it is not readable as one.
    pub fn cert_info(&self, host: &str) -> Option<CertInfo> {
        CertInfo::decode(&fs::read_to_string(self.info_file(host)).ok()?)
    }

    /// Keeps `cert` (the certificate text) and `info` for `host`.
    pub fn store_cert(&self, host: &str, cert: &str, info: &CertInfo) -> Result<(), Error> {
        self.ensure()?;
        self.write(&self.cert_file(host), &format!("{cert}\n"))?;
        self.write(&self.info_file(host), &info.encode())
    }

    /// Keeps `fingerprints` as the pins of `host`.
    pub fn store_pins(&self, host: &str, fingerprints: &[String]) -> Result<(), Error> {
        self.ensure()?;
        let mut text = fingerprints.join("\n");
        text.push('\n');
        self.write(&self.pins_file(host), &text)
    }
}
