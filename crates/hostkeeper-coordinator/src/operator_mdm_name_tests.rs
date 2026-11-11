//! `Mdm.PutName`: what it keeps, refuses and audits, and that the name is no setting.

use std::sync::{Arc, Mutex};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use crate::login_support::World;
use crate::mdm_secrets::MasterKey;
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::store::audit_failure;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

fn name_of(name: &str) -> proto::PutNameRequest {
    proto::PutNameRequest {
        name: name.to_owned(),
    }
}

fn world_over(store: Arc<dyn SetupStore>, label: &str) -> World {
    World::build(
        label,
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| {
            c.with_mdm_setup(MdmSetup::new(
                store,
                Some(MasterKey::from_bytes(&[0x21; 32]).expect("a key")),
                crate::enrollment::os_random(),
            ))
        },
    )
}

fn audit(world: &World) -> Vec<(String, String, Option<String>, Option<String>)> {
    world
        .audit()
        .into_iter()
        .map(|entry| (entry.by, entry.action, entry.subject, entry.reason))
        .collect()
}

/// Proves: a name is kept in lower case and answered with its enrollment URL, is audited under the
/// user's name as `mdm_name_set` with the name, is the status's name, and is replaced by another
/// before any Mac has enrolled.
#[test]
fn a_name_is_kept_audited_and_shown_by_the_status() {
    let world = world_over(Arc::new(FileSetup::volatile()), "mdm-name-set");
    let answer = world
        .coordinator
        .mdm_put_name(&alice(), &name_of("  MDM.Internal.Example.COM "))
        .expect("named");
    assert_eq!(
        answer,
        proto::PutNameResponse {
            name: "mdm.internal.example.com".to_owned(),
            enrollment_url: "https://mdm.internal.example.com/enroll".to_owned(),
        }
    );
    let status = world.coordinator.mdm_status(&alice()).expect("a status");
    assert_eq!(
        (
            status.name.as_str(),
            status.enrollment_url.as_str(),
            status.name_locked
        ),
        (
            "mdm.internal.example.com",
            "https://mdm.internal.example.com/enroll",
            false
        )
    );
    world
        .coordinator
        .mdm_put_name(&alice(), &name_of("mdm2.example.com"))
        .expect("replaced before a Mac enrolled");
    assert_eq!(
        world
            .coordinator
            .mdm_status(&alice())
            .expect("a status")
            .name,
        "mdm2.example.com"
    );
    assert_eq!(
        audit(&world),
        [
            ("alice", "mdm_name_set", "set to mdm.internal.example.com"),
            ("alice", "mdm_name_set", "set to mdm2.example.com"),
        ]
        .map(|(by, action, reason)| (
            by.to_owned(),
            action.to_owned(),
            None,
            Some(reason.to_owned())
        ))
    );
}

/// Proves: a name that is not DNS-safe is refused `invalid_request`, audited with that reason,
/// and nothing is kept; and the name is no setting: `PutSetting` refuses its key and
/// `ListSettings` does not list it.
#[test]
fn a_name_that_is_not_dns_safe_is_refused_and_the_name_is_no_setting() {
    let world = world_over(Arc::new(FileSetup::volatile()), "mdm-name-invalid");
    assert_eq!(
        world
            .coordinator
            .mdm_put_name(&alice(), &name_of("not a name")),
        Err(MdmRefusal::InvalidRequest)
    );
    assert_eq!(
        world
            .coordinator
            .mdm_status(&alice())
            .expect("a status")
            .name,
        ""
    );
    assert_eq!(
        audit(&world),
        [(
            "alice".to_owned(),
            "mdm_name_set".to_owned(),
            None,
            Some("refused: MDM_REFUSAL_INVALID_REQUEST".to_owned())
        )]
    );
    world
        .coordinator
        .mdm_put_name(&alice(), &name_of("mdm.example.com"))
        .expect("named");
    assert_eq!(
        world.coordinator.mdm_put_setting(
            &alice(),
            &proto::PutSettingRequest {
                name: "mdm_name".to_owned(),
                value: "evil.example.com".to_owned(),
            }
        ),
        Err(MdmRefusal::InvalidRequest)
    );
    let listed = world
        .coordinator
        .mdm_list_settings(&alice())
        .expect("listed");
    assert!(
        listed
            .settings
            .iter()
            .all(|setting| setting.name != "mdm_name")
    );
    assert_eq!(
        world
            .coordinator
            .mdm_status(&alice())
            .expect("a status")
            .name,
        "mdm.example.com"
    );
}

/// Proves: only the operator calls it, as for every operator call.
#[test]
fn only_an_operator_sets_the_name() {
    let world = world_over(Arc::new(FileSetup::volatile()), "mdm-name-callers");
    assert_eq!(
        world
            .coordinator
            .mdm_put_name(&Caller::Anonymous, &name_of("mdm.example.com")),
        Err(MdmRefusal::Unauthorized)
    );
    assert_eq!(
        world
            .coordinator
            .mdm_put_name(&Caller::Ci, &name_of("mdm.example.com")),
        Err(MdmRefusal::Forbidden)
    );
    assert!(audit(&world).is_empty());
}

/// Proves: when the audit entry cannot be written the name is not set and the call is refused
/// `audit_unavailable`, a refusal that cannot be audited is the audit's refusal too, and a store
/// that cannot write refuses `store_failed` with the entry of the attempt and one of its refusal,
/// and a store that cannot read refuses `store_failed` before anything is audited.
#[test]
fn a_name_without_its_audit_is_not_set_and_a_failed_store_is_told() {
    let world = world_over(Arc::new(FileSetup::volatile()), "mdm-name-audit-fails");
    world.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(
        world
            .coordinator
            .mdm_put_name(&alice(), &name_of("mdm.example.com")),
        Err(MdmRefusal::AuditUnavailable)
    );
    assert_eq!(
        world
            .coordinator
            .mdm_put_name(&alice(), &name_of("not a name")),
        Err(MdmRefusal::AuditUnavailable)
    );
    world.fake.refuse(|_| None);
    assert_eq!(
        world
            .coordinator
            .mdm_status(&alice())
            .expect("a status")
            .name,
        ""
    );

    /// A store that cannot write a setting, and cannot read when told.
    struct Broken(FileSetup, Mutex<bool>);
    impl SetupStore for Broken {
        fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
            self.0.put_secret(row)
        }
        fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
            self.0.secrets()
        }
        fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
            self.0.prune_superseded(at)
        }
        fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
            if *self.1.lock().expect("a lock") {
                Err(SetupError::Store("unreadable".to_owned()))
            } else {
                self.0.settings()
            }
        }
        fn set_setting(&self, _: &str, _: &str) -> Result<(), SetupError> {
            Err(SetupError::Store("the disk is full".to_owned()))
        }
    }
    let broken = Arc::new(Broken(FileSetup::volatile(), Mutex::new(false)));
    let failing = world_over(broken.clone(), "mdm-name-store-fails");
    assert_eq!(
        failing
            .coordinator
            .mdm_put_name(&alice(), &name_of("mdm.example.com")),
        Err(MdmRefusal::StoreFailed)
    );
    let reasons: Vec<Option<String>> = audit(&failing).into_iter().map(|row| row.3).collect();
    assert_eq!(
        reasons,
        [
            Some("set to mdm.example.com".to_owned()),
            Some("refused: MDM_REFUSAL_STORE_FAILED for mdm.example.com".to_owned()),
        ]
    );
    *broken.1.lock().expect("a lock") = true;
    assert_eq!(
        failing
            .coordinator
            .mdm_put_name(&alice(), &name_of("mdm.example.com")),
        Err(MdmRefusal::StoreFailed)
    );
    assert_eq!(
        audit(&failing).len(),
        2,
        "nothing is audited for a read that failed"
    );
}
