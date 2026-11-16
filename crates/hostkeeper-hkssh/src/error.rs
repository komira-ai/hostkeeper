//! What a command of `hkssh` ends with when it fails, and the exit code of each.

use std::path::PathBuf;

/// A failure of a command. None of these holds the token or a private key.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flag or an argument that breaks its rule; exit 2.
    #[error("{0}")]
    Usage(String),
    /// A file that could not be read; what it was is named, its content never.
    #[error("cannot read {what} from {}: {source}", path.display())]
    Read {
        what: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    /// A file or directory that could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The coordinator's refusal: the token of `SshRefusal` its status message is.
    #[error("refused: {0}")]
    Refused(String),
    /// No usable answer: the channel or the status carried no refusal token.
    #[error("no answer from the coordinator: {0}")]
    Transport(String),
    /// A program (`ssh`, `ssh-keygen`, `ssh-add`) that did not run or did not do its part.
    #[error("{program}: {reason}")]
    Program {
        program: String,
        reason: String,
    },
    /// The coordinator's answer cannot be used (a host with no pin, a certificate that is not
    /// one).
    #[error("{0}")]
    Unusable(String),
}

impl Error {
    /// The exit code: 2 for a usage error, 1 for anything else.
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Usage(_) => 2,
            _ => 1,
        }
    }

    /// The refusal token, when this is the coordinator's refusal.
    pub fn refusal(&self) -> Option<&str> {
        match self {
            Error::Refused(token) => Some(token),
            _ => None,
        }
    }
}
