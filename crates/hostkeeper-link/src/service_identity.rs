//! What the daemon reports of its services' identities (`HeartbeatRequest.service_identities`
//! of `hostkeeper.agent.v1`, docs/design/service-identity.md, section 4), as plain data for the
//! loop. The daemon's [`ServiceIdentitySource`] keeps the certificates; the loop only carries what
//! the source says. A daemon that has a source announces `service-identity-v1`.

/// The longest `last_error`, in bytes, that the coordinator reads; a longer one is cut at a
/// character boundary by [`ServiceIdentityReport::new`].
pub const MAX_IDENTITY_ERROR: usize = 256;

/// What a daemon holds of one service's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceIdentityReport {
    /// The service.
    pub service: String,
    /// The last second the installed certificate is valid; none while none is installed.
    pub not_after_unix: Option<u64>,
    /// Why the last try to get or install a certificate failed, at most [`MAX_IDENTITY_ERROR`]
    /// bytes; none once a try succeeds.
    pub last_error: Option<String>,
}

impl ServiceIdentityReport {
    /// The report of `service`, with `last_error` cut to [`MAX_IDENTITY_ERROR`] bytes at a
    /// character boundary.
    pub fn new(
        service: String,
        not_after_unix: Option<u64>,
        last_error: Option<String>,
    ) -> ServiceIdentityReport {
        ServiceIdentityReport {
            service,
            not_after_unix,
            last_error: last_error.map(|text| clip(&text).to_owned()),
        }
    }
}

/// `text` cut to at most [`MAX_IDENTITY_ERROR`] bytes, at a character boundary.
fn clip(text: &str) -> &str {
    let mut end = text.len().min(MAX_IDENTITY_ERROR);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The services' identities of the host, which every heartbeat reports (`service-identity-v1`).
pub trait ServiceIdentitySource: Send + Sync {
    /// The report of each service that has an identity, sorted by service name.
    fn reports(&self) -> Vec<ServiceIdentityReport>;
}

#[cfg(test)]
#[path = "service_identity_tests.rs"]
mod tests;
