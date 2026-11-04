//! The seal of a package version: `packages/<service>/<version>.seal`, written when the version
//! was unpacked and verified, read before the version is used (docs/package-format.md).
//!
//! ```json
//! {"schema":1,"tree_digest":"<64 hex>","package_digest":"<64 hex>"}
//! ```
//!
//! `tree_digest` is the [`crate::tree_digest`] of the version directory; `package_digest` is the
//! SHA-256 of the package archive it was unpacked from. The format changes additively only
//! (AGENTS.md B8): a reader ignores fields it does not know, and a change that an older reader
//! must not take for schema 1 gets a new `schema`, which that reader refuses
//! ([`SealError::Schema`]) rather than misreads.

/// The one schema this release writes and reads.
pub const SEAL_SCHEMA: u64 = 1;

/// Why a seal cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// Not JSON, not UTF-8, truncated, or a field missing or malformed. Says nothing about the
    /// tree: the caller tries again later.
    #[error("the seal cannot be read: {0}")]
    Unreadable(String),
    /// Written by a release that uses another schema; this one does not read it.
    #[error("the seal has schema {0}; this release reads schema {SEAL_SCHEMA} only")]
    Schema(u64),
    /// A digest given to [`Seal::new`] is not 64 lower-case hex digits.
    #[error("{0} is not a SHA-256 of 64 lower-case hex digits")]
    BadDigest(String),
}

/// What a seal says: the digest of the unpacked tree and of the archive it came from, each 64
/// lower-case hex digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seal {
    tree_digest: String,
    package_digest: String,
}

impl Seal {
    /// A seal of the two digests, each 64 lower-case hex digits ([`SealError::BadDigest`]
    /// otherwise).
    pub fn new(tree_digest: &str, package_digest: &str) -> Result<Seal, SealError> {
        for digest in [tree_digest, package_digest] {
            if !is_digest(digest) {
                return Err(SealError::BadDigest(digest.to_owned()));
            }
        }
        Ok(Seal {
            tree_digest: tree_digest.to_owned(),
            package_digest: package_digest.to_owned(),
        })
    }

    /// The digest of the version directory.
    pub fn tree_digest(&self) -> &str {
        &self.tree_digest
    }

    /// The SHA-256 of the package archive.
    pub fn package_digest(&self) -> &str {
        &self.package_digest
    }

    /// The bytes of the seal file, exactly
    /// `{"schema":1,"tree_digest":"<hex>","package_digest":"<hex>"}`.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":{SEAL_SCHEMA},\"tree_digest\":\"{}\",\"package_digest\":\"{}\"}}",
            self.tree_digest, self.package_digest
        )
    }

    /// Reads a seal file. Fields this release does not know are ignored; `schema` is read first
    /// and any value but [`SEAL_SCHEMA`] is [`SealError::Schema`]; anything else wrong is
    /// [`SealError::Unreadable`].
    pub fn parse(bytes: &[u8]) -> Result<Seal, SealError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| SealError::Unreadable(format!("not a JSON object: {e}")))?;
        let unreadable = |why: &str| SealError::Unreadable(why.to_owned());
        let schema = value
            .get("schema")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| unreadable("no whole-number schema"))?;
        if schema != SEAL_SCHEMA {
            return Err(SealError::Schema(schema));
        }
        let digest = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|text| is_digest(text))
                .ok_or_else(|| unreadable(&format!("no {key} of 64 lower-case hex digits")))
        };
        Seal::new(digest("tree_digest")?, digest("package_digest")?)
    }
}

fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
