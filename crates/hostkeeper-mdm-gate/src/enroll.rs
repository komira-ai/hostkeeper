//! The gate's enrollment side (docs/design/hostkeeper-mdm.md sections 4 and 5): the config
//! profile, and the one-time nonce pushed once the Mac's daemon has posted its request.
//!
//! The gate asks the coordinator's enrollment through one port, [`Enrollments`]: whether a
//! serial is on the allowlist, which requests claiming it wait for an operator, when a request
//! claiming it was last approved, a new nonce bound to a request, and an audit line. It is a
//! library call, since the gate is linked into the coordinator.
//!
//! **When the gate looks.** At each webhook event of a device (that device), at each request
//! posted to the coordinator ([`Gate::wake`]), and once a minute ([`run`], [`TICK`]): every
//! device. The time is the gate's injected clock. One look at a time, in the same turn as the
//! webhook's events.
//!
//! **What one look does, for a device** that is enrolled, has a serial from its
//! `DeviceInformation` answer that no other live enrollment has, and whose serial is on the
//! allowlist (anything else: nothing):
//!
//! 1. If a request claiming the serial was approved in this enrollment (at or after its first
//!    `TokenUpdate`), the gate sends nothing more to it in this enrollment, and removes the
//!    nonce profile it pushed in this enrollment (`RemoveProfile`, once).
//! 2. The config profile is installed, once per enrollment. One the device answered with an
//!    error is sent again [`CONFIG_RESEND_SECS`] after that answer; one NanoHUB never queued is
//!    sent again at the next look.
//! 3. A nonce is issued only when every condition of design 5.1 holds: the device is
//!    supervised (and enrolled, as above); the serial is on the allowlist (as above); no nonce
//!    issued for the serial is still good: unexpired ([`NONCE_VALID_SECS`] from its issue: at
//!    3600 seconds it is, at 3601 it is not) and unconsumed (no approval of the serial recorded
//!    since its issue); and exactly one request is a candidate: waiting for an
//!    operator, not a duplicate claim, claiming the serial and a platform UUID equal to the
//!    device's UDID (letter case aside), and posted at or after the first `TokenUpdate` of the
//!    current enrollment. The nonce is bound to that request; its profile is pushed and the push
//!    audited `mdm_nonce_pushed`.
//! 4. While the nonce is good, its profile is pushed again, with the same nonce, when the
//!    device answered the last push with an error at least [`NONCE_RESEND_SECS`] ago, or NanoHUB
//!    never queued it. No new nonce is issued.
//!
//! **What is kept where.** The record says when a nonce was issued, for which request, and what
//! became of its pushes; the nonce itself is only in the coordinator's store (by its SHA-256) and
//! in the gate's memory. The record is written before the nonce is issued, so a crash between
//! the two holds the device for an hour (the record says a nonce lives) and never makes two live
//! nonces. After a restart the gate has no bytes to push again: a push that failed waits for the
//! nonce to expire, and a new one is issued then.
//!
//! No byte of a nonce is logged or audited: [`Nonce`] and [`crate::Command`] show none in their
//! `Debug`, and the audit line names the serial, the request and the push count.

use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_enrollment::enroll::MAC_NONCE_VALID_SECS;
use hostkeeper_enrollment::names::KeyHash;

use crate::command::{Command, INSTALL_PROFILE};
use crate::gate::Gate;
use crate::ids::CommandUuid;
use crate::profile::{DaemonProfile, ProfileIdentifier, nonce_profile};
use crate::record::{DeviceRecord, NonceRecord, ProfileCommand, Sent};

/// How often every device is looked at.
pub const TICK: Duration = Duration::from_secs(60);

/// How long a nonce is good for after its issue, in seconds: the coordinator's.
pub const NONCE_VALID_SECS: u64 = MAC_NONCE_VALID_SECS;

/// How long after an error answer the nonce profile is pushed again, in seconds.
pub const NONCE_RESEND_SECS: u64 = 60;

/// How long after an error answer the config profile is sent again, in seconds.
pub const CONFIG_RESEND_SECS: u64 = 3600;

/// The audit action of a nonce profile NanoHUB queued.
pub const ACTION_NONCE_PUSHED: &str = "mdm_nonce_pushed";

/// A request waiting for an operator that claims a Mac serial, as the gate judges it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    /// The request's id: its key hash.
    pub request_id: KeyHash,
    /// The platform UUID it claims.
    pub platform_uuid: String,
    /// When it was posted, in unix seconds.
    pub posted_unix: u64,
    /// It is flagged a duplicate claim.
    pub duplicate_claim: bool,
}

/// The enrollment could not answer; the gate does nothing for the device this time.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct EnrollmentsError(pub String);

/// A nonce as the enrollment issued it. Its `Debug` shows no byte of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Nonce(Vec<u8>);

impl Nonce {
    /// The nonce of `bytes`.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, for the profile.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Nonce(..)")
    }
}

/// What the gate asks the coordinator's enrollment.
pub trait Enrollments: Send + Sync {
    /// Whether `serial` is on the Mac allowlist.
    fn allowed(&self, serial: &str) -> bool;

    /// The requests waiting for an operator (duplicate claims included) that claim `serial`.
    ///
    /// # Errors
    /// [`EnrollmentsError`]: the store cannot be read.
    fn waiting(&self, serial: &str) -> Result<Vec<Waiting>, EnrollmentsError>;

    /// When an approval of a request claiming `serial` was last recorded, in unix seconds;
    /// `None` when none was.
    ///
    /// # Errors
    /// [`EnrollmentsError`]: the store cannot be read.
    fn last_approval_unix(&self, serial: &str) -> Result<Option<u64>, EnrollmentsError>;

    /// A new nonce for `serial`, bound to the waiting request `request_id`.
    ///
    /// # Errors
    /// [`EnrollmentsError`]: none was issued.
    fn issue_nonce(&self, serial: &str, request_id: &KeyHash) -> Result<Nonce, EnrollmentsError>;

    /// Writes an audit line of `action` with `reason`.
    fn audit(&self, action: &str, reason: &str);
}

/// What the enrollment side needs: the farm's profile identifier, the daemon config its config
/// profile carries, and the enrollment.
pub struct EnrollSide {
    /// The farm's profile identifier.
    pub identifier: ProfileIdentifier,
    /// The daemon config of the config profile.
    pub config: DaemonProfile,
    /// The coordinator's enrollment.
    pub enrollments: Arc<dyn Enrollments>,
}

impl fmt::Debug for EnrollSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrollSide")
            .field("identifier", &self.identifier)
            .finish_non_exhaustive()
    }
}

/// The nonce the gate pushed a device, held in memory to push it again.
pub(crate) struct Held {
    pub(crate) request_id: KeyHash,
    pub(crate) issued_unix: u64,
    pub(crate) nonce: Nonce,
}

/// Looks at every device at each [`Gate::wake`] and every `every`, for as long as the future
/// is polled. It never returns.
pub async fn run(gate: Arc<Gate>, every: Duration) -> Infallible {
    loop {
        // Woken, or `every` passed: either way, one look.
        let _ = tokio::time::timeout(every, gate.woken()).await;
        gate.tick().await;
    }
}

/// Whether `nonce` is still good at `now`: unexpired, and unconsumed (design 5.1, condition 4).
/// An approval of its serial recorded at or after its issue (`last_approval`) ends it: that
/// approval consumed it, or approved its request without it, so it approves nothing more.
fn live(nonce: &NonceRecord, now: u64, last_approval: Option<u64>) -> bool {
    now.saturating_sub(nonce.issued_unix) <= NONCE_VALID_SECS
        && last_approval.is_none_or(|at| at < nonce.issued_unix)
}

/// The times one look judges a device's nonce by.
#[derive(Clone, Copy)]
struct Look {
    /// When the device's current enrollment began.
    since: u64,
    /// When an approval of its serial was last recorded.
    last_approval: Option<u64>,
    /// Now.
    now: u64,
}

/// Whether the profile command `command` (of `record`) is to be sent again at `now`: never
/// queued, or answered with an error at least `after` seconds ago.
fn again(record: &DeviceRecord, command: &ProfileCommand, now: u64, after: u64) -> bool {
    if record.is_unanswered(&command.uuid) {
        return false;
    }
    match command.answer {
        None => true,
        Some(answer) => !answer.acknowledged && now.saturating_sub(answer.at_unix) >= after,
    }
}

impl Gate {
    /// Every device, looked at once.
    pub async fn tick(&self) {
        let Some(side) = self.enroll_side() else {
            return;
        };
        let _turn = self.turn().await;
        for record in self.records() {
            self.advance(Some(side), record).await;
        }
    }

    /// One look at the device of `record`, as the gate holds it, by the enrollment side `side`
    /// (none: nothing); the caller holds the turn.
    pub(crate) async fn advance(&self, side: Option<&EnrollSide>, record: DeviceRecord) {
        let now = self.now();
        let eligible = side.and_then(|side| {
            self.eligible(side, &record)
                .map(|(serial, since)| (side, serial, since))
        });
        let Some((side, serial, since)) = eligible else {
            return;
        };
        let last_approval = match side.enrollments.last_approval_unix(&serial) {
            Ok(at) => at,
            Err(why) => {
                tracing::warn!(enrollment_id = %record.enrollment_id, %why, "the enrollment did not say whether the Mac is approved");
                return;
            }
        };
        if last_approval.is_some_and(|at| at >= since) {
            return self.remove_nonce(side, record, now).await;
        }
        // One profile command a look: a config profile sent now is followed by the nonce at
        // the next look, which the daemon, not yet installed, cannot have asked for anyway.
        if !self.send_config(side, record.clone(), now).await {
            let when = Look {
                since,
                last_approval,
                now,
            };
            self.push_nonce(side, record, &serial, when).await;
        }
    }

    /// The device's serial and the start of its enrollment, when the gate acts on it: enrolled,
    /// with a serial no other live enrollment has, on the allowlist.
    fn eligible(&self, side: &EnrollSide, record: &DeviceRecord) -> Option<(String, u64)> {
        let since = record
            .enrolled_since_unix
            .filter(|_| record.is_enrolled())?;
        let serial = record.serial.clone()?;
        // This record has the serial, so the gate knows it; an error is two live enrollments.
        let unique = self.status(&serial).is_ok();
        (unique && side.enrollments.allowed(&serial)).then_some((serial, since))
    }

    /// The config profile, when it is due; whether it was.
    async fn send_config(&self, side: &EnrollSide, mut record: DeviceRecord, now: u64) -> bool {
        let due = record
            .config_profile
            .as_ref()
            .is_none_or(|command| again(&record, command, now, CONFIG_RESEND_SECS));
        if !due {
            return false;
        }
        let Some([uuid, profile, payload]) = self.uuids(&mut record) else {
            return true;
        };
        record.config_profile = Some(ProfileCommand::new(uuid.clone(), now));
        let bytes = side.config.profile(&side.identifier, [&profile, &payload]);
        let command = Command::install_profile(uuid, bytes);
        if self.dispatch(&mut record, &command, now).await {
            tracing::info!(enrollment_id = %record.enrollment_id, "sent the config profile");
        }
        true
    }

    /// The nonce profile: a new nonce when every condition holds, or the live one again.
    async fn push_nonce(&self, side: &EnrollSide, record: DeviceRecord, serial: &str, when: Look) {
        let Look {
            since,
            last_approval,
            now,
        } = when;
        if record.supervised != Some(true) {
            return;
        }
        if let Some(nonce) = record
            .nonce
            .clone()
            .filter(|nonce| live(nonce, now, last_approval))
        {
            return self.push_again(side, record, nonce, now).await;
        }
        let live_elsewhere = self.records().iter().any(|other| {
            other
                .nonce
                .as_ref()
                .is_some_and(|nonce| nonce.serial == serial && live(nonce, now, last_approval))
        });
        if live_elsewhere {
            return;
        }
        let waiting = match side.enrollments.waiting(serial) {
            Ok(waiting) => waiting,
            Err(why) => {
                tracing::warn!(enrollment_id = %record.enrollment_id, %why, "the enrollment did not list the Mac's requests");
                return;
            }
        };
        let candidates: Vec<&Waiting> = waiting
            .iter()
            .filter(|request| {
                !request.duplicate_claim
                    && !record.udid.is_empty()
                    && request.platform_uuid.eq_ignore_ascii_case(&record.udid)
                    && request.posted_unix >= since
            })
            .collect();
        match candidates.as_slice() {
            [candidate] => {
                let request_id = candidate.request_id;
                self.issue(side, record, serial, request_id, now).await;
            }
            [] => {}
            _ => {
                tracing::info!(
                    enrollment_id = %record.enrollment_id,
                    candidates = candidates.len(),
                    "more than one request is a candidate for the Mac: no nonce, the operator decides"
                );
            }
        }
    }

    /// Issues a nonce for `serial` bound to `request_id`, and pushes its profile.
    async fn issue(
        &self,
        side: &EnrollSide,
        mut record: DeviceRecord,
        serial: &str,
        request_id: KeyHash,
        now: u64,
    ) {
        let before = record.clone();
        let Some([uuid, profile, payload]) = self.uuids(&mut record) else {
            return;
        };
        let nonce = NonceRecord {
            serial: serial.to_owned(),
            request_id,
            issued_unix: now,
            enrollment: record.enrollment,
            pushes: 0,
            push: Some(ProfileCommand::new(uuid.clone(), now)),
            removal: None,
        };
        record.nonce = Some(nonce.clone());
        stage(&mut record, &uuid, INSTALL_PROFILE, now);
        // Written before the nonce exists, so a crash between the two holds the device for an
        // hour and never makes two live nonces.
        if !self.kept(&record) {
            return;
        }
        let bytes = match side.enrollments.issue_nonce(serial, &request_id) {
            Ok(bytes) => bytes,
            Err(why) => {
                tracing::warn!(enrollment_id = %record.enrollment_id, %why, "no nonce was issued for the Mac");
                self.kept(&before);
                return;
            }
        };
        let profile = nonce_profile(
            &side.identifier,
            bytes.as_bytes(),
            &request_id,
            [&profile, &payload],
        );
        self.hold(
            &record.enrollment_id,
            Held {
                request_id,
                issued_unix: now,
                nonce: bytes,
            },
        );
        let command = Command::install_profile(uuid, profile);
        self.deliver_nonce(side, record, nonce, &command).await;
    }

    /// The profile of the live `nonce` (the record's) again, when its last push is to be sent
    /// again and the gate still holds the nonce.
    async fn push_again(
        &self,
        side: &EnrollSide,
        mut record: DeviceRecord,
        nonce: NonceRecord,
        now: u64,
    ) {
        let due = nonce.enrollment == record.enrollment
            && nonce
                .push
                .as_ref()
                .is_none_or(|push| again(&record, push, now, NONCE_RESEND_SECS));
        if !due {
            return;
        }
        let Some(held) = self.held(&record.enrollment_id, &nonce) else {
            tracing::info!(
                enrollment_id = %record.enrollment_id,
                "the nonce profile is not pushed again: the nonce was issued before the coordinator started; a new one is issued once it expires"
            );
            return;
        };
        let Some([uuid, profile, payload]) = self.uuids(&mut record) else {
            return;
        };
        let bytes = nonce_profile(
            &side.identifier,
            held.as_bytes(),
            &nonce.request_id,
            [&profile, &payload],
        );
        let nonce = NonceRecord {
            push: Some(ProfileCommand::new(uuid.clone(), now)),
            ..nonce
        };
        record.nonce = Some(nonce.clone());
        stage(&mut record, &uuid, INSTALL_PROFILE, now);
        if !self.kept(&record) {
            return;
        }
        let command = Command::install_profile(uuid, bytes);
        self.deliver_nonce(side, record, nonce, &command).await;
    }

    /// Sends the profile `command` of `nonce`, staged and kept in `record`; once NanoHUB queued
    /// it, counts and audits the push.
    async fn deliver_nonce(
        &self,
        side: &EnrollSide,
        mut record: DeviceRecord,
        mut nonce: NonceRecord,
        command: &Command,
    ) {
        if !self.post(&mut record, command).await {
            return;
        }
        nonce.pushes = nonce.pushes.saturating_add(1);
        let reason = format!(
            "serial {}; request {}; push {}",
            nonce.serial, nonce.request_id, nonce.pushes
        );
        tracing::info!(
            enrollment_id = %record.enrollment_id,
            serial = %nonce.serial,
            request_id = %nonce.request_id,
            push = nonce.pushes,
            "pushed the nonce profile"
        );
        side.enrollments.audit(ACTION_NONCE_PUSHED, &reason);
        record.nonce = Some(nonce);
        self.kept(&record);
    }

    /// The nonce profile pushed in this enrollment, removed once.
    async fn remove_nonce(&self, side: &EnrollSide, mut record: DeviceRecord, now: u64) {
        let Some(nonce) = record.nonce.clone() else {
            return;
        };
        let due = nonce.enrollment == record.enrollment
            && nonce.removal.as_ref().is_none_or(|removal| {
                removal.answer.is_none() && !record.is_unanswered(&removal.uuid)
            });
        if !due {
            return;
        }
        self.forget_held(&record.enrollment_id);
        let Some([uuid]) = self.uuids(&mut record) else {
            return;
        };
        record.nonce = Some(NonceRecord {
            removal: Some(ProfileCommand::new(uuid.clone(), now)),
            ..nonce
        });
        let command = Command::remove_profile(uuid, &side.identifier.nonce());
        if self.dispatch(&mut record, &command, now).await {
            tracing::info!(enrollment_id = %record.enrollment_id, "removed the nonce profile: the Mac is approved");
        }
    }

    /// `N` new UUIDs; `None`, with the reason kept in `record`, when no random bytes could be
    /// had.
    fn uuids<const N: usize>(&self, record: &mut DeviceRecord) -> Option<[CommandUuid; N]> {
        let uuids: Option<Vec<CommandUuid>> = (0..N).map(|_| self.uuid()).collect();
        let uuids = uuids.and_then(|uuids| uuids.try_into().ok());
        if uuids.is_none() {
            record.last_error = Some("no random bytes for a profile command".to_owned());
            self.kept(record);
        }
        uuids
    }

    /// Stages `command` in `record`, keeps the record, and sends the command; whether NanoHUB
    /// queued it.
    async fn dispatch(&self, record: &mut DeviceRecord, command: &Command, now: u64) -> bool {
        stage(record, command.uuid(), command.request_type(), now);
        self.kept(record) && self.post(record, command).await
    }

    /// Sends the staged and kept `command`, and writes what NanoHUB said of it into `record`;
    /// whether NanoHUB queued it.
    async fn post(&self, record: &mut DeviceRecord, command: &Command) -> bool {
        let sent = self.send(&record.enrollment_id.clone(), command).await;
        self.settle(record, command, sent.as_ref().err());
        sent.is_ok()
    }

    /// Keeps `record`; a write that fails is logged, and `false`.
    fn kept(&self, record: &DeviceRecord) -> bool {
        match self.keep(record) {
            Ok(()) => true,
            Err(why) => {
                tracing::error!(enrollment_id = %record.enrollment_id, %why, "cannot write a device record");
                false
            }
        }
    }
}

/// Puts the command `uuid` of `kind`, sent at `now` and not yet queued, on `record`'s list.
fn stage(record: &mut DeviceRecord, uuid: &CommandUuid, kind: &str, now: u64) {
    record.sent.push(Sent {
        uuid: uuid.clone(),
        request_type: kind.to_owned(),
        sent_unix: now,
        queued: false,
    });
}
