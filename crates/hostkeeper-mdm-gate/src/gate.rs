//! The gate's device side: it hears NanoHUB's webhook, keeps a record per device, asks each
//! newly enrolled device `DeviceInformation` and `SecurityInfo`, and says what it knows of a
//! Mac serial (design 6.1).
//!
//! **How an event is handled.** One event at a time (later ones wait their turn), so two
//! events of one device never both send it the same query. The record the event makes is
//! written before the gate's memory takes it, and before any command is sent: a command is
//! written as sent, with its UUID, before NanoHUB is asked to queue it, so its result is
//! matched even when it comes back before the queue call returns, or after a restart; once
//! NanoHUB says it queued it, the command is written as queued. A queue call that fails takes
//! the command off again and keeps NanoHUB's reason as the record's last error; the query is
//! asked again at the device's next check-in. A command never written as queued (the
//! coordinator stopped while asking, or a write failed) is dropped at the device's next event,
//! and its query is asked again then.
//!
//! **What a result is.** A result is read only when its command UUID is one the gate sent to
//! that device and the device has not answered: NanoHUB 0.2.0's webhook carries no signature,
//! so a result the gate did not ask for is passed over. `Acknowledged` is read as the
//! command's answer; `Error` and `CommandFormatError` are kept as the last error, and the query
//! is asked again at the device's next `TokenUpdate`, never sooner (a device that answers an
//! error is not asked in a loop); `NotNow` leaves the command unanswered, as the device sends
//! it again itself; an `Idle` asks any query that is due (one the device refused is not, until its next
//! `TokenUpdate`).
//!
//! **What the gate says of a serial.** [`Gate::status`] answers from the records, never by
//! asking the device. The serial is the one a `DeviceInformation` answer of the current
//! enrollment gave: an `Authenticate`'s serial alone is not enough. A serial no record has is
//! unknown; a serial two records have, neither checked out, is ambiguous until the older one
//! checks out. A device is supervised only when its answer said `IsSupervised` true.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::validity::UnixClock;

use crate::answer;
use crate::command::{Command, DEVICE_INFORMATION, SECURITY_INFO};
use crate::event::{self, Event, EventError};
use crate::ids::{CommandUuid, EnrollmentId, shown};
use crate::nanohub::{EnqueueError, NanoHub};
use crate::record::{DeviceRecord, Phase, Sent};
use crate::store::{DeviceStore, StoreError};

/// Fills a buffer with random bytes; `false` when it could not. Command UUIDs are drawn from it.
pub type Random = Box<dyn Fn(&mut [u8]) -> bool + Send + Sync>;

/// What the gate says of a device: the coordinator's condition is both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceStatus {
    /// Enrolled: a `TokenUpdate` and no `CheckOut` since.
    pub enrolled: bool,
    /// Its `DeviceInformation` answer of this enrollment said `IsSupervised` true.
    pub supervised: bool,
    /// When its current enrollment began: its first `TokenUpdate` after an `Authenticate` or a
    /// `CheckOut`, by the gate's clock; `None` before one.
    pub enrolled_since_unix: Option<u64>,
}

/// Why the gate says nothing of a serial.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unavailable {
    /// No record has the serial from a `DeviceInformation` answer.
    #[error("the gate knows no device with this serial")]
    Unknown,
    /// Two enrollments that have not checked out have the serial.
    #[error("the serial is the device of more than one enrollment")]
    Ambiguous,
}

/// Why a webhook event was not taken.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The body is not an event the gate reads.
    #[error(transparent)]
    Event(#[from] EventError),
    /// The device's record could not be written: the event is not taken, and NanoHUB is told.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What became of a webhook event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// It changed a device's record.
    Recorded,
    /// The gate passes it over: another topic, a user channel, a result of a command the gate
    /// did not send, or an event of a device it has no record of that begins nothing.
    PassedOver,
}

/// The gate's device side.
pub struct Gate {
    store: Box<dyn DeviceStore>,
    nanohub: NanoHub,
    clock: Arc<dyn UnixClock>,
    random: Random,
    devices: Mutex<BTreeMap<EnrollmentId, DeviceRecord>>,
    turn: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gate")
            .field("nanohub", &self.nanohub)
            .finish_non_exhaustive()
    }
}

impl Gate {
    /// The gate over the records of `store`, queueing commands through `nanohub`, reading the
    /// time from `clock` and drawing command UUIDs from `random`.
    ///
    /// # Errors
    /// [`StoreError`]: a record cannot be read or is not whole; the gate does not start.
    pub fn open(
        store: Box<dyn DeviceStore>,
        nanohub: NanoHub,
        clock: Arc<dyn UnixClock>,
        random: Random,
    ) -> Result<Self, StoreError> {
        let devices = store
            .load()?
            .into_iter()
            .map(|record| (record.enrollment_id.clone(), record))
            .collect();
        Ok(Self {
            store,
            nanohub,
            clock,
            random,
            devices: Mutex::new(devices),
            turn: tokio::sync::Mutex::new(()),
        })
    }

    fn devices(&self) -> std::sync::MutexGuard<'_, BTreeMap<EnrollmentId, DeviceRecord>> {
        self.devices.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every device record, by enrollment id.
    #[must_use]
    pub fn records(&self) -> Vec<DeviceRecord> {
        self.devices().values().cloned().collect()
    }

    /// What the gate says of the device whose `DeviceInformation` answer gave `serial`.
    ///
    /// # Errors
    /// [`Unavailable`]: no record has the serial, or two that have not checked out do.
    pub fn status(&self, serial: &str) -> Result<DeviceStatus, Unavailable> {
        let devices = self.devices();
        let mut matches = devices
            .values()
            .filter(|record| record.serial.as_deref() == Some(serial));
        let mut live = matches
            .clone()
            .filter(|record| record.phase != Phase::CheckedOut);
        let record = match (live.next(), live.next()) {
            (Some(_), Some(_)) => return Err(Unavailable::Ambiguous),
            (Some(record), None) => record,
            (None, _) => matches.next().ok_or(Unavailable::Unknown)?,
        };
        Ok(DeviceStatus {
            enrolled: record.is_enrolled(),
            supervised: record.supervised == Some(true),
            enrolled_since_unix: record.enrolled_since_unix,
        })
    }

    /// Takes one webhook body.
    ///
    /// # Errors
    /// [`WebhookError`]: the body is no event, or the record cannot be written.
    pub async fn webhook(&self, body: &[u8]) -> Result<Handled, WebhookError> {
        let Some(event) = event::parse(body)? else {
            return Ok(Handled::PassedOver);
        };
        let _turn = self.turn.lock().await;
        let now = self.clock.now_unix();
        let id = id_of(&event).clone();
        let known = self.devices().get(&id).cloned();
        // Commands NanoHUB never confirmed are dropped; when there were any, the record changed
        // and is written, whatever the event says, so their queries are asked again now.
        let settled = known.clone().map(|mut record| {
            record.drop_unqueued();
            record
        });
        let changed = settled
            .as_ref()
            .filter(|&record| Some(record) != known.as_ref())
            .cloned();
        let Some(mut record) = apply(settled, event, now).or(changed) else {
            return Ok(Handled::PassedOver);
        };
        let mut commands = Vec::new();
        for kind in record.due() {
            let Some(uuid) = self.uuid() else {
                record.last_error = Some(format!("{kind}: no random bytes for a command UUID"));
                break;
            };
            record.sent.push(Sent {
                uuid: uuid.clone(),
                request_type: kind.to_owned(),
                sent_unix: now,
                queued: false,
            });
            commands.push(command_of(kind, uuid));
        }
        self.keep(&record)?;
        for command in commands {
            let sent = self.send(&id, &command).await;
            self.settle(&mut record, &command, sent.err().as_ref());
        }
        Ok(Handled::Recorded)
    }

    /// Writes `record` and then takes it into memory.
    fn keep(&self, record: &DeviceRecord) -> Result<(), StoreError> {
        self.store.save(record)?;
        self.devices()
            .insert(record.enrollment_id.clone(), record.clone());
        Ok(())
    }

    /// Queues `command` for `id`; the error is NanoHUB's reason for not queueing it.
    async fn send(&self, id: &EnrollmentId, command: &Command) -> Result<(), EnqueueError> {
        let queued = self.nanohub.enqueue(id, command).await.inspect_err(|why| {
            tracing::warn!(enrollment_id = %id, command = command.request_type(), %why,
                "NanoHUB did not queue the command; it is asked again at the device's next check-in");
        })?;
        if let Some(push) = queued.push_error {
            tracing::warn!(enrollment_id = %id, command = command.request_type(), %push,
                "NanoHUB queued the command, and could not push the device; it fetches the command at its next check-in");
        }
        Ok(())
    }

    /// What NanoHUB said of `command`, written into `record`: queued (`why` is `None`), or not
    /// queued for `why`, when it is taken off again with the reason kept, so it is asked again
    /// at the next check-in. `record` changes once the new one is written; a write that fails
    /// is logged, and the command, not written as queued, is dropped at the next event.
    fn settle(&self, record: &mut DeviceRecord, command: &Command, why: Option<&EnqueueError>) {
        let mut taken = record.clone();
        match why {
            None => taken
                .sent
                .iter_mut()
                .filter(|sent| sent.uuid == *command.uuid())
                .for_each(|sent| sent.queued = true),
            Some(why) => {
                taken.take_sent(command.uuid().as_str());
                taken.last_error = Some(shown(&format!("{}: {why}", command.request_type())));
            }
        }
        match self.keep(&taken) {
            Ok(()) => *record = taken,
            Err(why) => {
                tracing::error!(enrollment_id = %record.enrollment_id, %why, "cannot write a device record");
            }
        }
    }

    /// A new command UUID; `None` when no random bytes could be had.
    fn uuid(&self) -> Option<CommandUuid> {
        let mut bytes = [0; 16];
        (self.random)(&mut bytes).then(|| CommandUuid::from_random(bytes))
    }
}

fn id_of(event: &Event) -> &EnrollmentId {
    match event {
        Event::Authenticate { id, .. }
        | Event::TokenUpdate { id, .. }
        | Event::CheckOut { id }
        | Event::Result { id, .. } => id,
    }
}

fn command_of(kind: &str, uuid: CommandUuid) -> Command {
    if kind == DEVICE_INFORMATION {
        Command::device_information(uuid)
    } else {
        Command::security_info(uuid)
    }
}

/// The record `event` makes of `known` (the device's record, if the gate has one) at `now`;
/// `None` when the event changes nothing and sends nothing.
fn apply(known: Option<DeviceRecord>, event: Event, now: u64) -> Option<DeviceRecord> {
    let begins = matches!(
        event,
        Event::Authenticate { .. } | Event::TokenUpdate { .. }
    );
    let mut record = match known {
        Some(record) => record,
        None if begins => DeviceRecord::new(id_of(&event).clone(), now),
        None => return None,
    };
    match event {
        Event::Authenticate { udid, serial, .. } => record.authenticate(&udid, serial, now),
        Event::TokenUpdate { udid, tally, .. } => record.token_update(&udid, tally, now),
        Event::CheckOut { .. } => record.check_out(now),
        Event::Result {
            command_uuid,
            status,
            payload,
            ..
        } => return result(record, command_uuid, &status, &payload, now),
    }
    Some(record)
}

/// The record a command result makes; `None` when it changes nothing.
fn result(
    mut record: DeviceRecord,
    uuid: Option<String>,
    status: &str,
    payload: &[u8],
    now: u64,
) -> Option<DeviceRecord> {
    let Some(uuid) = uuid else {
        // `Idle`: the device fetched its queue and it is empty; a query due is sent now.
        return (status == "Idle" && !record.due().is_empty()).then_some(record);
    };
    let sent = record
        .sent
        .iter()
        .find(|sent| sent.uuid.as_str() == uuid)?
        .clone();
    match status {
        "Acknowledged" => {
            record.take_sent(&uuid);
            match sent.request_type.as_str() {
                DEVICE_INFORMATION => record
                    .answer_device_information(answer::device_information(payload, &uuid), now),
                SECURITY_INFO => {
                    record.answer_security_info(answer::security_info(payload, &uuid), now);
                }
                // A command of the gate's enrollment side, which this release does not send.
                _ => {}
            }
        }
        "Error" | "CommandFormatError" => {
            record.take_sent(&uuid);
            record.refuse(&sent.request_type, &format!("the device answered {status}"));
        }
        // `NotNow`, or a status this release does not know: the command stays unanswered.
        _ => return None,
    }
    Some(record)
}
