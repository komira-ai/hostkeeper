//! The tables of the MDM server inside the coordinator, behind one port, [`MdmStore`]
//! (`docs/design/coordinator-mdm.md`, sections 4.2, 4.5, 4.6, 5.2 and 9.1): the Mac enrollments of
//! a serial with their push state, the identities (SCEP challenges and the certificates issued
//! under them), the commands queued for an enrollment, the ABM device list and the ABM sync state.
//!
//! Two stores implement it and one suite ([`conformance_tests`]) runs against both: [`files::FileMdm`],
//! one file per family beside each other in a directory, each replaced whole, and
//! [`crate::store::postgres::mdm::PgMdm`], the tables of migration `0010`. The rules are the ones the
//! model check of the queue and of enrollment (`crates/hostkeeper-mdm-model`, CM4) found necessary,
//! each kept by the store itself so that no caller can forget it:
//!
//! - a serial has at most one live enrollment; entering a new one retires the old one, cancels
//!   its commands and retires its identity in the same change;
//! - enqueuing an id that exists changes nothing, and enqueuing for an enrollment that is not live
//!   is refused;
//! - a claim takes only a queued command of a live enrollment, never one in `tried`, oldest first;
//! - an answer changes a command only while it is sent, and only through the enrollment it was
//!   sent to; any other answer is dropped;
//! - a challenge is consumed once; a repeat with the same key gets the stored certificate, another
//!   key is refused, and a refusal changes nothing;
//! - a retired or dead identity is refused.
//!
//! Times are unix seconds as the database holds them (`bigint`). Every call that needs the time is
//! given it, so the callers' injected clock decides, and nothing here reads one.

use std::fmt;

pub(crate) mod files;
pub(crate) mod state;

pub(crate) use state::consume;

/// Why a call of the MDM store failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MdmError {
    /// The files or the database could not be read or written; the change is not made.
    #[error("the MDM store failed: {0}")]
    Store(String),
    /// A challenge with this hash was issued before.
    #[error("a challenge with this hash exists")]
    ChallengeExists,
    /// The enrollment was replaced or checked out, and does not come back.
    #[error("enrollment {0} has ended and cannot be entered again")]
    EnrollmentEnded(String),
}

/// Where an enrollment stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EnrollmentState {
    /// The one enrollment of its serial that commands are queued for.
    Live,
    /// A newer enrollment of the serial took its place.
    Replaced,
    /// The Mac left the MDM (`CheckOut`).
    CheckedOut,
}

/// Where a command stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandState {
    /// Waiting for the Mac.
    Queued,
    /// Delivered to a connection, no answer yet.
    Sent,
    /// The Mac answered `Acknowledged`.
    Acked,
    /// The Mac answered `Error`.
    Errored,
    /// Its enrollment ended before it was answered.
    Cancelled,
}

/// Where an identity stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdentityState {
    /// A challenge was issued; no certificate yet.
    Challenged,
    /// The certificate was issued, which consumed the challenge.
    Issued,
    /// Its enrollment was replaced or checked out: no request on it is accepted.
    Retired,
    /// Refused for good by an operator or a rule.
    Dead,
}

macro_rules! spelling {
    ($ty:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            /// The spelling the tables and the files hold.
            pub(crate) fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            /// The state spelled `text`, as [`Self::as_str`] spells it.
            pub(crate) fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

spelling!(EnrollmentState { Live => "live", Replaced => "replaced", CheckedOut => "checked_out" });
spelling!(CommandState {
    Queued => "queued", Sent => "sent", Acked => "acked", Errored => "errored",
    Cancelled => "cancelled"
});
spelling!(IdentityState {
    Challenged => "challenged", Issued => "issued", Retired => "retired", Dead => "dead"
});

/// One enrollment of a Mac.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Enrollment {
    /// The MDM enrollment id (the Mac's `EnrollmentID`).
    pub(crate) enrollment_id: String,
    /// The serial of the Mac, as ABM spells it.
    pub(crate) serial: String,
    /// The UDID the Mac reported.
    pub(crate) udid: String,
    /// Where it stands; [`MdmStore::replace_live`] enters it as live whatever is set here.
    pub(crate) state: EnrollmentState,
    /// The SHA-256 of the identity certificate it presents, in hex.
    pub(crate) cert_sha256: Option<String>,
    /// The push token of the last `TokenUpdate`.
    pub(crate) push_token: Option<Vec<u8>>,
    /// The push magic of the last `TokenUpdate`.
    pub(crate) push_magic: Option<String>,
    /// The push topic of the last `TokenUpdate`.
    pub(crate) topic: Option<String>,
    /// A push was sent and no request from the enrollment has reached `/mdm` since.
    pub(crate) push_outstanding: bool,
    /// When the last push was sent.
    pub(crate) last_push_unix: Option<i64>,
    /// When the last request of the enrollment reached `/mdm`.
    pub(crate) last_seen_unix: Option<i64>,
    /// When the enrollment was entered.
    pub(crate) created_unix: i64,
}

impl Enrollment {
    /// A live enrollment `enrollment_id` of `serial` and `udid`, entered at `created_unix`, with no
    /// identity, push token or contact yet.
    pub(crate) fn new(enrollment_id: &str, serial: &str, udid: &str, created_unix: i64) -> Self {
        Self {
            enrollment_id: enrollment_id.to_owned(),
            serial: serial.to_owned(),
            udid: udid.to_owned(),
            state: EnrollmentState::Live,
            cert_sha256: None,
            push_token: None,
            push_magic: None,
            topic: None,
            push_outstanding: false,
            last_push_unix: None,
            last_seen_unix: None,
            created_unix,
        }
    }
}

// The push token and magic are what a push is made with: they stay out of every log line.
impl fmt::Debug for Enrollment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Enrollment")
            .field("enrollment_id", &self.enrollment_id)
            .field("serial", &self.serial)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// A challenge issued for a `/enroll` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Challenge {
    /// The SHA-256 of the challenge in hex: the challenge itself is never kept.
    pub(crate) challenge_sha256: String,
    /// The serial it is bound to.
    pub(crate) serial: String,
    /// The UDID of the blob it is bound to.
    pub(crate) udid: String,
    /// The second it stops being valid for a first issuance.
    pub(crate) expires_unix: i64,
    /// An operator confirmed the request it was issued for.
    pub(crate) confirmed: bool,
}

/// The certificate recorded under a challenge.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Issuance {
    /// The SHA-256 of the certificate in hex.
    pub(crate) cert_sha256: String,
    /// The certificate, DER.
    pub(crate) cert_der: Vec<u8>,
    /// The second the certificate stops being valid.
    pub(crate) not_after_unix: i64,
}

impl fmt::Debug for Issuance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Issuance")
            .field("cert_sha256", &self.cert_sha256)
            .field("not_after_unix", &self.not_after_unix)
            .finish_non_exhaustive()
    }
}

/// A row of identities: a challenge and, once consumed, what was issued under it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Identity {
    /// The SHA-256 of the challenge in hex.
    pub(crate) challenge_sha256: String,
    /// The serial it is bound to.
    pub(crate) serial: String,
    /// The UDID it is bound to.
    pub(crate) udid: String,
    /// When the challenge stops being valid for a first issuance.
    pub(crate) expires_unix: i64,
    /// An operator confirmed the request.
    pub(crate) confirmed: bool,
    /// Where it stands.
    pub(crate) state: IdentityState,
    /// The SHA-256 of the public key of the request that consumed it, in hex.
    pub(crate) key_sha256: Option<String>,
    /// What was issued; `None` while challenged.
    pub(crate) issued: Option<Issuance>,
    /// When it was issued.
    pub(crate) issued_unix: Option<i64>,
}

/// Why a certificate is refused for a challenge. A refusal changes nothing, so the challenge is not
/// consumed by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No such challenge.
    Unknown,
    /// Past its hour, and not issued before.
    Expired,
    /// Consumed by another key.
    KeyMismatch,
    /// Its identity is retired or dead.
    Ended,
}

/// What [`MdmStore::consume_challenge`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Consumed {
    /// The challenge was consumed and the issuance recorded.
    Issued,
    /// The challenge was consumed by the same key before: the certificate stored then.
    Replayed(Issuance),
    /// Nothing was changed.
    Refused(Refusal),
}

/// A command to queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewCommand {
    /// The command's UUID.
    pub(crate) id: String,
    /// The enrollment it is for.
    pub(crate) enrollment_id: String,
    /// Its `RequestType`.
    pub(crate) command_type: String,
    /// The plist the Mac is sent.
    pub(crate) payload: Vec<u8>,
    /// When it is enqueued.
    pub(crate) created_unix: i64,
}

/// What [`MdmStore::enqueue`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Enqueued {
    /// It is queued.
    Queued,
    /// The id existed; nothing changed.
    Exists,
    /// The enrollment is not live; nothing changed.
    NotLive,
}

/// One command of the queue.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Command {
    /// The command's UUID.
    pub(crate) id: String,
    /// The order it was enqueued in, over all enrollments.
    pub(crate) seq: i64,
    /// The enrollment it is for.
    pub(crate) enrollment_id: String,
    /// Its `RequestType`.
    pub(crate) command_type: String,
    /// The plist the Mac is sent.
    pub(crate) payload: Vec<u8>,
    /// Where it stands.
    pub(crate) state: CommandState,
    /// The last accepted answer was `NotNow` and it was not sent since.
    pub(crate) deferred: bool,
    /// How many times it was sent.
    pub(crate) deliveries: i64,
    /// When it was enqueued.
    pub(crate) created_unix: i64,
    /// When it was last sent.
    pub(crate) sent_unix: Option<i64>,
    /// When it reached an end state.
    pub(crate) done_unix: Option<i64>,
    /// The Mac's answer to it, when it sent one.
    pub(crate) response: Option<Vec<u8>>,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("id", &self.id)
            .field("enrollment_id", &self.enrollment_id)
            .field("command_type", &self.command_type)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// What a Mac answers a command with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// Done.
    Acknowledged,
    /// Failed for good.
    Error,
    /// Not now: the command goes back to the queue.
    NotNow,
}

/// What [`MdmStore::answer`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answered {
    /// The command was sent to this enrollment and the answer was taken.
    Taken,
    /// The answer was dropped: no such command, not sent, or sent to another enrollment.
    Dropped,
}

/// A device of ABM's list.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AbmDevice {
    /// The serial, as Apple spells it.
    pub(crate) serial: String,
    /// The model.
    pub(crate) model: Option<String>,
    /// Apple's description.
    pub(crate) description: Option<String>,
    /// ABM's `profile_status`: empty, assigned, pushed or removed.
    pub(crate) profile_status: String,
    /// The profile assigned to it.
    pub(crate) profile_uuid: Option<String>,
    /// When the sync first saw it.
    pub(crate) first_seen_unix: i64,
    /// When the sync last wrote it.
    pub(crate) synced_unix: i64,
    /// When ABM reported it removed; `None` while it is ours.
    pub(crate) removed_unix: Option<i64>,
}

/// Where the ABM sync stands.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AbmState {
    /// The cursor of the last sync, empty before the first.
    pub(crate) cursor: String,
    /// The UUID of the ADE profile this coordinator defined.
    pub(crate) profile_uuid: Option<String>,
    /// The digest of the profile as rendered.
    pub(crate) profile_digest: Option<String>,
    /// When the last sync ended.
    pub(crate) synced_unix: Option<i64>,
}

/// What one page of the ABM sync changes, made in one transaction with the cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AbmBatch {
    /// Devices added or modified: each replaces the row of its serial and clears `removed_unix`,
    /// keeping `first_seen_unix` of a row that exists.
    pub(crate) upserts: Vec<AbmDevice>,
    /// Serials ABM reported removed; one the table does not hold is ignored.
    pub(crate) removed: Vec<String>,
    /// When the removal was seen.
    pub(crate) removed_unix: i64,
    /// The sync state after the page.
    pub(crate) state: Option<AbmState>,
}

/// The MDM tables.
pub(crate) trait MdmStore: Send + Sync {
    /// Enters `new` as the live enrollment of its serial. The live enrollment of the serial that
    /// is another one is replaced, its commands cancelled and its identity retired, in the same
    /// change. Entering an enrollment that is live is a refresh of its row.
    ///
    /// The store does not check `new.cert_sha256` against an identity: an enrollment without one
    /// retires no identity when it ends, so the caller enters each with the certificate it presented,
    /// after it has checked that the identity is issued for the same serial and UDID.
    ///
    /// # Errors
    /// [`MdmError::EnrollmentEnded`]: `new` was replaced or checked out before.
    /// [`MdmError::Store`]: the change is not made.
    fn replace_live(&self, new: &Enrollment) -> Result<Vec<String>, MdmError>;

    /// The `CheckOut` of a live enrollment: it ends, its commands are cancelled and its identity
    /// retired. `false` when it is not live.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn check_out(&self, enrollment_id: &str, now_unix: i64) -> Result<bool, MdmError>;

    /// Records the push token, magic and topic of a live enrollment (`TokenUpdate`). `false`
    /// when it is not live.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn set_push(
        &self,
        enrollment_id: &str,
        token: &[u8],
        magic: &str,
        topic: &str,
    ) -> Result<bool, MdmError>;

    /// Records that a push was sent to the enrollment at `now_unix`: it is outstanding.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn record_push(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError>;

    /// Records that a request of the enrollment reached `/mdm` at `now_unix`: no push is
    /// outstanding after it.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn record_contact(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError>;

    /// The enrollment `enrollment_id`.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn enrollment(&self, enrollment_id: &str) -> Result<Option<Enrollment>, MdmError>;

    /// The live enrollment of `serial`.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn live_enrollment(&self, serial: &str) -> Result<Option<Enrollment>, MdmError>;

    /// Records an issued challenge.
    ///
    /// # Errors
    /// [`MdmError::ChallengeExists`]: the hash was issued before. [`MdmError::Store`].
    fn issue_challenge(&self, challenge: &Challenge) -> Result<(), MdmError>;

    /// Consumes the challenge `challenge_sha256` for the request whose public key hashes to
    /// `key_sha256`, recording `issuance` as what was issued; a repeat with the same key gets the
    /// stored issuance, and a request that is refused changes nothing.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn consume_challenge(
        &self,
        challenge_sha256: &str,
        key_sha256: &str,
        issuance: &Issuance,
        now_unix: i64,
    ) -> Result<Consumed, MdmError>;

    /// The identity row of a challenge.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn challenge(&self, challenge_sha256: &str) -> Result<Option<Identity>, MdmError>;

    /// The identity row of an issued certificate.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn identity(&self, cert_sha256: &str) -> Result<Option<Identity>, MdmError>;

    /// Marks the identity of a certificate dead, whatever it was. `false` when none is held.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn kill_identity(&self, cert_sha256: &str) -> Result<bool, MdmError>;

    /// Queues a command.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn enqueue(&self, command: &NewCommand) -> Result<Enqueued, MdmError>;

    /// Takes the oldest queued command of a live enrollment that is not in `tried`, and marks it
    /// sent at `now_unix`.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn claim(
        &self,
        enrollment_id: &str,
        tried: &[String],
        now_unix: i64,
    ) -> Result<Option<Command>, MdmError>;

    /// Takes the Mac's answer to command `id`, sent to `enrollment_id`, with the reply it
    /// carried. `Acknowledged` and `Error` end the command; `NotNow` queues it again, marked
    /// deferred.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn answer(
        &self,
        enrollment_id: &str,
        id: &str,
        answer: Answer,
        response: Option<&[u8]>,
        now_unix: i64,
    ) -> Result<Answered, MdmError>;

    /// Queues again every command sent at or before `sent_before_unix` that has no answer, and
    /// answers the `(id, enrollment id)` of each, so that the caller pushes.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn requeue_timed_out(&self, sent_before_unix: i64) -> Result<Vec<(String, String)>, MdmError>;

    /// The command `id`.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn command(&self, id: &str) -> Result<Option<Command>, MdmError>;

    /// The commands of an enrollment, in the order they were enqueued.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn commands(&self, enrollment_id: &str) -> Result<Vec<Command>, MdmError>;

    /// The ids of the live enrollments that hold a queued command, in id order: what a restarted
    /// coordinator pushes for, since the Mac does not call until it is pushed.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn enrollments_with_queued(&self) -> Result<Vec<String>, MdmError>;

    /// Applies one page of the ABM sync in one change.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn abm_apply(&self, batch: &AbmBatch) -> Result<(), MdmError>;

    /// The ABM device `serial`.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn abm_device(&self, serial: &str) -> Result<Option<AbmDevice>, MdmError>;

    /// Every ABM device, in the order of the serials' bytes.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn abm_devices(&self) -> Result<Vec<AbmDevice>, MdmError>;

    /// The ABM sync state; `None` before the first sync wrote it.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    fn abm_state(&self) -> Result<Option<AbmState>, MdmError>;
}

#[cfg(test)]
#[path = "conformance_tests.rs"]
mod conformance_tests;

#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;
