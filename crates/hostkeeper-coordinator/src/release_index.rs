//! The release index a set of the desired state may carry, checked once when it is set.
//!
//! A set names a version's release in one of two ways: one package digest (`release_digest`, the
//! package every host is ordered whatever its platform), or the release index of the version
//! (`release_index`, the text of its `index.json`, byte for byte) with the SHA-256 of that text
//! (`release_index_digest`). [`checked`] judges the second, in this order, and refuses the first
//! that fails:
//!
//! 1. one of the two keys without the other, either not text, or the package digest as well
//!    (`index_invalid`);
//! 2. a digest that is not 64 lowercase hex characters (`index_invalid`);
//! 3. a text whose SHA-256 is not the digest (`index_digest_mismatch`): the index was changed or
//!    cut on its way;
//! 4. a text that the release index parser (`hostkeeper_config::index`) does not read as the
//!    index of this service at this version (`index_invalid`).
//!
//! What the coordinator keeps of an index that passed is its record ([`IndexRecord`]): the
//! digest of the index and the digest of the package it lists for each platform. The text is
//! dropped; each host fetches the index itself and checks its SHA-256 against the one its order
//! names, so the coordinator never serves or trusts the bytes.

use hostkeeper_config::index::ReleaseIndex;
use hostkeeper_config::names::{ServiceName, Version as IndexVersion};
use hostkeeper_wire::version::Version;
use serde_json::Value;

use crate::desired::DesiredError;
use crate::ops_file;
use crate::registry::{Digest, IndexKey, IndexRecord};

/// The refusal `index_invalid`, saying `why`.
const fn invalid(why: &'static str) -> DesiredError {
    DesiredError::IndexInvalid(why)
}

/// The record of the index a set of `service` at `version` named by `text` and `digest`, if it
/// named one: none when it named neither. `has_package_digest` says that the same set names a
/// package digest, which an index replaces.
///
/// # Errors
/// [`DesiredError::IndexInvalid`] and [`DesiredError::IndexDigestMismatch`], as the module
/// documentation says.
pub(crate) fn checked(
    text: Option<&Value>,
    digest: Option<&Value>,
    has_package_digest: bool,
    service: &str,
    version: &Version,
) -> Result<Option<IndexRecord>, DesiredError> {
    let (text, digest) = match (text, digest) {
        (None, None) => return Ok(None),
        (Some(text), Some(digest)) => (text, digest),
        _ => {
            return Err(invalid(
                "release_index and release_index_digest come together",
            ));
        }
    };
    if has_package_digest {
        return Err(invalid("release_index is not set with a package digest"));
    }
    let (Some(text), Some(digest)) = (text.as_str(), digest.as_str()) else {
        return Err(invalid("release_index and its digest are text"));
    };
    let digest = Digest::parse(digest)
        .map_err(|_| invalid("release_index_digest is not 64 lowercase hex characters"))?;
    if ops_file::digest(text.as_bytes()) != digest.as_str() {
        return Err(DesiredError::IndexDigestMismatch);
    }
    let index = ServiceName::new(service)
        .ok()
        .zip(IndexVersion::new(version.as_str()).ok())
        .and_then(|(service, version)| ReleaseIndex::parse(text, &service, &version).ok())
        .ok_or(invalid(
            "release_index is not the release index of the service at the version",
        ))?;
    Ok(Some(IndexRecord {
        digest,
        packages: index
            .packages()
            .iter()
            .filter_map(|entry| {
                Digest::parse(entry.sha256.as_str())
                    .ok()
                    .map(|sha| (IndexKey::of(entry.platform), sha))
            })
            .collect(),
    }))
}
