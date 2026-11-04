//! The previous schema of the deferral file: the reader of kbf's agent, kbf v0.0.64, the
//! release hostkeeper's first crates were imported from (public: github.com/komira-ai/kbf at the
//! tag `v0.0.64`). Copied from it, trimmed to what reading the file needs and with each item's
//! source named; nothing in it is hostkeeper's.
//!
//! - `Deferral`: `crates/kbf-updater/src/agent/hold.rs`, the struct and its derives.
//! - `Version`: `crates/kbf-updater/src/version.rs`, `Version::parse` and its `try_from`:
//!   1 to 128 bytes, a digit first, then letters, digits and `. + ~ -`.
//! - `Token`: `crates/kbf-updater/src/wire_report.rs`, `Token::parse` and its `try_from`: 1 to 64
//!   bytes of `[a-z0-9_]`.

use serde::{Deserialize, Serialize};

const MAX_LEN: usize = 128;
const MAX_TOKEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

impl Version {
    pub fn parse(s: &str) -> Result<Version, &'static str> {
        let first = s.bytes().next().ok_or("empty")?;
        if s.len() > MAX_LEN {
            return Err("too long");
        }
        if !first.is_ascii_digit() {
            return Err("first not a digit");
        }
        if !s.bytes().all(allowed) {
            return Err("character");
        }
        Ok(Version(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn allowed(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'~' | b'-')
}

impl TryFrom<String> for Version {
    type Error = &'static str;

    fn try_from(s: String) -> Result<Version, &'static str> {
        Version::parse(&s)
    }
}

impl From<Version> for String {
    fn from(v: Version) -> String {
        v.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

impl Token {
    pub fn parse(text: &str) -> Option<Token> {
        let word = (1..=MAX_TOKEN).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        word.then(|| Token(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Token {
    type Error = &'static str;

    fn try_from(s: String) -> Result<Token, &'static str> {
        Token::parse(&s).ok_or("not a token")
    }
}

impl From<Token> for String {
    fn from(t: Token) -> String {
        t.0
    }
}

/// An update the host declined, and until when (Unix seconds).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deferral {
    pub target: Version,
    pub reason: Token,
    pub retry_after_unix: u64,
}
