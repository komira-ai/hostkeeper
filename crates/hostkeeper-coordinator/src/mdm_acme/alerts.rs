//! The alerts of the certificate for the MDM name, derived from the renewal's status
//! ([`super::status::Snapshot`]): `mdm_tls_renewal_failed` while attempts fail and
//! `mdm_dns_credential_rejected` while the DNS provider refuses the credential.

use super::status::Snapshot;
use crate::alerts::{Alert, Kind, Subject};

/// The alerts `snapshot` raises.
pub(crate) fn derive(snapshot: &Snapshot) -> Vec<Alert> {
    let mut alerts = Vec::new();
    if let Some(failing) = &snapshot.failing {
        alerts.push(Alert {
            subject: Subject::of(Kind::MdmTlsRenewalFailed),
            message: format!(
                "the certificate for the MDM name could not be renewed ({} attempts since unix {}): \
                 {}. The Macs keep the certificate they have until it expires; upload one by hand \
                 from the MDM setup or fix the cause",
                failing.attempts, failing.since_unix, failing.message
            ),
        });
    }
    if let Some(message) = &snapshot.dns_rejected {
        alerts.push(Alert {
            subject: Subject::of(Kind::MdmDnsCredentialRejected),
            message: format!(
                "{message}: upload another DNS credential from the MDM setup, or give this one \
                 the right to the zone"
            ),
        });
    }
    alerts
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;
