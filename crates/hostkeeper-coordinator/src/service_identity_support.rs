//! What the tests of the service identity share: a farm whose group lists a service with an
//! identity and one without, hosts that hold their own key and certificate (approved by an
//! operator or by an inventory record), and the call of `IssueServiceCertificate`.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{ApprovalRecord, Decision, INVENTORY_APPROVER, Membership};
use hostkeeper_enrollment::names::Role;
use hostkeeper_proto::hostkeeper::agent::v1::{
    IssueServiceCertificateRequest, IssueServiceCertificateResponse, PollState,
};
use hostkeeper_testkit::Scratch;
use rcgen::{CertificateParams, DistinguishedName, DnType};

use crate::enrollment::tests::support::{
    HostKey, Rig as EnrollmentRig, enroll_request, host_id, signed_poll,
};
use crate::enrollment::{Enrollment, RpcError};
use crate::group_decision_support::Farm;
use crate::host_groups_support::{SELF, World};
use crate::marks_support::native_targets;
use crate::ops::AuditEntry;
use crate::releases::Targets;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::{Rig, id};

/// The package whose entry says `identity`.
pub(crate) const SERVICE: &str = "js-agent";

/// The package whose entry does not.
pub(crate) const PLAIN: &str = "hello";

/// The group of the farm.
pub(crate) const GROUP: &str = "farm";

/// The release of `service` pinned by the digest `n`, every entry of which has the manifest field
/// `identity` set to `identity`.
pub(crate) fn identity_targets(service: &str, n: u8, identity: Option<bool>) -> Targets {
    let mut targets = native_targets(service, n);
    for list in targets.values_mut() {
        for entry in &mut list.entries {
            entry.manifest.as_mut().expect("a manifest").identity = identity;
        }
    }
    targets
}

/// A host as it holds itself: the key it enrolled with and the certificate it was issued.
pub(crate) struct Held {
    pub(crate) name: String,
    pub(crate) key: HostKey,
    pub(crate) cert: Vec<u8>,
}

/// The approver of a host an operator approved by hand, in these tests.
const OPERATOR_APPROVER: &str = "operator";

impl Held {
    /// The certificate as a connection presents it.
    pub(crate) fn peer(&self) -> Option<Arc<[u8]>> {
        Some(Arc::from(self.cert.as_slice()))
    }
}

/// A request for a certificate for a key the daemon made for a service: a CSR of `key` whose
/// subject is `subject`.
pub(crate) fn csr_of(key: &HostKey, subject: &str) -> Vec<u8> {
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, subject);
    params
        .serialize_request(&key.pair)
        .expect("a CSR")
        .der()
        .to_vec()
}

/// The request of a certificate for `service` with a CSR of `key`.
pub(crate) fn request_for(service: &str, key: &HostKey) -> IssueServiceCertificateRequest {
    IssueServiceCertificateRequest {
        service: service.to_owned(),
        csr_der: csr_of(key, &format!("service:{service}")),
    }
}

/// A coordinator the tests call through the router, with what is under it.
pub(crate) struct Wired {
    pub(crate) coordinator: Arc<Coordinator>,
    pub(crate) enrollment: EnrollmentRig,
    pub(crate) fake: Fake,
    _dir: Scratch,
}

impl Wired {
    /// `rig`'s coordinator, which has no enrollment, behind an `Arc`.
    pub(crate) fn bare(rig: Rig) -> Wired {
        Wired {
            coordinator: Arc::new(rig.coordinator),
            enrollment: EnrollmentRig::new("identity-unused-enrollment"),
            fake: Fake::new(),
            _dir: rig.dir,
        }
    }
}

/// A farm in group mode: the group [`GROUP`] lists [`SERVICE`], whose release says `identity`, and
/// [`PLAIN`], whose release does not, each wanted at a version with its release kept.
pub(crate) struct Fix {
    pub(crate) farm: Farm,
}

impl Fix {
    /// A farm named `name`.
    pub(crate) async fn new(name: &str) -> Fix {
        let farm = Farm::new(name);
        farm.group(GROUP, &[SERVICE, PLAIN], None);
        let fix = Fix { farm };
        fix.release(SERVICE, "0.1.230", Some(true)).await;
        fix.release(PLAIN, "1.0", None).await;
        fix
    }

    /// A farm named `name` that holds no host group, so it places by role: the rollout file gives
    /// the role `storage` the services `web` and `db`.
    pub(crate) fn without_groups(name: &str) -> Fix {
        Fix {
            farm: Farm::new(name),
        }
    }

    /// Sets `package` in the group at `version` with a release whose entries say `identity`.
    pub(crate) async fn release(&self, package: &str, version: &str, identity: Option<bool>) {
        let body = serde_json::json!({"service": package, "version": version});
        self.farm
            .set(GROUP, &body, Some(identity_targets(package, 1, identity)))
            .await;
    }

    /// `HostGroups.Put` of [`GROUP`] listing hostkeeper and `packages` alone, over the group as it
    /// is now: what an operator does to take a service out of the group.
    pub(crate) fn relist(&self, packages: &[&str]) {
        let name = GROUP.parse().expect("a group name");
        let generation = self
            .coordinator()
            .desired
            .host_group(&name)
            .expect("the group")
            .generation;
        let listed: Vec<&str> = std::iter::once(SELF)
            .chain(packages.iter().copied())
            .collect();
        self.farm
            .world
            .put(GROUP, &listed, Some(generation))
            .expect("the group is put");
    }

    /// The farm's coordinator behind an `Arc`, to call through [`crate::grpc::route`]; the farm's
    /// hosts keep their certificates.
    pub(crate) fn wired(self) -> Wired {
        let World {
            rig,
            fake,
            enrollment,
            ..
        } = self.farm.world;
        Wired {
            coordinator: Arc::new(rig.coordinator),
            enrollment,
            fake,
            _dir: rig.dir,
        }
    }

    /// The coordinator.
    pub(crate) fn coordinator(&self) -> &Coordinator {
        self.farm.world.coordinator()
    }

    /// The enrollment.
    pub(crate) fn enrollment(&self) -> &Arc<Enrollment> {
        &self.farm.world.enrollment.enrollment
    }

    /// The host `host`, approved by an operator into `group` and heard once, so the coordinator
    /// knows its features and the installers its daemon lists.
    pub(crate) fn operator_host(&self, host: &str) -> Held {
        self.enrolled(host, Some(GROUP), None, false)
    }

    /// The host `host`, approved into [`GROUP`] by an inventory record, as the coordinator does
    /// by itself, and heard once.
    pub(crate) fn auto_host(&self, host: &str) -> Held {
        self.enrolled(host, Some(GROUP), Some(INVENTORY_APPROVER), false)
    }

    /// A new key of the host `host`, which the inventory record approves, taking its id over from
    /// the key it held, and heard once.
    pub(crate) fn auto_host_replaced(&self, host: &str) -> Held {
        self.enrolled(host, Some(GROUP), Some(INVENTORY_APPROVER), true)
    }

    /// The host `host`, approved by an operator into [`GROUP`] and never heard: the coordinator
    /// knows nothing of its daemon.
    pub(crate) fn unheard_host(&self, host: &str) -> Held {
        self.enrolled_as(host, Some(GROUP), None, false, false)
    }

    /// The host `host`, approved by an operator and placed in no group.
    pub(crate) fn lone_host(&self, host: &str) -> Held {
        self.enrolled(host, None, None, false)
    }

    /// The host `host` enrolled with its own key and approved by `approver` (an operator
    /// named [`OPERATOR_APPROVER`] when none), taking the id over when `replace`, in `group` with its move
    /// over, and heard once.
    fn enrolled(
        &self,
        host: &str,
        group: Option<&str>,
        approver: Option<&str>,
        replace: bool,
    ) -> Held {
        self.enrolled_as(host, group, approver, replace, true)
    }

    /// [`Self::enrolled`], heard once when `heard`.
    fn enrolled_as(
        &self,
        host: &str,
        group: Option<&str>,
        approver: Option<&str>,
        replace: bool,
        heard: bool,
    ) -> Held {
        let enrollment = self.enrollment();
        let key = HostKey::new();
        let machine = format!("machine-{}-{}", host, u8::from(replace));
        enrollment
            .enroll(enroll_request(&key, &machine))
            .expect("posted");
        let hash = key.key_hash();
        let decision = Decision {
            host_id: host_id(host),
            role: Role::parse("storage").expect("a role"),
            services: Vec::new(),
            replace,
            host_group: None,
        };
        enrollment
            .store()
            .approve_recorded(
                &hash,
                &hash,
                &decision,
                &ApprovalRecord {
                    approver: approver.unwrap_or(OPERATOR_APPROVER).to_owned(),
                    approved_unix: crate::enrollment::tests::support::NOW,
                    reason: "matched".to_owned(),
                    mac_nonce: None,
                    mac_enrolled_since_unix: None,
                },
            )
            .expect("approved");
        let issued = signed_poll(enrollment, &key, &key).expect("polled");
        assert_eq!(issued.state, PollState::Issued as i32);
        if let Some(group) = group {
            if !replace {
                self.farm.world.move_to(host, group).expect("moved");
            }
            enrollment
                .store()
                .set_membership(
                    &host_id(host),
                    Membership {
                        host_group: group.to_owned(),
                        moving_from: None,
                    },
                )
                .expect("the move ended");
            self.coordinator()
                .lock_fleet()
                .sync_move(&id(host), None, false);
        }
        if heard {
            self.farm.tell(host, &[]);
        }
        Held {
            name: host.to_owned(),
            key,
            cert: issued.certificate_der,
        }
    }

    /// `IssueServiceCertificate` of `service` by `held`, with a key made now for the service.
    pub(crate) fn ask(
        &self,
        held: &Held,
        service: &str,
    ) -> Result<IssueServiceCertificateResponse, RpcError> {
        self.ask_with(&held.peer(), &request_for(service, &HostKey::new()))
    }

    /// `IssueServiceCertificate` with `request` by the holder of the certificate `peer`.
    pub(crate) fn ask_with(
        &self,
        peer: &Option<Arc<[u8]>>,
        request: &IssueServiceCertificateRequest,
    ) -> Result<IssueServiceCertificateResponse, RpcError> {
        self.coordinator()
            .issue_service_certificate(self.enrollment(), peer.as_deref(), request)
    }

    /// The audit entries of `action` so far, oldest first.
    pub(crate) fn audited(&self, action: &str) -> Vec<AuditEntry> {
        self.farm
            .world
            .audit()
            .into_iter()
            .filter(|entry| entry.action == action)
            .collect()
    }
}
