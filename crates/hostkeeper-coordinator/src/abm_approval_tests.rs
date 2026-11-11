//! The ABM side of the Mac path over the coordinator's real parts: the table the sync keeps and
//! the settings of the setup, wired to the enrollment in whichever order the builders run.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use crate::abm::Abm;
use crate::abm::fake::{FakeAbm, Sim, client_clock, counting_random, recording_sleep};
use crate::enrollment::tests::support::{HostKey, SERIAL, mac_request};
use crate::enrollment::{ConfirmRefusal, DeviceStatus, FakeDeviceState, MacDecline};
use crate::host_groups_support::{SELF, body, coordinator_on, put_of};
use crate::marks_support::operator;
use crate::mdm_setup::MdmSetup;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::store::mdm::files::FileMdm;
use crate::store::mdm::{
    AbmBatch, AbmDevice, AbmState, Answer, Answered, Challenge, Command, Consumed, Enqueued,
    Enrollment as MdmEnrollment, Identity, Issuance, MdmError, MdmStore, NewCommand,
};
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};
use crate::testutil::{Rig, scratch};

fn device(serial: &str) -> AbmDevice {
    AbmDevice {
        serial: serial.to_owned(),
        model: None,
        description: None,
        profile_status: "pushed".to_owned(),
        profile_uuid: None,
        first_seen_unix: 1,
        synced_unix: 1,
        removed_unix: None,
    }
}

/// A coordinator holding the group `build` with the real ABM client over `store` and the setup
/// `setup`, linked to `enrollment` first (`enrollment_first`) or last.
fn coordinator(
    rig: &Rig,
    enrollment: &Arc<crate::enrollment::Enrollment>,
    store: &Arc<FileMdm>,
    setup: MdmSetup,
    enrollment_first: bool,
    abm: bool,
) -> Coordinator {
    let client = Abm::new(
        Arc::clone(store) as _,
        FakeAbm::new(Sim::default().handler()) as _,
        client_clock(),
        counting_random(),
        recording_sleep().0,
    );
    let first = coordinator_on(
        &Fake::new(),
        &rig.clock,
        enrollment_first.then(|| Arc::clone(enrollment)),
    );
    let first = first.with_mdm_setup(setup);
    let linked = if abm { first.with_abm(client) } else { first };
    if enrollment_first {
        linked
    } else {
        linked.with_enrollment(Arc::clone(enrollment))
    }
}

/// Proves: with the enrollment linked before or after the ABM client and the setup, a Mac in the
/// table is approved with the host id the setting `host_id_prefix` gives and the group the setting
/// `default_group` names; the same Mac removed from the table is confirmed no more and declined
/// `serial_not_in_abm`; a serial never in the table is declined; and a coordinator with no ABM
/// client declines it `serial_not_allowed`.
///
/// Catches: a link that only works in one order of the builders, settings that are not read, and a
/// removal in the sync that the approval does not see.
#[tokio::test]
async fn the_coordinators_table_and_settings_decide_the_approval_in_either_order() {
    for (n, (enrollment_first, abm)) in [(true, true), (false, true), (true, false)]
        .into_iter()
        .enumerate()
    {
        let name = format!("abm-approval-{n}");
        let gate = Arc::new(FakeDeviceState::default());
        gate.set(
            SERIAL,
            Ok(DeviceStatus {
                enrolled: true,
                supervised: true,
                enrolled_since_unix: None,
                enrolled_via_dep: Some(true),
            }),
        );
        let mac = crate::enrollment::tests::support::Rig::configured(
            &format!("{name}-enrollment"),
            Arc::new(RealFs),
            crate::enrollment::os_random(),
            |e| {
                e.with_mac(crate::enrollment::MacPolicy {
                    device_state: gate,
                    allowlist: crate::enrollment::MacAllowlist::default(),
                })
            },
        );
        let dir = scratch(&format!("{name}-tables"));
        let store = Arc::new(FileMdm::open(&dir.path().join("mdm")).expect("the tables"));
        store
            .abm_apply(&AbmBatch {
                upserts: vec![device(SERIAL)],
                ..AbmBatch::default()
            })
            .expect("a row");
        let setup = MdmSetup::volatile();
        setup.set_setting("default_group", "build").expect("set");
        setup.set_setting("host_id_prefix", "lab-").expect("set");
        let mut rig = Rig::new(&name);
        rig.coordinator = coordinator(&rig, &mac.enrollment, &store, setup, enrollment_first, abm);
        rig.at(2_000);
        rig.coordinator
            .host_groups_put(&operator(), put_of(body("build", &[SELF], None), None))
            .expect("put");
        let e = &mac.enrollment;
        let key = HostKey::new();
        let nonce = e.issue_mac_nonce(SERIAL, &key.key_hash()).expect("a nonce");
        e.enroll(mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes()))
            .expect("enrolled");
        if !abm {
            assert_eq!(
                e.mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
                crate::enrollment::MacOutcome::Declined(MacDecline::SerialNotAllowed)
            );
            assert_eq!(
                e.confirm_in_abm(SERIAL),
                Err(ConfirmRefusal::Declined(MacDecline::SerialNotInAbm))
            );
            continue;
        }
        let approved = e.store().recorded_approvals().expect("read");
        assert_eq!(approved.len(), 1, "{n}");
        assert_eq!(
            (
                approved[0].host_id.as_str(),
                approved[0].host_group.as_str()
            ),
            ("lab-c02xk1abjg5j", "build")
        );
        assert_eq!(e.confirm_in_abm(SERIAL), Ok(()));
        store
            .abm_apply(&AbmBatch {
                removed: vec![SERIAL.to_owned()],
                removed_unix: 5,
                ..AbmBatch::default()
            })
            .expect("removed");
        assert_eq!(
            e.confirm_in_abm(SERIAL),
            Err(ConfirmRefusal::Declined(MacDecline::SerialNotInAbm))
        );
        let second = HostKey::new();
        let again = e
            .issue_mac_nonce(SERIAL, &second.key_hash())
            .expect("a nonce");
        e.enroll(mac_request(
            &second,
            "machine-two",
            SERIAL,
            again.as_bytes(),
        ))
        .expect("enrolled");
        let state = e
            .store()
            .request(&second.key_hash())
            .expect("read")
            .expect("held")
            .state;
        assert_eq!(state, RequestState::Pending, "{n}");
    }
}

/// A table that answers every question about a serial with an error, over the files store.
struct Unreadable(Arc<FileMdm>);

macro_rules! delegate {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)+) => {
        $(fn $name(&self, $($arg: $ty),*) -> $ret { self.0.$name($($arg),*) })+
    };
}

impl MdmStore for Unreadable {
    fn abm_device(&self, _serial: &str) -> Result<Option<AbmDevice>, MdmError> {
        Err(MdmError::Store("the table is unreadable".to_owned()))
    }

    fn claim(&self, id: &str, tried: &[String], now: i64) -> Result<Option<Command>, MdmError> {
        self.0.claim(id, tried, now)
    }

    fn answer(
        &self,
        enrollment_id: &str,
        id: &str,
        answer: Answer,
        response: Option<&[u8]>,
        now: i64,
    ) -> Result<Answered, MdmError> {
        self.0.answer(enrollment_id, id, answer, response, now)
    }

    fn consume_challenge(
        &self,
        hash: &str,
        key: &str,
        issuance: &Issuance,
        now: i64,
    ) -> Result<Consumed, MdmError> {
        self.0.consume_challenge(hash, key, issuance, now)
    }

    fn requeue_timed_out(&self, before: i64) -> Result<Vec<(String, String)>, MdmError> {
        self.0.requeue_timed_out(before)
    }

    delegate! {
        replace_live(new: &MdmEnrollment) -> Result<Vec<String>, MdmError>;
        check_out(id: &str, now: i64) -> Result<bool, MdmError>;
        set_push(id: &str, token: &[u8], magic: &str, topic: &str) -> Result<bool, MdmError>;
        record_push(id: &str, now: i64) -> Result<(), MdmError>;
        record_contact(id: &str, now: i64) -> Result<(), MdmError>;
        enrollment(id: &str) -> Result<Option<MdmEnrollment>, MdmError>;
        live_enrollment(serial: &str) -> Result<Option<MdmEnrollment>, MdmError>;
        issue_challenge(c: &Challenge) -> Result<(), MdmError>;
        challenge(h: &str) -> Result<Option<Identity>, MdmError>;
        identity(c: &str) -> Result<Option<Identity>, MdmError>;
        kill_identity(c: &str) -> Result<bool, MdmError>;
        enqueue(c: &NewCommand) -> Result<Enqueued, MdmError>;
        command(id: &str) -> Result<Option<Command>, MdmError>;
        commands(id: &str) -> Result<Vec<Command>, MdmError>;
        abm_apply(b: &AbmBatch) -> Result<(), MdmError>;
        abm_devices() -> Result<Vec<AbmDevice>, MdmError>;
        abm_state() -> Result<Option<AbmState>, MdmError>;
    }
}

/// Proves: a table that cannot be read and a settings store that cannot be read each come out of
/// the coordinator's ABM side as an error and not as a no: the confirmation says `Unread`, and a
/// Mac's approval is refused `store_failed` and stays pending.
///
/// Catches: a store fault read as "the serial is not in ABM", or as "it is".
#[tokio::test]
async fn an_unreadable_table_or_settings_store_is_an_error_of_the_abm_side() {
    for (n, table) in [true, false].into_iter().enumerate() {
        let name = format!("abm-approval-unreadable-{n}");
        let gate = Arc::new(FakeDeviceState::default());
        gate.set(
            SERIAL,
            Ok(DeviceStatus {
                enrolled: true,
                supervised: true,
                enrolled_since_unix: None,
                enrolled_via_dep: Some(true),
            }),
        );
        let mac = crate::enrollment::tests::support::Rig::configured(
            &format!("{name}-enrollment"),
            Arc::new(RealFs),
            crate::enrollment::os_random(),
            |e| {
                e.with_mac(crate::enrollment::MacPolicy {
                    device_state: gate,
                    allowlist: crate::enrollment::MacAllowlist::default(),
                })
            },
        );
        let dir = scratch(&format!("{name}-tables"));
        let files = Arc::new(FileMdm::open(&dir.path().join("mdm")).expect("the tables"));
        files
            .abm_apply(&AbmBatch {
                upserts: vec![device(SERIAL)],
                ..AbmBatch::default()
            })
            .expect("a row");
        let (store, setup): (Arc<FileMdm>, MdmSetup) = (Arc::clone(&files), MdmSetup::volatile());
        let (client_store, setup) = if table {
            (Arc::new(Unreadable(store)) as Arc<dyn MdmStore>, setup)
        } else {
            let broken = MdmSetup::new(
                Arc::new(UnreadableSettings(FileSetup::volatile())),
                None,
                crate::enrollment::os_random(),
            );
            (store as Arc<dyn MdmStore>, broken)
        };
        let client = Abm::new(
            client_store,
            FakeAbm::new(Sim::default().handler()) as _,
            client_clock(),
            counting_random(),
            recording_sleep().0,
        );
        let mut rig = Rig::new(&name);
        rig.coordinator =
            coordinator_on(&Fake::new(), &rig.clock, Some(Arc::clone(&mac.enrollment)))
                .with_mdm_setup(setup)
                .with_abm(client);
        rig.at(2_000);
        let e = &mac.enrollment;
        let key = HostKey::new();
        let nonce = e.issue_mac_nonce(SERIAL, &key.key_hash()).expect("a nonce");
        e.enroll(mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes()))
            .expect("enrolled");
        assert_eq!(
            e.mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
            crate::enrollment::MacOutcome::Refused(EnrollmentRefusal::StoreFailed),
            "table {table}"
        );
        if table {
            assert_eq!(e.confirm_in_abm(SERIAL), Err(ConfirmRefusal::Unread));
        }
        assert!(e.store().recorded_approvals().expect("read").is_empty());
    }
}

/// A settings store whose reads fail.
struct UnreadableSettings(FileSetup);

impl SetupStore for UnreadableSettings {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        self.0.put_secret(row)
    }

    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        self.0.secrets()
    }

    fn prune_superseded(&self, superseded_unix: i64) -> Result<u64, SetupError> {
        self.0.prune_superseded(superseded_unix)
    }

    fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
        Err(SetupError::Store("the settings are unreadable".to_owned()))
    }

    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.0.set_setting(name, value)
    }
}
