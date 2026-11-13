//! The coordinator's read of a repository's descriptor and key (docs/deploy/package-repositories.md,
//! "Structure of a repository"), for `Repositories.Discover` and `Repositories.Add`: two GETs of
//! the repository's origin, `<base_uri>/hostkeeper-repository.json` and the keyring the descriptor
//! names, over HTTPS to the roots the coordinator is built with, by the client private repositories
//! are read with ([`crate::repository_origin`]): no redirect, a `Content-Length` and the
//! descriptor's bound before a byte of the body is read, and a deadline over both reads.
//!
//! Nothing here trusts the repository: the descriptor is judged by
//! [`hostkeeper_release::descriptor::check`], the keyring by [`registry::keys`], the fingerprints
//! the operator is shown are computed from the keyring's bytes, and the descriptor's own list is
//! only compared with them. What the read finds is the operator's to confirm (the trust a key has
//! is the operator's word, as it is when they choose a key file), and `Add` reads it again and
//! refuses a descriptor or a key that is not the one confirmed.
//!
//! A failure's text names its kind and never the URL or the credential: a private repository's
//! read carries one.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::RepositoriesRefusal as Refusal;
use hostkeeper_release::descriptor::{self, Descriptor};
use hostkeeper_release::registry::{self, MAX_KEYRING};

use crate::service::Coordinator;

/// The default of `--repository-discover-secs`.
pub const DISCOVER_SECS: u64 = 20;

/// The default of `--repository-descriptor-max-bytes`, and its ceiling: the descriptor's own.
pub const DESCRIPTOR_MAX_BYTES: usize = descriptor::MAX_BYTES;

/// The bounds of a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The time the two reads may take, together.
    pub(crate) timeout: Duration,
    /// The most bytes of a descriptor.
    pub(crate) descriptor_max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(DISCOVER_SECS),
            descriptor_max_bytes: DESCRIPTOR_MAX_BYTES,
        }
    }
}

/// What a read of a repository found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// The descriptor, as read.
    pub(crate) descriptor_bytes: Vec<u8>,
    /// The descriptor, parsed.
    pub(crate) descriptor: Descriptor,
    /// The keyring, as read.
    pub(crate) keyring: Vec<u8>,
    /// The fingerprints of the keyring's keys, computed: each primary key, then its subkeys.
    pub(crate) fingerprints: Vec<String>,
    /// The keyring's user ids.
    pub(crate) user_ids: Vec<String>,
    /// The SHA-256 of the descriptor, lower-case hex.
    pub(crate) descriptor_sha256: String,
    /// The SHA-256 of the keyring, lower-case hex.
    pub(crate) keyring_sha256: String,
}

impl Coordinator {
    /// The same coordinator reading a repository's descriptor within `timeout`, at most
    /// `descriptor_max_bytes` of it.
    #[must_use]
    pub fn with_discovery(mut self, timeout: Duration, descriptor_max_bytes: usize) -> Self {
        self.discovery = Limits {
            timeout,
            descriptor_max_bytes,
        };
        self
    }

    /// Reads the descriptor and the keyring of the repository at `base_uri`, with `authorization`
    /// as the header of that name when there is one.
    ///
    /// # Errors
    /// `invalid_request` (not a base URL), `discovery_failed` (an origin that cannot be read, or
    /// takes too long), `descriptor_invalid` (the descriptor, the keyring, or their disagreement).
    pub(crate) async fn read_repository(
        &self,
        base_uri: &str,
        authorization: Option<&str>,
    ) -> Result<Found, Refusal> {
        if !registry::is_base_uri(base_uri) {
            return Err(Refusal::InvalidRequest);
        }
        let Some(origin) = self.repository_origin.as_ref() else {
            tracing::warn!("discovery refused: the coordinator has no origin client");
            return Err(Refusal::DiscoveryFailed);
        };
        let limits = self.discovery;
        let read = async {
            let descriptor_bytes = origin
                .read_file(
                    base_uri,
                    descriptor::FILE,
                    authorization,
                    limits.descriptor_max_bytes,
                    limits.timeout,
                )
                .await
                .map_err(|why| {
                    tracing::warn!(%why, "discovery failed reading the descriptor");
                    Refusal::DiscoveryFailed
                })?;
            let parsed = descriptor::check(&descriptor_bytes).map_err(|why| {
                tracing::warn!(%why, "discovery refused the descriptor");
                Refusal::DescriptorInvalid
            })?;
            let keyring = origin
                .read_file(
                    base_uri,
                    &parsed.keyring,
                    authorization,
                    MAX_KEYRING,
                    limits.timeout,
                )
                .await
                .map_err(|why| {
                    tracing::warn!(%why, "discovery failed reading the keyring");
                    Refusal::DiscoveryFailed
                })?;
            Ok::<_, Refusal>((descriptor_bytes, parsed, keyring))
        };
        let (descriptor_bytes, parsed, keyring) = tokio::time::timeout(limits.timeout, read)
            .await
            .map_err(|_| {
                tracing::warn!("discovery failed: the reads took longer than allowed");
                Refusal::DiscoveryFailed
            })??;
        let invalid = |why: &dyn std::fmt::Display| {
            tracing::warn!(%why, "discovery refused the keyring");
            Refusal::DescriptorInvalid
        };
        let keys = registry::keys(&keyring).map_err(|why| invalid(&why))?;
        let user_ids = registry::user_ids(&keyring).map_err(|why| invalid(&why))?;
        let fingerprints: Vec<String> = keys
            .into_iter()
            .flat_map(|key| std::iter::once(key.fingerprint).chain(key.subkeys))
            .collect();
        if let Some(claimed) = parsed
            .fingerprints
            .iter()
            .find(|claimed| !fingerprints.contains(claimed))
        {
            return Err(invalid(&format!(
                "the descriptor claims the fingerprint {claimed}, which no key of the keyring has"
            )));
        }
        Ok(Found {
            descriptor_sha256: registry::keyring_sha256(&descriptor_bytes),
            keyring_sha256: registry::keyring_sha256(&keyring),
            descriptor_bytes,
            descriptor: parsed,
            keyring,
            fingerprints,
            user_ids,
        })
    }
}
