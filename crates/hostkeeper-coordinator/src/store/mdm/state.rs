//! The MDM tables as memory: the rules of [`MdmStore`](super::MdmStore) written once over maps.
//! [`FileMdm`](super::files::FileMdm) makes each call on a copy and writes the families that changed
//! before it keeps the copy.

use std::collections::BTreeMap;

use super::{
    AbmBatch, AbmDevice, AbmState, Answer, Answered, Challenge, Command, CommandState, Consumed,
    Enqueued, Enrollment, EnrollmentState, Identity, IdentityState, Issuance, MdmError, NewCommand,
    Refusal,
};

/// Everything the MDM tables hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MdmState {
    /// By enrollment id.
    pub(crate) enrollments: BTreeMap<String, Enrollment>,
    /// By the SHA-256 of the challenge.
    pub(crate) identities: BTreeMap<String, Identity>,
    /// By command id.
    pub(crate) commands: BTreeMap<String, Command>,
    /// By serial.
    pub(crate) abm_devices: BTreeMap<String, AbmDevice>,
    /// The sync state, once written.
    pub(crate) abm_state: Option<AbmState>,
}

/// What a request with the public key `key_sha256` gets of the challenge `row`; `row` is changed
/// only when the answer is [`Consumed::Issued`]. Both stores decide by this function.
pub(crate) fn consume(
    row: &mut Identity,
    key_sha256: &str,
    issuance: &Issuance,
    now_unix: i64,
) -> Consumed {
    match (row.state, &row.issued) {
        (IdentityState::Retired | IdentityState::Dead, _) => Consumed::Refused(Refusal::Ended),
        (IdentityState::Issued, Some(stored)) if row.key_sha256.as_deref() == Some(key_sha256) => {
            Consumed::Replayed(stored.clone())
        }
        (IdentityState::Issued, _) => Consumed::Refused(Refusal::KeyMismatch),
        (IdentityState::Challenged, _) if row.expires_unix <= now_unix => {
            Consumed::Refused(Refusal::Expired)
        }
        (IdentityState::Challenged, _) => {
            row.state = IdentityState::Issued;
            row.key_sha256 = Some(key_sha256.to_owned());
            row.issued = Some(issuance.clone());
            row.issued_unix = Some(now_unix);
            Consumed::Issued
        }
    }
}

impl MdmState {
    fn live_of(&self, serial: &str) -> Option<&Enrollment> {
        self.enrollments
            .values()
            .find(|e| e.serial == serial && e.state == EnrollmentState::Live)
    }

    fn next_seq(&self) -> i64 {
        self.commands.values().map(|c| c.seq).max().unwrap_or(0) + 1
    }

    /// Ends `enrollment_id` as `to`: its queued and sent commands are cancelled and its identity
    /// retired.
    fn end(&mut self, enrollment_id: &str, to: EnrollmentState, now_unix: i64) {
        let cert = self.enrollments.get_mut(enrollment_id).and_then(|e| {
            e.state = to;
            e.push_outstanding = false;
            e.cert_sha256.clone()
        });
        for command in self.commands.values_mut().filter(|c| {
            c.enrollment_id == enrollment_id
                && matches!(c.state, CommandState::Queued | CommandState::Sent)
        }) {
            command.state = CommandState::Cancelled;
            command.done_unix = Some(now_unix);
        }
        for identity in self.identities.values_mut().filter(|i| {
            i.issued.as_ref().map(|x| &x.cert_sha256) == cert.as_ref() && cert.is_some()
        }) {
            identity.state = IdentityState::Retired;
        }
    }

    pub(crate) fn replace_live(&mut self, new: &Enrollment) -> Result<Vec<String>, MdmError> {
        if self
            .enrollments
            .get(&new.enrollment_id)
            .is_some_and(|old| old.state != EnrollmentState::Live)
        {
            return Err(MdmError::EnrollmentEnded(new.enrollment_id.clone()));
        }
        let replaced: Vec<String> = self
            .enrollments
            .values()
            .filter(|e| {
                e.serial == new.serial
                    && e.state == EnrollmentState::Live
                    && e.enrollment_id != new.enrollment_id
            })
            .map(|e| e.enrollment_id.clone())
            .collect();
        for id in &replaced {
            self.end(id, EnrollmentState::Replaced, new.created_unix);
        }
        let mut row = new.clone();
        row.state = EnrollmentState::Live;
        self.enrollments.insert(row.enrollment_id.clone(), row);
        Ok(replaced)
    }

    pub(crate) fn check_out(&mut self, enrollment_id: &str, now_unix: i64) -> bool {
        let live = self
            .enrollments
            .get(enrollment_id)
            .is_some_and(|e| e.state == EnrollmentState::Live);
        if live {
            self.end(enrollment_id, EnrollmentState::CheckedOut, now_unix);
        }
        live
    }

    pub(crate) fn set_push(
        &mut self,
        enrollment_id: &str,
        token: &[u8],
        magic: &str,
        topic: &str,
    ) -> bool {
        match self.enrollments.get_mut(enrollment_id) {
            Some(e) if e.state == EnrollmentState::Live => {
                e.push_token = Some(token.to_vec());
                e.push_magic = Some(magic.to_owned());
                e.topic = Some(topic.to_owned());
                true
            }
            _ => false,
        }
    }

    pub(crate) fn record_push(&mut self, enrollment_id: &str, now_unix: i64) {
        if let Some(e) = self.enrollments.get_mut(enrollment_id) {
            e.push_outstanding = true;
            e.last_push_unix = Some(now_unix);
        }
    }

    pub(crate) fn record_contact(&mut self, enrollment_id: &str, now_unix: i64) {
        if let Some(e) = self.enrollments.get_mut(enrollment_id) {
            e.push_outstanding = false;
            e.last_seen_unix = Some(now_unix);
        }
    }

    pub(crate) fn live_enrollment(&self, serial: &str) -> Option<Enrollment> {
        self.live_of(serial).cloned()
    }

    pub(crate) fn issue_challenge(&mut self, challenge: &Challenge) -> Result<(), MdmError> {
        if self.identities.contains_key(&challenge.challenge_sha256) {
            return Err(MdmError::ChallengeExists);
        }
        self.identities.insert(
            challenge.challenge_sha256.clone(),
            Identity {
                challenge_sha256: challenge.challenge_sha256.clone(),
                serial: challenge.serial.clone(),
                udid: challenge.udid.clone(),
                expires_unix: challenge.expires_unix,
                confirmed: challenge.confirmed,
                state: IdentityState::Challenged,
                key_sha256: None,
                issued: None,
                issued_unix: None,
            },
        );
        Ok(())
    }

    pub(crate) fn consume_challenge(
        &mut self,
        challenge_sha256: &str,
        key_sha256: &str,
        issuance: &Issuance,
        now_unix: i64,
    ) -> Consumed {
        match self.identities.get_mut(challenge_sha256) {
            Some(row) => consume(row, key_sha256, issuance, now_unix),
            None => Consumed::Refused(Refusal::Unknown),
        }
    }

    pub(crate) fn identity(&self, cert_sha256: &str) -> Option<Identity> {
        self.identities
            .values()
            .find(|i| {
                i.issued
                    .as_ref()
                    .is_some_and(|x| x.cert_sha256 == cert_sha256)
            })
            .cloned()
    }

    pub(crate) fn kill_identity(&mut self, cert_sha256: &str) -> bool {
        let found = self.identities.values_mut().find(|i| {
            i.issued
                .as_ref()
                .is_some_and(|x| x.cert_sha256 == cert_sha256)
        });
        found.map(|i| i.state = IdentityState::Dead).is_some()
    }

    pub(crate) fn enqueue(&mut self, command: &NewCommand) -> Enqueued {
        if self.commands.contains_key(&command.id) {
            return Enqueued::Exists;
        }
        let live = self
            .enrollments
            .get(&command.enrollment_id)
            .is_some_and(|e| e.state == EnrollmentState::Live);
        if !live {
            return Enqueued::NotLive;
        }
        let seq = self.next_seq();
        self.commands.insert(
            command.id.clone(),
            Command {
                id: command.id.clone(),
                seq,
                enrollment_id: command.enrollment_id.clone(),
                command_type: command.command_type.clone(),
                payload: command.payload.clone(),
                state: CommandState::Queued,
                deferred: false,
                deliveries: 0,
                created_unix: command.created_unix,
                sent_unix: None,
                done_unix: None,
                response: None,
            },
        );
        Enqueued::Queued
    }

    pub(crate) fn claim(
        &mut self,
        enrollment_id: &str,
        tried: &[String],
        now_unix: i64,
    ) -> Option<Command> {
        let live = self
            .enrollments
            .get(enrollment_id)
            .is_some_and(|e| e.state == EnrollmentState::Live);
        if !live {
            return None;
        }
        let command = self
            .commands
            .values_mut()
            .filter(|c| {
                c.enrollment_id == enrollment_id
                    && c.state == CommandState::Queued
                    && !tried.contains(&c.id)
            })
            .min_by_key(|c| c.seq)?;
        command.state = CommandState::Sent;
        command.deferred = false;
        command.deliveries += 1;
        command.sent_unix = Some(now_unix);
        Some(command.clone())
    }

    pub(crate) fn answer(
        &mut self,
        enrollment_id: &str,
        id: &str,
        answer: Answer,
        response: Option<&[u8]>,
        now_unix: i64,
    ) -> Answered {
        let Some(command) = self
            .commands
            .get_mut(id)
            .filter(|c| c.enrollment_id == enrollment_id && c.state == CommandState::Sent)
        else {
            return Answered::Dropped;
        };
        command.response = response.map(<[u8]>::to_vec);
        match answer {
            Answer::Acknowledged | Answer::Error => {
                command.state = if answer == Answer::Acknowledged {
                    CommandState::Acked
                } else {
                    CommandState::Errored
                };
                command.done_unix = Some(now_unix);
            }
            Answer::NotNow => {
                command.state = CommandState::Queued;
                command.deferred = true;
            }
        }
        Answered::Taken
    }

    pub(crate) fn requeue_timed_out(&mut self, sent_before_unix: i64) -> Vec<(String, String)> {
        let mut back: Vec<&mut Command> = self
            .commands
            .values_mut()
            .filter(|c| {
                c.state == CommandState::Sent
                    && c.sent_unix.is_some_and(|sent| sent <= sent_before_unix)
            })
            .collect();
        back.sort_by_key(|c| c.seq);
        back.into_iter()
            .map(|c| {
                c.state = CommandState::Queued;
                c.deferred = false;
                (c.id.clone(), c.enrollment_id.clone())
            })
            .collect()
    }

    pub(crate) fn commands_of(&self, enrollment_id: &str) -> Vec<Command> {
        let mut all: Vec<Command> = self
            .commands
            .values()
            .filter(|c| c.enrollment_id == enrollment_id)
            .cloned()
            .collect();
        all.sort_by_key(|c| c.seq);
        all
    }

    pub(crate) fn abm_apply(&mut self, batch: &AbmBatch) {
        for device in &batch.upserts {
            let first_seen_unix = self
                .abm_devices
                .get(&device.serial)
                .map_or(device.first_seen_unix, |old| old.first_seen_unix);
            self.abm_devices.insert(
                device.serial.clone(),
                AbmDevice {
                    first_seen_unix,
                    removed_unix: None,
                    ..device.clone()
                },
            );
        }
        for serial in &batch.removed {
            if let Some(device) = self.abm_devices.get_mut(serial) {
                device.removed_unix = Some(batch.removed_unix);
            }
        }
        if let Some(state) = &batch.state {
            self.abm_state = Some(state.clone());
        }
    }
}
