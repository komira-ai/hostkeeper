//! `Mdm.Verify` over a setup alone (no Macs' listener): what it says of a token the master key does
//! not open, and that a store which fails part way refuses and sends nothing.

use std::sync::{Arc, Mutex};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{MdmAbmResult, MdmRefusal};

use crate::login_support::World;
use crate::mdm_secrets::MasterKey;
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

fn world_over(store: Arc<dyn SetupStore>, key: u8, name: &str) -> World {
    let world = World::build_timed(
        name,
        crate::login_support::settings(),
        crate::login_support::counting(),
        true,
        |c, _| {
            c.with_mdm_setup(MdmSetup::new(
                store,
                Some(MasterKey::from_bytes(&[key; 32]).expect("a key")),
                crate::enrollment::os_random(),
            ))
        },
    );
    world.at(1_800_000_000);
    world
}

fn put_token(world: &World) {
    world
        .coordinator
        .mdm_put_secret(
            &alice(),
            proto::PutSecretRequest {
                kind: "abm_token".to_owned(),
                material: br#"{"consumer_key":"ck","consumer_secret":"cs","access_token":"at","access_secret":"as","access_secret_expiry":"2027-10-31T22:01:36Z"}"#.to_vec(),
                not_after_unix: None,
                with_certificate: Vec::new(),
            },
        )
        .expect("stored");
}

fn ask() -> proto::VerifyRequest {
    proto::VerifyRequest::default()
}

/// Proves: with no listener there is nothing to ask Apple with, so a token held is `not_checked`;
/// and a token sealed under another master key is `unreadable` (Apple is not asked), not missing.
#[tokio::test]
async fn a_token_held_without_a_client_is_not_checked_and_one_the_key_does_not_open_is_unreadable()
{
    let store: Arc<dyn SetupStore> = Arc::new(FileSetup::volatile());
    let before = world_over(Arc::clone(&store), 0x01, "mdm-verify-rekey-before");
    put_token(&before);
    let answer = before
        .coordinator
        .mdm_verify(&alice(), ask())
        .await
        .expect("verified");
    assert_eq!(
        answer.abm.expect("a check").result,
        i32::from(MdmAbmResult::NotChecked)
    );
    assert!(!answer.listener);

    let after = world_over(store, 0x02, "mdm-verify-rekey-after");
    let answer = after
        .coordinator
        .mdm_verify(&alice(), ask())
        .await
        .expect("verified");
    let check = answer.abm.expect("a check");
    assert_eq!(check.result, i32::from(MdmAbmResult::Unreadable));
    assert_eq!(check.detail, "the token held does not open");
}

/// Proves: a token held by a coordinator that has no master key is not called missing: the answer is
/// `unreadable` saying no secrets key is configured, and `no_token` is said only for a store with no
/// token under a key.
#[tokio::test]
async fn a_token_held_with_no_master_key_is_unreadable_and_not_missing() {
    let store: Arc<dyn SetupStore> = Arc::new(FileSetup::volatile());
    let keyed = world_over(Arc::clone(&store), 0x31, "mdm-verify-nokey-before");
    let answer = keyed
        .coordinator
        .mdm_verify(&alice(), ask())
        .await
        .expect("verified");
    assert_eq!(
        answer.abm.expect("a check").result,
        i32::from(MdmAbmResult::NoToken)
    );
    put_token(&keyed);

    let world = World::build_timed(
        "mdm-verify-nokey-after",
        crate::login_support::settings(),
        crate::login_support::counting(),
        true,
        |c, _| c.with_mdm_setup(MdmSetup::new(store, None, crate::enrollment::os_random())),
    );
    world.at(1_800_000_000);
    let answer = world
        .coordinator
        .mdm_verify(&alice(), ask())
        .await
        .expect("verified");
    let check = answer.abm.expect("a check");
    assert_eq!(check.result, i32::from(MdmAbmResult::Unreadable));
    assert_eq!(check.detail, "no secrets key is configured");
}

/// Proves: a store whose secrets stop reading after some number of reads makes the call refuse
/// `store_failed` wherever it stops, never answer a half-read setup, and never panic; the call is
/// answered once the store reads again.
#[tokio::test]
async fn a_store_that_fails_part_way_refuses_wherever_it_stops() {
    struct Flaky(FileSetup, Mutex<(usize, usize)>);
    impl SetupStore for Flaky {
        fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
            self.0.put_secret(row)
        }
        fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
            let mut state = self.1.lock().expect("a lock");
            state.1 += 1;
            if state.1 == state.0 {
                Err(SetupError::Store("unreadable".to_owned()))
            } else {
                self.0.secrets()
            }
        }
        fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
            self.0.prune_superseded(at)
        }
        fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
            self.0.settings()
        }
        fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
            self.0.set_setting(name, value)
        }
    }
    let flaky = Arc::new(Flaky(FileSetup::volatile(), Mutex::new((0, 0))));
    let world = world_over(flaky.clone(), 0x21, "mdm-verify-store");
    put_token(&world);
    let mut refused = 0;
    for failing in 1..=40 {
        *flaky.1.lock().expect("a lock") = (failing, 0);
        match world.coordinator.mdm_verify(&alice(), ask()).await {
            Ok(_) => {}
            Err(why) => {
                assert_eq!(why, MdmRefusal::StoreFailed, "read {failing}");
                refused += 1;
            }
        }
    }
    assert!(refused > 1, "reads of the setup stop the call: {refused}");
    *flaky.1.lock().expect("a lock") = (0, 0);
    assert!(world.coordinator.mdm_verify(&alice(), ask()).await.is_ok());
}
