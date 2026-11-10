//! What the tests of the decision by host group share: a farm of enrolled hosts placed in groups,
//! heartbeats of what they run, and the desired versions and marks of the groups seeded.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{
    Class, Heartbeat, Outcome, Phase, ServiceBeat, ServiceReply, UpdateResult,
};
use serde_json::{Value, json};

use crate::decide::Why;
use crate::host_groups_support::{SELF, World};
use crate::marks_support::{INSTALLER, NEW_DAEMON, ask, ask_typed, beat_of, digest, runs};
use crate::orders::Order;
use crate::releases::Targets;
use crate::service::ci_calls::Setter;
use crate::testutil::{id, version};
use crate::write_record::WriteAsk;

/// A rollout policy that `edit` made of the default one.
pub(crate) fn policy(edit: impl FnOnce(&mut proto::RolloutPolicy)) -> proto::RolloutPolicy {
    let mut policy = proto::RolloutPolicy::default();
    edit(&mut policy);
    policy
}

/// A stage of the update plan that the plan enters by itself.
pub(crate) fn stages(percents: &[u32]) -> Vec<proto::PolicyStage> {
    percents
        .iter()
        .map(|percent| proto::PolicyStage {
            percent: *percent,
            advance: proto::StageAdvance::Auto.into(),
        })
        .collect()
}

/// A farm: a coordinator past its quiet start with an enrollment, and the hosts and groups put in
/// it.
pub(crate) struct Farm {
    pub(crate) world: World,
}

impl Farm {
    /// A farm named `name`.
    pub(crate) fn new(name: &str) -> Self {
        Self {
            world: World::new(name),
        }
    }

    /// A farm named `name` over the committed state `fake` holds.
    pub(crate) fn over(name: &str, fake: crate::store::Fake) -> Self {
        Self {
            world: World::over(name, fake),
        }
    }

    /// The group `name` listing `packages` after hostkeeper, with `policy`.
    pub(crate) fn group(
        &self,
        name: &str,
        packages: &[&str],
        policy: Option<proto::RolloutPolicy>,
    ) {
        let listed: Vec<&str> = std::iter::once(SELF)
            .chain(packages.iter().copied())
            .collect();
        self.world
            .put_with(name, &listed, policy, None)
            .expect("a group put");
    }

    /// The enrolled host `host`, placed in the group `group` with its move over: as a host
    /// approved into the group is.
    pub(crate) fn host(&self, host: &str, group: &str) {
        self.moving(host, group);
        let enrolled = hostkeeper_enrollment::names::HostId::parse(host).expect("a host id");
        self.world
            .enrollment
            .enrollment
            .store()
            .set_membership(
                &enrolled,
                hostkeeper_enrollment::enroll::Membership {
                    host_group: group.to_owned(),
                    moving_from: None,
                },
            )
            .expect("the move ended");
        self.world
            .coordinator()
            .lock_fleet()
            .sync_move(&id(host), None, false);
    }

    /// The enrolled host `host`, moved into the group `group` and moving.
    pub(crate) fn moving(&self, host: &str, group: &str) {
        self.world.enroll(host);
        self.world.move_to(host, group).expect("a host moved");
    }

    /// The enrolled host `host`, in no group.
    pub(crate) fn lone(&self, host: &str) {
        self.world.enroll(host);
    }

    /// Sets the desired version of `package` in `group`.
    pub(crate) fn want(&self, group: &str, package: &str, at: &str) {
        self.world.seed_desired(group, package, at);
    }

    /// The reply to a heartbeat of `host` that lists `services`, each as it is given.
    pub(crate) fn tell_with(&self, host: &str, services: Vec<ServiceBeat>) -> ServiceReply {
        ask(
            &self.world.rig,
            host,
            "storage",
            beat_of(NEW_DAEMON, services),
        )
    }

    /// The reply to a heartbeat of `host` that lists hostkeeper and `services`, each healthy at
    /// the version given.
    pub(crate) fn tell(&self, host: &str, services: &[(&str, &str)]) -> ServiceReply {
        let listed = std::iter::once((SELF, "1.0"))
            .chain(services.iter().copied())
            .map(|(name, at)| runs(name, at))
            .collect();
        self.tell_with(host, listed)
    }

    /// The operator's set of `body`, in the group `named` (empty: each group that lists the
    /// package), with `targets` when given.
    pub(crate) async fn set(&self, named: &str, body: &Value, targets: Option<Targets>) {
        self.world
            .coordinator()
            .set_desired_in(
                &Setter::Operator("operator"),
                body.to_string().as_bytes(),
                &WriteAsk::default(),
                targets,
                named,
            )
            .await
            .expect("a set");
    }

    /// The operator's set of `package` at `at` with the package digest of number `n`, in the
    /// group `named` (empty: each group that lists the package).
    pub(crate) async fn deploy(&self, named: &str, package: &str, at: &str, n: u8) {
        let body = json!({"service": package, "version": at, "release_digest": digest(n)});
        self.set(named, &body, None).await;
    }

    /// The beat of a daemon that takes installs and lists hostkeeper and `services`, each healthy
    /// at the version given.
    pub(crate) fn beat_with(&self, services: &[(&str, &str)]) -> Heartbeat {
        let listed = std::iter::once((SELF, "1.0"))
            .chain(services.iter().copied())
            .map(|(name, at)| runs(name, at))
            .collect();
        beat_of(INSTALLER, listed)
    }

    /// The typed order for a heartbeat of `host`, a daemon that takes installs, that lists
    /// hostkeeper and `services`.
    pub(crate) fn order(&self, host: &str, services: &[(&str, &str)]) -> Option<Order> {
        self.order_on(host, self.beat_with(services))
    }

    /// The typed order for the heartbeat `beat` of `host`.
    pub(crate) fn order_on(&self, host: &str, beat: Heartbeat) -> Option<Order> {
        ask_typed(&self.world.rig, host, "storage", beat).1
    }

    /// Why the gate does not order the package `package` to `host` now.
    pub(crate) fn why(&self, host: &str, package: &str) -> Option<Why> {
        let coordinator = self.world.coordinator();
        let host = id(host);
        let basis = coordinator.basis_of(&host);
        let desired = coordinator.desired_for(&basis.services);
        let fleet = coordinator.lock_fleet();
        let now = (coordinator.clock)();
        crate::halt::decide(
            coordinator.ops(),
            &basis.services,
            &desired,
            package,
            &fleet,
            &host,
            now,
        )
        .why()
    }

    /// The version `package` is wanted at in `group`, if it is wanted.
    pub(crate) fn wanted(&self, group: &str, package: &str) -> Option<Version> {
        let group = group.parse().expect("a group name");
        self.world
            .coordinator()
            .desired
            .group_desired(&group, package)
            .map(|desired| desired.version)
    }
}

/// What a host that failed its update of `package` to `target` reports: it runs `running` again
/// and its last update failed as `phase` says.
pub(crate) fn failed(package: &str, running: &str, target: &str, phase: Phase) -> ServiceBeat {
    let mut one = runs(package, running);
    one.last_update = Some(UpdateResult {
        target: version(target),
        outcome: Outcome::Failed,
        phase: Some(phase),
        class: (phase == Phase::AfterCommit).then_some(Class::Release),
        reason: None,
        order_id: None,
        order_try: None,
    });
    one
}
