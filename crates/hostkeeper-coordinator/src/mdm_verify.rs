//! `Mdm.Verify` of `hostkeeper.operator.v1`: the setup page's button that reads the setup back and
//! proves its parts work (`docs/design/coordinator-mdm.md`, sections 4.9 and 7, task CM11b).
//!
//! It answers, in one call:
//!
//! - the status, as [`Coordinator::mdm_status`] computes it at this call: configured, what is
//!   missing, the expiries read from the material;
//! - whether the Macs' listener runs, whether it has a TLS configuration to serve, and the names
//!   of the certificate it serves;
//! - when an ABM token is held, whether Apple accepts it. `GET /account` of the ABM service is the
//!   proof: it answers only an accepted token (spike `abm.md`). The question is asked through the
//!   port [`AbmAccountCheck`], which the ABM client implements; a coordinator without one says
//!   `not_checked`, not `accepted`;
//! - when the caller names an enrolled Mac, one push to it through the sender the queue uses
//!   ([`hostkeeper_mdm_push::pusher::Pusher::test_push`]). What APNs, or the connection, answered is
//!   one class of [`proto::MdmPushResult`], so that a wrong topic, a dead token and a certificate
//!   of the wrong environment are told apart: the first two are not the same repair as the third.
//!
//! The call is session-only ([`crate::grpc::SESSION_ONLY`]) and audited as `mdm_verify` before
//! anything is asked of Apple, so that a push is never sent without its entry. A check that fails
//! is a field of the answer, not a refusal: the page must show the failure beside what passed.
//! The push is not recorded against the Mac: no command is queued, no floor armed.

use hostkeeper_mdm_push::BoxFuture;
use hostkeeper_mdm_push::apns::PushError;
use hostkeeper_mdm_push::pusher::Target;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{MdmAbmResult, MdmPushResult, MdmRefusal};

use crate::audit_actions;
use crate::mdm_certs::CertFacts;
use crate::mdm_secrets::{AbmToken, SecretKind};
use crate::mdm_server::queue::{DEFAULT_PUSH, target_of};
use crate::mdm_setup::SetupFailure;
use crate::operator::Caller;
use crate::operator_mdm_setup::read_failed;
use crate::service::Coordinator;
use crate::store::mdm::EnrollmentState;

/// The longest enrollment id (`operator.proto`, `mdm.proto`): 64 ASCII letters, digits and `-`.
const MAX_ENROLLMENT_ID: usize = 64;

/// What Apple's account answer says of the account a token is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbmAccount {
    /// `org_name` of the account answer.
    pub organization: String,
}

/// Why the ABM token was not proved.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AbmCheckError {
    /// Apple refused the token: an OAuth session refused, or `401` or `403` on the account
    /// request. The text names no part of the token.
    #[error("{0}")]
    Rejected(String),
    /// Apple could not be reached, or answered in a way that says nothing of the token. The text
    /// names no part of the token.
    #[error("{0}")]
    Unreachable(String),
}

/// What asks Apple's ABM service whether a token is accepted.
///
/// The implementation opens the OAuth session with the token and reads `GET /account`, and gives
/// every try its own timeout: the call waits for it.
pub trait AbmAccountCheck: Send + Sync {
    /// The account `token` is for.
    ///
    /// # Errors
    /// [`AbmCheckError`]: refused, or not reached.
    fn account<'a>(
        &'a self,
        token: &'a AbmToken,
    ) -> BoxFuture<'a, Result<AbmAccount, AbmCheckError>>;
}

impl Coordinator {
    /// `Mdm.Verify`: reads the setup back, asks Apple about the ABM token and, for a named Mac,
    /// sends a test push.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the enrollment id is not one),
    /// `audit_unavailable`, `store_failed`.
    pub async fn mdm_verify(
        &self,
        caller: &Caller,
        request: proto::VerifyRequest,
    ) -> Result<proto::VerifyResponse, MdmRefusal> {
        let by = caller.operator()?;
        let action = audit_actions::MDM_VERIFY;
        let id = request.enrollment_id;
        let subject = (!id.is_empty()).then_some(id.as_str());
        if !id.is_empty() && !is_enrollment_id(&id) {
            self.audit_setup(by, action, None, "refused: invalid_request".to_owned())?;
            return Err(MdmRefusal::InvalidRequest);
        }
        self.audit_setup(
            by,
            action,
            subject,
            format!("test push {}", if id.is_empty() { "no" } else { "yes" }),
        )?;
        let status = self.setup_status()?;
        let (listener, serving, served_names, served_names_the_name) = self.served(&status.name);
        let abm = self.verify_abm().await?;
        let push = if id.is_empty() {
            None
        } else {
            Some(self.verify_push(&id, &status.push_topic).await?)
        };
        Ok(proto::VerifyResponse {
            status: Some(status),
            listener,
            serving,
            served_names,
            served_names_the_name,
            abm: Some(abm),
            push,
        })
    }

    /// Whether the listener runs, whether it has a TLS configuration, the names of the
    /// certificate that configuration serves, and whether they cover `name`.
    fn served(&self, name: &str) -> (bool, bool, Vec<String>, bool) {
        let Some(server) = self.mdm_server() else {
            return (false, false, Vec::new(), false);
        };
        if server.tls().config().is_none() {
            return (true, false, Vec::new(), false);
        }
        // The configuration was made from this certificate a moment ago; one that does not read now
        // (the store failing, a renewal in between) names nothing.
        let facts = self
            .mdm_setup
            .open(SecretKind::TlsCert)
            .ok()
            .flatten()
            .and_then(|pem| CertFacts::read(pem.expose()));
        let covers = !name.is_empty() && facts.as_ref().is_some_and(|f| f.names_host(name));
        (true, true, facts.map_or_else(Vec::new, |f| f.names), covers)
    }

    /// What Apple says of the ABM token held.
    async fn verify_abm(&self) -> Result<proto::MdmAbmCheck, MdmRefusal> {
        let check = |result: MdmAbmResult, organization: String, detail: String| {
            Ok(proto::MdmAbmCheck {
                result: result.into(),
                organization,
                detail,
            })
        };
        let secret = match self.mdm_setup.open(SecretKind::AbmToken) {
            Ok(Some(secret)) => secret,
            Ok(None) => return check(MdmAbmResult::NoToken, String::new(), String::new()),
            Err(SetupFailure::NoKey) => {
                return check(
                    MdmAbmResult::Unreadable,
                    String::new(),
                    "no secrets key is configured".to_owned(),
                );
            }
            Err(SetupFailure::Seal(_)) => {
                return check(
                    MdmAbmResult::Unreadable,
                    String::new(),
                    "the token held does not open".to_owned(),
                );
            }
            Err(other) => return Err(read_failed(other)),
        };
        let Ok(token) = AbmToken::parse(secret.expose()) else {
            return check(
                MdmAbmResult::Unreadable,
                String::new(),
                "the token held is not its five parts".to_owned(),
            );
        };
        let Some(asker) = self.mdm_server().and_then(|server| server.abm()) else {
            return check(
                MdmAbmResult::NotChecked,
                String::new(),
                "this coordinator has no ABM client to ask Apple with".to_owned(),
            );
        };
        match asker.account(&token).await {
            Ok(account) => check(MdmAbmResult::Accepted, account.organization, String::new()),
            Err(AbmCheckError::Rejected(why)) => check(MdmAbmResult::Rejected, String::new(), why),
            Err(AbmCheckError::Unreachable(why)) => {
                check(MdmAbmResult::Unreachable, String::new(), why)
            }
        }
    }

    /// One push to the live enrollment `id`, and what came of it.
    async fn verify_push(&self, id: &str, topic: &str) -> Result<proto::MdmPushCheck, MdmRefusal> {
        let outcome = |result: MdmPushResult, detail: String, apns_id: String, pushed: bool| {
            proto::MdmPushCheck {
                enrollment_id: id.to_owned(),
                result: result.into(),
                detail,
                apns_id,
                topic: if pushed {
                    topic.to_owned()
                } else {
                    String::new()
                },
            }
        };
        let none = |result: MdmPushResult| Ok(outcome(result, String::new(), String::new(), false));
        let Some(server) = self.mdm_server() else {
            return none(MdmPushResult::NoListener);
        };
        let Some(pusher) = server.pusher() else {
            return none(MdmPushResult::NoPushIdentity);
        };
        let enrollment = match server.store.enrollment(id) {
            Ok(Some(enrollment)) if enrollment.state == EnrollmentState::Live => enrollment,
            Ok(_) => return none(MdmPushResult::NoSuchEnrollment),
            Err(why) => {
                tracing::error!(%why, "the enrollment for a test push could not be read");
                return Err(MdmRefusal::StoreFailed);
            }
        };
        let Some((token, magic)) = target_of(&enrollment) else {
            return none(MdmPushResult::NoPushToken);
        };
        let settings = self
            .mdm_setup
            .push_settings()
            .map_or(DEFAULT_PUSH, |(settings, _)| settings);
        let target = Target {
            token: &token,
            magic: &magic,
        };
        Ok(match pusher.test_push(target, settings).await {
            Ok(delivered) => outcome(
                MdmPushResult::Sent,
                String::new(),
                delivered.apns_id.unwrap_or_default(),
                true,
            ),
            Err(why) => {
                let (result, detail) = classify(&why);
                outcome(result, detail, String::new(), true)
            }
        })
    }
}

/// Whether `id` is an enrollment id: 1 to 64 ASCII letters, digits and `-`.
fn is_enrollment_id(id: &str) -> bool {
    id.len() <= MAX_ENROLLMENT_ID && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The class of a failed push and what APNs or the connection said.
///
/// A connection that ended with `BadCertificateEnvironment` is its own class: the certificate is
/// the other environment's, which no other repair than a new certificate cures.
pub(crate) fn classify(why: &PushError) -> (MdmPushResult, String) {
    match why {
        PushError::BadDeviceToken(reason) => (MdmPushResult::BadDeviceToken, reason.clone()),
        PushError::Unregistered => (MdmPushResult::Unregistered, String::new()),
        PushError::TopicDisallowed => (MdmPushResult::TopicDisallowed, String::new()),
        PushError::NoClientCertificate => (MdmPushResult::NoClientCertificate, String::new()),
        PushError::CertificateRejected(reason) => {
            (MdmPushResult::CertificateRejected, reason.clone())
        }
        PushError::CertificateAlert(alert) if alert == "BadCertificateEnvironment" => {
            (MdmPushResult::BadCertificateEnvironment, alert.clone())
        }
        PushError::CertificateAlert(alert) => (MdmPushResult::CertificateRejected, alert.clone()),
        PushError::Throttled => (MdmPushResult::Throttled, String::new()),
        PushError::Unavailable(status) => (MdmPushResult::ApnsUnavailable, status.to_string()),
        PushError::Rejected { status, reason } => {
            (MdmPushResult::Rejected, format!("{status} {reason}"))
        }
        PushError::Connection(reason) => (MdmPushResult::ConnectionFailed, reason.clone()),
    }
}

#[cfg(test)]
#[path = "mdm_verify_tests.rs"]
mod tests;
