//! The tables of the MDM server with a switch, for the tests: a call of the method named in
//! [`Flaky::fail`] fails with a store error, any other is the real table's.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, PoisonError};

use crate::store::mdm::files::FileMdm;
use crate::store::mdm::{
    AbmBatch, AbmDevice, AbmState, Answer, Answered, Challenge, Command as Row, Consumed, Enqueued,
    Enrollment, Identity, Issuance, MdmError, MdmStore, NewCommand,
};

pub(crate) struct Flaky {
    inner: FileMdm,
    failing: Mutex<Option<&'static str>>,
    not_live: std::sync::atomic::AtomicBool,
    /// Runs once on the tables just before the next `consume_challenge`: a competing request.
    race: Mutex<Option<RaceHook>>,
}

/// What a competing request does to the tables.
type RaceHook = Box<dyn FnOnce(&FileMdm) + Send>;

impl Flaky {
    pub(super) fn new(inner: FileMdm) -> Self {
        Self {
            inner,
            failing: Mutex::new(None),
            not_live: std::sync::atomic::AtomicBool::new(false),
            race: Mutex::new(None),
        }
    }

    /// Before the next `consume_challenge`, `competitor` runs on the tables.
    pub(crate) fn race_before_consume(&self, competitor: impl FnOnce(&FileMdm) + Send + 'static) {
        *self.race.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(competitor));
    }

    /// From now on the method `name` fails; `None` mends it.
    pub(crate) fn fail(&self, name: Option<&'static str>) {
        *self.failing.lock().unwrap_or_else(PoisonError::into_inner) = name;
    }

    /// From now on `set_push` finds the enrollment not live.
    pub(crate) fn end_enrollments_before_set_push(&self) {
        self.not_live.store(true, Ordering::SeqCst);
    }

    fn check(&self, name: &'static str) -> Result<(), MdmError> {
        match *self.failing.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(failing) if failing == name => Err(MdmError::Store(format!("{name} failed"))),
            _ => Ok(()),
        }
    }
}

macro_rules! delegate {
    ($(fn $name:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name(&self $(, $arg: $ty)*) -> $ret {
            self.check(stringify!($name))?;
            self.inner.$name($($arg),*)
        })*
    };
}

impl MdmStore for Flaky {
    fn set_push(
        &self,
        enrollment_id: &str,
        token: &[u8],
        magic: &str,
        topic: &str,
    ) -> Result<bool, MdmError> {
        self.check("set_push")?;
        if self.not_live.load(Ordering::SeqCst) {
            // The enrollment ended between the read and this write.
            return Ok(false);
        }
        self.inner.set_push(enrollment_id, token, magic, topic)
    }

    fn consume_challenge(
        &self,
        challenge_sha256: &str,
        key_sha256: &str,
        issuance: &Issuance,
        now_unix: i64,
    ) -> Result<Consumed, MdmError> {
        self.check("consume_challenge")?;
        let competitor = self
            .race
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(competitor) = competitor {
            competitor(&self.inner);
        }
        self.inner
            .consume_challenge(challenge_sha256, key_sha256, issuance, now_unix)
    }

    delegate! {
        fn replace_live(&self, new: &Enrollment) -> Result<Vec<String>, MdmError>;
        fn check_out(&self, enrollment_id: &str, now_unix: i64) -> Result<bool, MdmError>;
        fn record_push(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError>;
        fn record_contact(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError>;
        fn enrollment(&self, enrollment_id: &str) -> Result<Option<Enrollment>, MdmError>;
        fn live_enrollment(&self, serial: &str) -> Result<Option<Enrollment>, MdmError>;
        fn issue_challenge(&self, challenge: &Challenge) -> Result<(), MdmError>;
        fn challenge(&self, challenge_sha256: &str) -> Result<Option<Identity>, MdmError>;
        fn identity(&self, cert_sha256: &str) -> Result<Option<Identity>, MdmError>;
        fn kill_identity(&self, cert_sha256: &str) -> Result<bool, MdmError>;
        fn enqueue(&self, command: &NewCommand) -> Result<Enqueued, MdmError>;
        fn claim(&self, enrollment_id: &str, tried: &[String], now_unix: i64) -> Result<Option<Row>, MdmError>;
        fn answer(&self, enrollment_id: &str, id: &str, answer: Answer, response: Option<&[u8]>, now_unix: i64) -> Result<Answered, MdmError>;
        fn requeue_timed_out(&self, sent_before_unix: i64) -> Result<Vec<(String, String)>, MdmError>;
        fn command(&self, id: &str) -> Result<Option<Row>, MdmError>;
        fn commands(&self, enrollment_id: &str) -> Result<Vec<Row>, MdmError>;
        fn enrollments_with_queued(&self) -> Result<Vec<String>, MdmError>;
        fn identities_expiring(&self, before_unix: i64) -> Result<Vec<(String, i64)>, MdmError>;
        fn abm_apply(&self, batch: &AbmBatch) -> Result<(), MdmError>;
        fn abm_device(&self, serial: &str) -> Result<Option<AbmDevice>, MdmError>;
        fn abm_devices(&self) -> Result<Vec<AbmDevice>, MdmError>;
        fn abm_state(&self) -> Result<Option<AbmState>, MdmError>;
    }
}
