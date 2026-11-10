//! What a coordinator orders a host before the migration of its store and after it: the same.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::{MemoryRecords, Record, Records as _};

use crate::assignment::Entry;
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::host_groups_support::coordinator_on;
use crate::marks_support::{INSTALLER, ask_typed, beat_of, native_targets, runs};
use crate::migrate_support::{Store, desired};
use crate::releases::Release;
use crate::services::Services;
use crate::store::postgres::transfer::Moved;
use crate::store::{Backend as _, Commit, Fake};
use crate::testutil::Rig;

/// The rollout file of the tests: two services on the role `web`.
const ROLLOUT: &str = "\
[[service]]
name = \"kbf\"
roles = [\"web\"]
[[service]]
name = \"kbf-config\"
roles = [\"web\"]
";

/// A coordinator over what `moved` holds, placing hosts by `services` (the rollout file before
/// the migration, none after it: the groups place), past its quiet start. The enrollment's
/// records are `moved`'s hosts.
fn started(name: &str, moved: &Moved, services: Services) -> (Rig, EnrollmentRig) {
    let records = Arc::new(MemoryRecords::new());
    let hosts: Vec<Record> = moved
        .records
        .hosts
        .iter()
        .cloned()
        .map(Record::Host)
        .collect();
    records.commit(&hosts).expect("hosts");
    let fake = Fake::new();
    fake.commit(&Commit {
        desired: Some((&Default::default(), &moved.loaded.desired)),
        ops: Some((&Default::default(), &moved.loaded.ops)),
        ..Commit::default()
    })
    .expect("committed");
    let enrollment = EnrollmentRig::over_records(&format!("{name}-enrollment"), records);
    let mut rig = Rig::new(name);
    rig.coordinator = coordinator_on(&fake, &rig.clock, Some(Arc::clone(&enrollment.enrollment)))
        .with_services(services);
    rig.at(2_000);
    (rig, enrollment)
}

/// The store of three web hosts, the config package excluded on the third, with `kbf` and
/// `kbf-config` at 2.1.0 and a release kept for each.
fn farm() -> Store {
    let mut store = Store::of(&[("w1", "web"), ("w2", "web"), ("w3", "web")]);
    store.entry("kbf-config", "w3", Entry::Exclude);
    for service in ["kbf", "kbf-config"] {
        store.want(service, desired("2.1.0"));
        store
            .moved
            .loaded
            .desired
            .releases
            .entry(service.to_owned())
            .or_default()
            .insert(
                "2.1.0".to_owned(),
                Arc::new(Release::new(native_targets(service, 1), None)),
            );
    }
    store
}

/// Proves: a heartbeat of a host that runs only `hostkeeper` is answered the same by a coordinator
/// that places by roles, the rollout file and an exclusion over the store before the migration,
/// and by one that places by the host groups over the store the plan made: the hosts of the group
/// with both packages and the host the config package is excluded on are each ordered what they
/// were ordered, `kbf` first.
///
/// Catches: a group set, order or desired version that differs from what the roles ordered, which
/// the window of the migration would then show as hosts ordered something new.
#[test]
fn a_host_is_ordered_after_the_migration_what_it_was_ordered_before() {
    let store = farm();
    let plan = store.plan(ROLLOUT);
    assert_eq!(plan.errors, Vec::<String>::new());
    for host in ["w1", "w3"] {
        let beat = || beat_of(INSTALLER, vec![runs("hostkeeper", "1.0")]);
        let (before, _keep) = started(
            &format!("migrate-orders-before-{host}"),
            &store.moved,
            Services::parse(ROLLOUT).expect("a rollout file"),
        );
        let (after, _kept) = started(
            &format!("migrate-orders-after-{host}"),
            &plan.after,
            Services::default(),
        );
        let was = ask_typed(&before, host, "web", beat());
        let is = ask_typed(&after, host, "web", beat());
        assert!(
            was.1.is_some(),
            "{host} is ordered something before: {was:?}"
        );
        assert_eq!(format!("{is:?}"), format!("{was:?}"), "{host}");
    }
}
