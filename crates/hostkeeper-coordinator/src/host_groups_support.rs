//! What the tests of the `HostGroups` calls share: a coordinator over an in-memory store and
//! an enrollment over in-memory records (both can be told to fail), enrolled hosts that
//! heartbeat what they run, groups put and their desired versions and marks seeded, and the
//! state a refused call must leave as it was.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_enrollment::enroll::Membership;
use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;
use hostkeeper_wire::version::Version;

use crate::audit_file::UnixClock;
use crate::desired::{DesiredStore, Held};
use crate::enrollment::Enrollment;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, approve, enroll_request};
use crate::host_groups::{GroupDesired, GroupMarks, GroupName};
use crate::marks_support::{
    NEW_DAEMON, ask, beat_of, native_targets, operator, runs, services_with,
};
use crate::ops::{AuditEntry, OpsState, OpsStore, Write};
use crate::releases::Release;
use crate::service::Coordinator;
use crate::store::{Backend, Fake, Shared};
use crate::testutil::Rig;
use crate::verdict_file::VerdictFile;

/// The operator's reason in the calls of the tests.
pub(crate) const REASON: &str = "for the tests";

/// The time the audit of the tests reads, in unix seconds.
pub(crate) const UNIX: u64 = 1_900_000_000;

/// The package every group of the tests lists.
pub(crate) const SELF: &str = "hostkeeper";

/// A coordinator past its quiet start over `fake` and, when given, `enrollment`, its audit
/// reading [`UNIX`].
pub(crate) fn coordinator_on(
    fake: &Fake,
    clock: &Arc<AtomicU64>,
    enrollment: Option<Arc<Enrollment>>,
) -> Coordinator {
    let list = format!(
        "{}services = [\"web\", \"db\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    let loaded = fake.load().expect("the fake loads");
    let backend: Shared = Arc::new(fake.clone());
    let unix: UnixClock = Arc::new(|| UNIX);
    let coordinator = Rig::start_over(
        DesiredStore::over(Arc::clone(&backend), loaded.desired),
        clock,
        &list,
    )
    .with_services(services_with(""))
    .with_legacy_service(None)
    .with_ops(OpsStore::over(Arc::clone(&backend), loaded.ops).with_unix(unix))
    .with_verdicts(VerdictFile::over(backend, loaded.verdicts));
    match enrollment {
        Some(enrollment) => coordinator.with_enrollment(enrollment),
        None => coordinator,
    }
}

/// Enrolls the host `host` in `enrollment`: its request is posted and approved, as a storage
/// host.
pub(crate) fn enroll_host(enrollment: &Enrollment, host: &str) {
    let key = HostKey::new();
    enrollment
        .enroll(enroll_request(&key, &format!("machine-{host}")))
        .expect("posted");
    approve(enrollment, &key, host, false);
}

/// A coordinator with its enrollment, and what it runs on.
pub(crate) struct World {
    pub(crate) rig: Rig,
    pub(crate) fake: Fake,
    pub(crate) records: Arc<MemoryRecords>,
    pub(crate) enrollment: EnrollmentRig,
}

impl World {
    /// A world named `name`, its clock at 2000 s.
    pub(crate) fn new(name: &str) -> Self {
        Self::over(name, Fake::new())
    }

    /// A world named `name` over the committed state `fake` holds.
    pub(crate) fn over(name: &str, fake: Fake) -> Self {
        let records = Arc::new(MemoryRecords::new());
        let enrollment =
            EnrollmentRig::over_records(&format!("{name}-enrollment"), records.clone());
        let mut rig = Rig::new(name);
        rig.coordinator =
            coordinator_on(&fake, &rig.clock, Some(Arc::clone(&enrollment.enrollment)));
        rig.at(2_000);
        Self {
            rig,
            fake,
            records,
            enrollment,
        }
    }

    /// The coordinator.
    pub(crate) fn coordinator(&self) -> &Coordinator {
        &self.rig.coordinator
    }

    /// Enrolls the host `host` ([`enroll_host`]).
    pub(crate) fn enroll(&self, host: &str) {
        enroll_host(&self.enrollment.enrollment, host);
    }

    /// A heartbeat of `host` that lists `services`, each at 1.0.
    pub(crate) fn beat(&self, host: &str, services: &[&str]) {
        let listed = services.iter().map(|name| runs(name, "1.0")).collect();
        ask(&self.rig, host, "storage", beat_of(NEW_DAEMON, listed));
    }

    /// `HostGroups.Put` of the group `name` listing `packages`, over the generation
    /// `if_generation`, with the policy `policy`.
    pub(crate) fn put_with(
        &self,
        name: &str,
        packages: &[&str],
        policy: Option<proto::RolloutPolicy>,
        if_generation: Option<u64>,
    ) -> Result<proto::PutHostGroupResponse, Refusal> {
        self.coordinator().host_groups_put(
            &operator(),
            put_of(body(name, packages, policy), if_generation),
        )
    }

    /// [`Self::put_with`] with the default policy.
    pub(crate) fn put(
        &self,
        name: &str,
        packages: &[&str],
        if_generation: Option<u64>,
    ) -> Result<proto::PutHostGroupResponse, Refusal> {
        self.put_with(name, packages, None, if_generation)
    }

    /// `HostGroups.Move` of `host` into `group`.
    pub(crate) fn move_to(
        &self,
        host: &str,
        group: &str,
    ) -> Result<proto::MoveHostResponse, Refusal> {
        self.coordinator()
            .host_groups_move(&operator(), move_of(host, group))
    }

    /// Sets the group's desired version of a package, as the calls of a later release do. The
    /// version has no release kept: no host is picked an entry of it.
    pub(crate) fn seed_desired(&self, group: &str, package: &str, version: &str) {
        self.seed(group, package, version, None);
    }

    /// [`Self::seed_desired`] with the release [`native_targets`] 1 kept for the version, which
    /// is what picks a host an entry of it.
    pub(crate) fn seed_ordered(&self, group: &str, package: &str, version: &str) {
        let release = Arc::new(Release::new(native_targets(package, 1), None));
        self.seed(group, package, version, Some(release));
    }

    fn seed(&self, group: &str, package: &str, version: &str, release: Option<Arc<Release>>) {
        let name: GroupName = group.parse().expect("a group name");
        let at = Version::parse(version).expect("a version");
        let (staged, ()) = self
            .coordinator()
            .desired
            .stage_host_groups(|held| {
                let mut groups = held.clone();
                groups.0.get_mut(&name).expect("the group").desired.insert(
                    package.to_owned(),
                    GroupDesired {
                        version: at.clone(),
                        generation: 1,
                        write_id: None,
                        replaced_version: None,
                    },
                );
                Ok::<_, Infallible>((groups, ()))
            })
            .expect("staged");
        let staged = match release {
            Some(release) => staged.with_release(package, &at, release),
            None => staged,
        };
        self.coordinator()
            .ops()
            .decide_changes_with(staged, |_| vec![AuditEntry::new("test", "seed")])
            .expect("seeded");
    }

    /// Makes `change` to the marks of `package` in `group`.
    pub(crate) fn seed_marks(
        &self,
        group: &str,
        package: &str,
        change: impl FnOnce(&mut GroupMarks),
    ) {
        let name: GroupName = group.parse().expect("a group name");
        self.coordinator()
            .ops()
            .change(
                Write::BeforeMemory,
                AuditEntry::new("test", "seed"),
                |state| {
                    change(
                        state
                            .host_groups
                            .0
                            .entry(name)
                            .or_default()
                            .entry(package.to_owned())
                            .or_default(),
                    );
                },
            )
            .expect("seeded");
    }

    /// The ops state now.
    pub(crate) fn ops(&self) -> OpsState {
        self.coordinator().ops().state()
    }

    /// The audit ring now, oldest first.
    pub(crate) fn audit(&self) -> Vec<AuditEntry> {
        self.ops().audit
    }

    /// The audit entries after the first `skip`.
    pub(crate) fn audit_after(&self, skip: usize) -> Vec<AuditEntry> {
        self.audit().split_off(skip)
    }

    /// The group of the host `host`, as its record holds it.
    pub(crate) fn membership(&self, host: &str) -> Membership {
        let host = hostkeeper_enrollment::names::HostId::parse(host).expect("a host id");
        self.enrollment
            .enrollment
            .store()
            .membership(&host)
            .expect("read")
            .expect("enrolled")
    }

    /// Everything a refused call must leave as it was: what the store committed, and every
    /// host's group.
    pub(crate) fn state(&self) -> (Held, OpsState, Vec<(String, Membership)>) {
        let memberships = self
            .enrollment
            .enrollment
            .store()
            .memberships()
            .expect("read")
            .into_iter()
            .map(|(id, membership)| (id.to_string(), membership))
            .collect();
        (self.fake.desired(), self.fake.ops(), memberships)
    }

    /// The uninstall intents of the state now: package, node id and generation.
    pub(crate) fn intents(&self) -> Vec<(String, String, u64)> {
        self.ops()
            .assignments
            .iter()
            .flat_map(|(package, entries)| {
                entries
                    .uninstall
                    .iter()
                    .map(|(host, generation)| (package.clone(), host.clone(), *generation))
            })
            .collect()
    }
}

/// The body of the group `name` listing `packages`, with `policy`.
pub(crate) fn body(
    name: &str,
    packages: &[&str],
    policy: Option<proto::RolloutPolicy>,
) -> proto::HostGroup {
    proto::HostGroup {
        name: name.to_owned(),
        packages: packages.iter().map(|p| (*p).to_owned()).collect(),
        policy,
    }
}

/// A `Put` of `body`, replacing the generation `if_generation`.
pub(crate) fn put_of(
    body: proto::HostGroup,
    if_generation: Option<u64>,
) -> proto::PutHostGroupRequest {
    proto::PutHostGroupRequest {
        host_group: Some(body),
        if_generation,
        reason: REASON.to_owned(),
    }
}

/// A `Move` of `host` into `group`.
pub(crate) fn move_of(host: &str, group: &str) -> proto::MoveHostRequest {
    proto::MoveHostRequest {
        node_id: host.to_owned(),
        host_group: group.to_owned(),
        reason: REASON.to_owned(),
    }
}

/// A `Remove` of `group`.
pub(crate) fn remove_of(group: &str) -> proto::RemoveHostGroupRequest {
    proto::RemoveHostGroupRequest {
        host_group: group.to_owned(),
        reason: REASON.to_owned(),
    }
}

/// The audit entry `seq` the operator makes: `action` about `group`, `node` and `service`, with
/// `reason`.
pub(crate) fn operator_entry(
    seq: u64,
    action: &str,
    group: Option<&str>,
    node: Option<&str>,
    reason: &str,
) -> AuditEntry {
    AuditEntry {
        seq,
        by: "operator".to_owned(),
        action: action.to_owned(),
        service: None,
        node_id: node.map(str::to_owned),
        host_group: group.map(str::to_owned),
        reason: Some(reason.to_owned()),
    }
}

/// The audit entry `seq` the coordinator makes when it cancels the uninstall of `service` from
/// `node` for a change of the group `group`.
pub(crate) fn cancelled(seq: u64, group: &str, service: &str, node: &str) -> AuditEntry {
    AuditEntry {
        seq,
        by: "coordinator".to_owned(),
        action: "uninstall_cancelled".to_owned(),
        service: Some(service.to_owned()),
        node_id: Some(node.to_owned()),
        host_group: Some(group.to_owned()),
        reason: None,
    }
}
