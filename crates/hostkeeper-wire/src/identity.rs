//! Who a certificate says its holder is, read from its subjectAltNames and from nothing else.
//!
//! The CA makes an updater's certificate with exactly these subjectAltNames:
//!
//! - one URI `<role prefix>updater`, the certificate's role;
//! - one URI `<host-role prefix><name>`, what the host runs (any [`RoleName`]);
//! - one DNS name, the host's id.
//!
//! The two prefixes are [`SanPrefixes`]: [`DEFAULT_ROLE_URI_PREFIX`] and
//! [`DEFAULT_HOST_ROLE_URI_PREFIX`] unless a coordinator is started with others, so that
//! certificates made under other prefixes keep being read.
//!
//! The coordinator reads the certificate a caller presents, which TLS has already checked
//! chains to the CA and may be used as a client's: a certificate of any other role (a storage
//! node's, a frontend's, the coordinator's), or one that does not name exactly one of each of
//! the above, is no updater, and the caller is refused. The heartbeat's body is never asked who
//! sent it. The updater reads its own certificate the same way, to know which host it is and
//! what its host runs, so that neither is a flag that could disagree with the certificate (and
//! neither goes in the heartbeat).

use crate::names::{HostId, RoleName};
use crate::san::{SanError, names_of, uri_values_of};

/// The role of an updater's certificate.
pub const UPDATER_ROLE: &str = "updater";

/// How a certificate names its role, by default: this, then the role's name.
pub const DEFAULT_ROLE_URI_PREFIX: &str = "urn:hostkeeper:role:";

/// How a certificate names its host's role, by default: this, then the role's name.
pub const DEFAULT_HOST_ROLE_URI_PREFIX: &str = "urn:hostkeeper:host-role:";

/// The two certificate namespaces: how a certificate names its role and its host's role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanPrefixes {
    role: String,
    host_role: String,
}

/// Why two URI prefixes cannot be a [`SanPrefixes`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrefixError {
    /// A prefix is empty.
    #[error("a URI prefix is empty")]
    Empty,
    /// A prefix has a byte that is not visible ASCII, or a space.
    #[error("a URI prefix must be visible ASCII without spaces: {0:?}")]
    NotVisible(String),
    /// The two are the same, or one starts the other, so a URI could be of both.
    #[error(
        "the role prefix and the host-role prefix must differ, and neither may start the other"
    )]
    Overlap,
}

impl SanPrefixes {
    /// The pair `role` and `host_role`: each non-empty visible ASCII without spaces, the two
    /// different, and neither a prefix of the other (a URI is then of at most one).
    ///
    /// # Errors
    /// [`PrefixError`]: see its variants.
    pub fn new(role: &str, host_role: &str) -> Result<Self, PrefixError> {
        for prefix in [role, host_role] {
            if prefix.is_empty() {
                return Err(PrefixError::Empty);
            }
            if !prefix.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(PrefixError::NotVisible(prefix.to_owned()));
            }
        }
        if role.starts_with(host_role) || host_role.starts_with(role) {
            return Err(PrefixError::Overlap);
        }
        Ok(Self {
            role: role.to_owned(),
            host_role: host_role.to_owned(),
        })
    }

    /// The prefix of the role URI.
    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }

    /// The prefix of the host-role URI.
    #[must_use]
    pub fn host_role(&self) -> &str {
        &self.host_role
    }
}

impl Default for SanPrefixes {
    fn default() -> Self {
        Self {
            role: DEFAULT_ROLE_URI_PREFIX.to_owned(),
            host_role: DEFAULT_HOST_ROLE_URI_PREFIX.to_owned(),
        }
    }
}

/// An updater, as its certificate names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The host.
    pub host_id: HostId,
    /// What the host runs.
    pub role: RoleName,
}

/// Why a certificate is no updater's.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    /// The bytes are not a certificate.
    #[error("the client certificate does not parse: {0}")]
    NotACertificate(String),
    /// The certificate presents no role, several, or one that is not `updater`.
    #[error("the client certificate's role is not `updater` (it names {0:?})")]
    NotAnUpdater(Vec<String>),
    /// It does not name exactly one host role that is a role name.
    #[error("the client certificate must name exactly one host role (it names {0:?})")]
    HostRoleName(Vec<String>),
    /// It does not name exactly one host id.
    #[error("the client certificate must name exactly one host id (it names {0:?})")]
    HostId(Vec<String>),
}

impl From<SanError> for IdentityError {
    fn from(e: SanError) -> Self {
        Self::NotACertificate(e.to_string())
    }
}

/// The updater a DER certificate names, under `prefixes`.
///
/// # Errors
/// [`IdentityError`]: see its variants.
pub fn identity_of(der: &[u8], prefixes: &SanPrefixes) -> Result<Identity, IdentityError> {
    let roles = uri_values_of(der, prefixes.role())?;
    if roles != [UPDATER_ROLE] {
        return Err(IdentityError::NotAnUpdater(roles));
    }
    let host_roles = uri_values_of(der, prefixes.host_role())?;
    let role = match host_roles.as_slice() {
        [name] => name.parse::<RoleName>().ok(),
        _ => None,
    }
    .ok_or(IdentityError::HostRoleName(host_roles))?;
    let dns = names_of(der)?;
    let host_id = match dns.dns() {
        [name] => name.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| IdentityError::HostId(dns.dns().to_vec()))?;
    Ok(Identity { host_id, role })
}
