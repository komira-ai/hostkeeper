//! The operator's tokens: each has a name, and the tokens file holds the SHA-256 of each token,
//! never the token.
//!
//! `--tokens-file` is TOML, one `[[token]]` table for each token:
//!
//! ```toml
//! [[token]]
//! name = "ci-ops"     # 1 to 32 bytes of a-z, 0-9 and -; no two tokens share a name
//! sha256 = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"  # nor a hash
//! ```
//!
//! `sha256` is the SHA-256 of the token as 64 lowercase hex digits, so the file holds no secret.
//! No two tokens may have the same `sha256`: one secret is one name, so the audit names the same
//! operator whatever the order of the file, and removing a name revokes its secret.
//! Whoever can write the file can add a token, so it is read under the rule of a secret file: a
//! regular file owned by the coordinator's user with mode `0600` or `0400`. `coordinator` and
//! `ci` are not names a token may have: the audit uses them for the coordinator's own changes
//! and CI's.
//!
//! A bearer is hashed and compared with the hash of each token in constant time
//! ([`OperatorTokens::name_of`]); the name of the token that matches is who the operator is,
//! and it is what the audit records as `by` ([`crate::ops::AuditEntry`]).
//! `--operator-token-file` is one token named `operator` ([`OperatorTokens::single`]).
//!
//! The tokens file is read again on the tick that looks at the TLS files (`--tls-reload-secs`,
//! [`crate::listen::reload_every`]): a token removed from the file is refused from the next
//! look and one added is accepted, with no restart. A file that cannot be read again, or is
//! refused, leaves the tokens in use.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, PoisonError, RwLock};

use hostkeeper_wire::secret::{SecretError, read_secret};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::enrollment::{GATE_AUDITOR, MAC_APPROVER};
use crate::ops::label;
use crate::ops_file::digest;
use crate::reconcile::{CI, COORDINATOR};

/// The name `--operator-token-file` gives its one token.
pub const LEGACY_NAME: &str = "operator";

/// The flag that names the tokens file, which the texts of its errors name instead of its path.
pub const FLAG: &str = "--tokens-file";

/// The most bytes of a tokens file.
const MAX_FILE_BYTES: u64 = 1 << 16;

/// The name of an operator token: 1 to 32 bytes of `a-z`, `0-9` and `-`, other than the names
/// the audit uses for changes that are not an operator's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenName(String);

/// Why a token name is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenNameError {
    /// Not 1 to 32 bytes of `a-z`, `0-9` and `-`.
    #[error("a token name is 1 to 32 bytes of a-z, 0-9 and -")]
    Grammar,
    /// A name the audit uses for the coordinator's own changes, CI's, or the MDM gate's.
    #[error(
        "a token may not be named {0}: the audit uses that name for changes that are not an operator's"
    )]
    Reserved(&'static str),
}

impl FromStr for TokenName {
    type Err = TokenNameError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if !label(text) {
            return Err(TokenNameError::Grammar);
        }
        if let Some(reserved) = [COORDINATOR, CI, MAC_APPROVER, GATE_AUDITOR]
            .into_iter()
            .find(|name| *name == text)
        {
            return Err(TokenNameError::Reserved(reserved));
        }
        Ok(Self(text.to_owned()))
    }
}

impl TokenName {
    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why the tokens file is refused. The texts name the flag, never the path.
#[derive(Debug, thiserror::Error)]
pub enum TokensError {
    /// The file cannot be read, or breaks the rule of a secret file.
    #[error(transparent)]
    Read(#[from] SecretError),
    /// The file is longer than [`MAX_FILE_BYTES`].
    #[error("--tokens-file: it is over {MAX_FILE_BYTES} bytes")]
    TooLong,
    /// The file is not UTF-8.
    #[error("--tokens-file: it is not UTF-8")]
    NotText,
    /// The text is not TOML of the shape (the text of the parser).
    #[error("--tokens-file: {0}")]
    Parse(String),
    /// The name of the token `index` (counting from 1) is refused.
    #[error("--tokens-file: token {index}: {source}")]
    Name {
        /// Which `[[token]]`.
        index: usize,
        /// Why.
        #[source]
        source: TokenNameError,
    },
    /// The `sha256` of the token `name` is not 64 lowercase hex digits.
    #[error("--tokens-file: token {name}: sha256 is not 64 lowercase hex digits")]
    Digest {
        /// The token.
        name: String,
    },
    /// Two tokens have the same name.
    #[error("--tokens-file: the name {0} is used twice")]
    Repeated(String),
    /// Two tokens have the same `sha256`: one secret would answer to two names.
    #[error("--tokens-file: tokens {first} and {second} have the same sha256")]
    SharedDigest {
        /// The token that has it first in the file.
        first: String,
        /// The later token with the same hash.
        second: String,
    },
    /// The file names no token.
    #[error("--tokens-file: it names no token")]
    Empty,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    token: Vec<RawToken>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToken {
    name: String,
    sha256: String,
}

/// A token as the coordinator keeps it: its name and the hash of the token.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    name: TokenName,
    /// 64 lowercase hex digits.
    sha256: String,
}

/// The tokens `text` names.
fn parse(text: &str) -> Result<Vec<Token>, TokensError> {
    let file: RawFile = toml::from_str(text).map_err(|e| TokensError::Parse(e.to_string()))?;
    if file.token.is_empty() {
        return Err(TokensError::Empty);
    }
    let mut tokens: Vec<Token> = Vec::new();
    for (n, raw) in file.token.into_iter().enumerate() {
        let name: TokenName = raw.name.parse().map_err(|source| TokensError::Name {
            index: n + 1,
            source,
        })?;
        let hex = raw.sha256.len() == 64
            && raw
                .sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !hex {
            return Err(TokensError::Digest { name: name.0 });
        }
        if tokens.iter().any(|token| token.name == name) {
            return Err(TokensError::Repeated(name.0));
        }
        if let Some(earlier) = tokens.iter().find(|token| token.sha256 == raw.sha256) {
            return Err(TokensError::SharedDigest {
                first: earlier.name.0.clone(),
                second: name.0,
            });
        }
        tokens.push(Token {
            name,
            sha256: raw.sha256,
        });
    }
    Ok(tokens)
}

/// The tokens of the file at `path`, read under the rule of a secret file.
fn read(path: &Path) -> Result<Vec<Token>, TokensError> {
    let bytes = read_secret(path, FLAG, MAX_FILE_BYTES)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(TokensError::TooLong);
    }
    let text = String::from_utf8(bytes).map_err(|_| TokensError::NotText)?;
    parse(&text)
}

/// What one look at the tokens file did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reload {
    /// The file names other tokens than the ones in use: they are in use now.
    Applied,
    /// The file names the tokens in use, or there is no file to look at.
    Unchanged,
    /// The file could not be read or was refused: the tokens in use stay.
    Rejected,
}

struct Inner {
    tokens: RwLock<Vec<Token>>,
    /// The tokens file, if the tokens came from one.
    source: Option<PathBuf>,
}

/// The tokens an operator may call with, shared by the coordinator and the tick that re-reads
/// them.
#[derive(Clone)]
pub struct OperatorTokens {
    inner: Arc<Inner>,
}

impl OperatorTokens {
    fn of(tokens: Vec<Token>, source: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                tokens: RwLock::new(tokens),
                source,
            }),
        }
    }

    /// One token, named [`LEGACY_NAME`] (what `--operator-token-file` is). It is never re-read.
    #[must_use]
    pub fn single(token: &str) -> Self {
        let token = Token {
            name: TokenName(LEGACY_NAME.to_owned()),
            sha256: digest(token.as_bytes()),
        };
        Self::of(vec![token], None)
    }

    /// The tokens of the file at `path`, which [`Self::reload`] reads again.
    ///
    /// # Errors
    /// [`TokensError`]: the file cannot be read, breaks the rule of a secret file, is over
    /// 64 KiB, is not UTF-8 or TOML of the shape, names no token, repeats a name or a
    /// `sha256`, or has a name or a digest the rules above refuse.
    pub fn from_file(path: &Path) -> Result<Self, TokensError> {
        read(path).map(|tokens| Self::of(tokens, Some(path.to_owned())))
    }

    /// The name of the token `presented`, if it is one: its SHA-256 is compared with each
    /// token's in constant time.
    #[must_use]
    pub fn name_of(&self, presented: &str) -> Option<TokenName> {
        let hash = digest(presented.as_bytes());
        let tokens = self
            .inner
            .tokens
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        tokens
            .iter()
            .find(|token| bool::from(token.sha256.as_bytes().ct_eq(hash.as_bytes())))
            .map(|token| token.name.clone())
    }

    /// Reads the tokens file again. Tokens that changed are in use from now on (a token that is
    /// gone is refused, one that is new is accepted); a file that cannot be read or is refused
    /// leaves the tokens in use and is logged as a warning.
    pub fn reload(&self) -> Reload {
        let Some(path) = &self.inner.source else {
            return Reload::Unchanged;
        };
        match read(path) {
            Ok(tokens) => {
                let mut held = self
                    .inner
                    .tokens
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                if *held == tokens {
                    return Reload::Unchanged;
                }
                let count = tokens.len();
                *held = tokens;
                drop(held);
                tracing::info!(count, "the tokens file changed: its tokens are in use");
                Reload::Applied
            }
            Err(why) => {
                tracing::warn!(%why, "the tokens file was not read again: the tokens in use stay");
                Reload::Rejected
            }
        }
    }
}

impl From<String> for OperatorTokens {
    /// The one token named [`LEGACY_NAME`].
    fn from(token: String) -> Self {
        Self::single(&token)
    }
}
