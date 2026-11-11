//! The alert `mdm_identity_near_expiry` (`docs/design/coordinator-mdm.md`, 4.2): the identity
//! certificates of the live Macs and the CA that issues them, against the window of the setting
//! `renew_alert_days`. [`crate::alerts`] holds the kinds and the book; this holds what is specific
//! to the identities: the derivation, and the reading of the tables and the setup that feeds it.

use crate::alerts::{Alert, EnrollmentFacts, Kind, Subject};
use crate::mdm_secrets::SecretKind;
use crate::mdm_settings;
use crate::service::Coordinator;

/// `mdm_identity_near_expiry`: the identities that expire within the window and the CA, when it
/// does. Its version is the window in days, or `expired` once any of them has.
pub(crate) fn mdm_identity_near_expiry(enrollment: &EnrollmentFacts, unix: u64) -> Vec<Alert> {
    let Some(days) = enrollment.mdm_renew_alert_days else {
        return Vec::new();
    };
    let window = unix.saturating_add(u64::from(days) * 86_400);
    let ca = enrollment
        .mdm_ca_not_after
        .filter(|not_after| *not_after < window);
    if enrollment.mdm_identities.is_empty() && ca.is_none() {
        return Vec::new();
    }
    let expired = ca.is_some_and(|at| at <= unix)
        || enrollment.mdm_identities.iter().any(|(_, at)| *at <= unix);
    let mut subject = Subject::of(Kind::MdmIdentityNearExpiry);
    subject.version = if expired {
        "expired".to_owned()
    } else {
        days.to_string()
    };
    let mut parts = Vec::new();
    if !enrollment.mdm_identities.is_empty() {
        let serials: Vec<&str> = enrollment
            .mdm_identities
            .iter()
            .map(|(serial, _)| serial.as_str())
            .collect();
        parts.push(format!(
            "the identity certificates of {} expire within {days} days (or have): enroll each Mac again before they do",
            serials.join(", ")
        ));
    }
    if let Some(at) = ca {
        parts.push(format!(
            "the CA that issues the Macs' identities expires at unix {at}: no identity issued after that is valid"
        ));
    }
    vec![Alert {
        subject,
        message: parts.join("; "),
    }]
}

impl Coordinator {
    /// Notes in `facts` the identities of the Macs and the CA that expire within the window of
    /// `renew_alert_days`; a store or a setup that cannot be read is logged and raises no alert of
    /// its own.
    pub(crate) fn note_identities(&self, facts: &mut EnrollmentFacts) {
        let Some(server) = self.mdm_server() else {
            return;
        };
        let setup = &self.mdm_setup;
        let (Ok(secrets), Ok(settings)) = (setup.list_secrets(), setup.settings()) else {
            tracing::warn!(
                "the MDM identities' expiry is not read: the MDM setup could not be read"
            );
            return;
        };
        let days = mdm_settings::number_of(&settings, "renew_alert_days", 90);
        facts.mdm_renew_alert_days = Some(days);
        facts.mdm_ca_not_after = secrets
            .iter()
            .filter(|info| info.kind == SecretKind::MdmCaKey && info.superseded_unix.is_none())
            .find_map(|info| info.not_after_unix)
            .and_then(|at| u64::try_from(at).ok());
        let window = i64::try_from(self.ops.unix_now())
            .unwrap_or(i64::MAX)
            .saturating_add(i64::from(days) * 86_400);
        match server.store.identities_expiring(window) {
            Ok(rows) => {
                facts.mdm_identities = rows
                    .into_iter()
                    .filter_map(|(serial, at)| Some((serial, u64::try_from(at).ok()?)))
                    .collect();
            }
            Err(why) => tracing::warn!(%why, "the MDM identities' expiry is not read"),
        }
    }
}

#[cfg(test)]
#[path = "alerts_mdm_tests.rs"]
mod tests;
