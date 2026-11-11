//! `Enrollment.Approve` and `Enrollment.AddRecord` by host group end to end, through the router
//! as the private listener serves them, once over gRPC and once over the JSON gateway, on a
//! coordinator that holds a group put by `HostGroups.Put`: the approval puts its host in the group
//! it names and answers the group-bearing audit entry, a record keeps the group it names, and an
//! approval naming services or a group the coordinator does not hold is refused with the status
//! of its value.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use prost::Message;
use tonic::Code;

use super::rollouts::{Refused, WIRES, Wire, rpc};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enroll_request};
use crate::host_groups_support::{SELF, body, coordinator_on, put_of};
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::{OPERATOR, Rig};

const PUT: &str = "/hostkeeper.operator.v1.HostGroups/Put";
const APPROVE: &str = "/hostkeeper.operator.v1.Enrollment/Approve";
const ADD_RECORD: &str = "/hostkeeper.operator.v1.Enrollment/AddRecord";

/// The reason of the approvals.
const WHY: &str = "racked";

/// A coordinator with its enrollment, holding the group `web`, called through the router.
struct Wired {
    coordinator: Arc<Coordinator>,
    enrollment: EnrollmentRig,
    _rig: Rig,
}

impl Wired {
    /// A coordinator named `name`, with the group `web` put over `wire`.
    async fn new(name: &str, wire: Wire) -> Self {
        let enrollment = EnrollmentRig::over_records(
            &format!("{name}-enrollment"),
            Arc::new(MemoryRecords::new()),
        );
        let fake = Fake::new();
        let rig = Rig::new(name);
        let coordinator =
            coordinator_on(&fake, &rig.clock, Some(Arc::clone(&enrollment.enrollment)));
        rig.at(2_000);
        let wired = Self {
            coordinator: Arc::new(coordinator),
            enrollment,
            _rig: rig,
        };
        let _: proto::PutHostGroupResponse = wired
            .ask(wire, PUT, &put_of(body("web", &[SELF], None), None))
            .await
            .expect("put");
        wired
    }

    /// The call `path` with `request` and the operator's token over `wire`.
    async fn ask<R: Message + Default>(
        &self,
        wire: Wire,
        path: &str,
        request: &impl Message,
    ) -> Result<R, Refused> {
        rpc(&self.coordinator, wire, path, Some(OPERATOR), request).await
    }

    /// A key whose request, from `machine`, is pending.
    fn posted(&self, machine: &str) -> HostKey {
        let key = HostKey::new();
        self.enrollment
            .enrollment
            .enroll(enroll_request(&key, machine))
            .expect("posted");
        key
    }

    /// The group the host `host` is in, as its record holds it.
    fn group_of(&self, host: &str) -> Option<String> {
        self.enrollment
            .enrollment
            .store()
            .membership(&HostId::parse(host).expect("a host id"))
            .expect("read")
            .map(|membership| membership.host_group)
    }
}

/// The approval of `key`'s request as `host`, naming `role`, `host_group` and `services`.
fn approval(
    key: &HostKey,
    host: &str,
    role: &str,
    host_group: &str,
    services: &[&str],
) -> proto::ApproveRequest {
    proto::ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        host_id: host.to_owned(),
        role: role.to_owned(),
        services: services.iter().map(|s| (*s).to_owned()).collect(),
        reason: WHY.to_owned(),
        host_group: host_group.to_owned(),
        ..proto::ApproveRequest::default()
    }
}

/// Proves, over gRPC and over the gateway, on a coordinator holding `web`:
/// - `Approve` naming `host_group` web answers the host id and its `enroll_approve` entry, whose
///   `host_group` is web and whose reason names the group, and the host's record is in web;
/// - `Approve` naming web only by `role` puts its host in web too;
/// - `AddRecord` naming web answers the record with web as `role` and `host_group`, a window,
///   and a `record_add` entry whose reason names the group.
///
/// Catches: the service adapter refusing, or reading as naming none, a group the coordinator
/// holds, which the coordinator's own calls would not show.
#[tokio::test]
async fn approve_and_add_record_place_by_group_over_both_wires() {
    for (n, wire) in WIRES.into_iter().enumerate() {
        let wired = Wired::new(&format!("grpc-enroll-groups-{n}"), wire).await;
        let key = wired.posted("machine-1");

        let approved: proto::ApproveResponse = wired
            .ask(wire, APPROVE, &approval(&key, "web-1", "", "web", &[]))
            .await
            .expect("approved");

        assert_eq!(approved.host_id, "web-1", "{wire:?}");
        let entry = approved.entry.expect("the entry");
        assert_eq!(
            (
                entry.action.as_str(),
                entry.node_id.as_deref(),
                entry.host_group.as_deref(),
                entry.reason.as_deref()
            ),
            (
                "enroll_approve",
                Some("web-1"),
                Some("web"),
                Some(
                    format!(
                        "request {}; group web; claims machine_id=machine-1; {WHY}",
                        key.key_hash()
                    )
                    .as_str()
                )
            ),
            "{wire:?}"
        );
        assert_eq!(wired.group_of("web-1").as_deref(), Some("web"), "{wire:?}");

        let by_role = wired.posted("machine-2");
        let _: proto::ApproveResponse = wired
            .ask(wire, APPROVE, &approval(&by_role, "web-2", "web", "", &[]))
            .await
            .expect("approved by role");
        assert_eq!(wired.group_of("web-2").as_deref(), Some("web"), "{wire:?}");

        let added: proto::AddRecordResponse = wired
            .ask(
                wire,
                ADD_RECORD,
                &proto::AddRecordRequest {
                    host_id: "web-3".to_owned(),
                    data_macs: vec!["0c:c4:7a:00:00:03".to_owned()],
                    host_group: "web".to_owned(),
                    ..proto::AddRecordRequest::default()
                },
            )
            .await
            .expect("added");
        let record = added.record.expect("the record");
        assert_eq!(
            (record.role.as_str(), record.host_group.as_str()),
            ("web", "web"),
            "{wire:?}"
        );
        assert!(record.approve_until_unix > 0, "{wire:?}");
        let entry = added.entry.expect("the entry");
        assert_eq!(entry.action, "record_add", "{wire:?}");
        assert!(
            entry
                .reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("group web; data 0c:c4:7a:00:00:03")),
            "{wire:?}: {:?}",
            entry.reason
        );
    }
}

/// Proves, over gRPC and over the gateway, on a coordinator holding `web`: `Approve` naming
/// services is refused `INVALID_ARGUMENT` `services_retired`, one naming a group it does not hold
/// `NOT_FOUND` `unknown_host_group`, and `AddRecord` naming a group it does not hold
/// `NOT_FOUND` `unknown_host_group`; the request stays pending and no record is added.
///
/// Catches: a refusal of the coordinator answered with another status by the adapter.
#[tokio::test]
async fn approve_and_add_record_refusals_by_group_have_their_status_over_both_wires() {
    for (n, wire) in WIRES.into_iter().enumerate() {
        let wired = Wired::new(&format!("grpc-enroll-groups-refused-{n}"), wire).await;
        let key = wired.posted("machine-1");
        let unknown = (Code::NotFound, "unknown_host_group".to_owned());

        let refused = wired
            .ask::<proto::ApproveResponse>(
                wire,
                APPROVE,
                &approval(&key, "web-1", "", "web", &["kbf"]),
            )
            .await
            .expect_err("services");
        assert_eq!(
            refused,
            (Code::InvalidArgument, "services_retired".to_owned()),
            "{wire:?}"
        );
        let refused = wired
            .ask::<proto::ApproveResponse>(wire, APPROVE, &approval(&key, "web-1", "", "db", &[]))
            .await
            .expect_err("unknown group");
        assert_eq!(refused, unknown, "{wire:?}");
        let refused = wired
            .ask::<proto::AddRecordResponse>(
                wire,
                ADD_RECORD,
                &proto::AddRecordRequest {
                    host_id: "web-3".to_owned(),
                    data_macs: vec!["0c:c4:7a:00:00:03".to_owned()],
                    host_group: "db".to_owned(),
                    ..proto::AddRecordRequest::default()
                },
            )
            .await
            .expect_err("unknown group");
        assert_eq!(refused, unknown, "{wire:?}");

        assert_eq!(wired.group_of("web-1"), None, "{wire:?}");
        assert!(
            wired.coordinator.ops().state().inventory.is_empty(),
            "{wire:?}"
        );
    }
}
