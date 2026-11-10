//! The operator's calls of the privileged tests: one method per RPC the tests use, on the
//! generated tonic clients of `hostkeeper-proto` through [`hostkeeper_testkit::operator`], each
//! giving the reply as proto3 canonical JSON (fields by their JSON names, enums by name, 64-bit
//! integers as strings, a field at its default left out) or the status the coordinator refused
//! with.
//!
//! The module is shared by the two coordinator harnesses (`e2e::coordinator`, which runs the
//! coordinator in the test process, and `support::pair`, which runs it as a child process); each
//! includes it by path. A call blocks, so it is made from a thread that is not inside a tokio
//! runtime, which is every test's thread.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use hostkeeper_proto::FILE_DESCRIPTOR_SET;
use hostkeeper_proto::hostkeeper::agent::v1::Platform;
use hostkeeper_proto::hostkeeper::operator::v1::{
    AddRecordRequest, ApproveRequest, AssignRequest, ConvergeRequest, DesiredState, ForceRequest,
    GetHostRequest, HostList, ListHostsRequest, ListPendingRequest, ListRecordsRequest,
    ListServicesRequest, PutRepositoryRequest, RetireRequest, SetDesiredRequest, StatusRequest,
    UnassignRequest, enrollment_client::EnrollmentClient, force_request::Targets,
    hosts_client::HostsClient, releases_client::ReleasesClient,
    repositories_client::RepositoriesClient, services_client::ServicesClient,
};
use hostkeeper_proto::hostkeeper::release::v1::{Repository, TargetList};
use hostkeeper_testkit::operator::{Operator, canonical_json};
use serde_json::Value;
use tonic::Status;

/// One call: the generated client `$client`'s `$method` with `$request`, its reply as the JSON of
/// the message `$reply` of `hostkeeper.operator.v1`.
macro_rules! rpc {
    ($api:expr, $client:ident . $method:ident, $request:expr, $reply:literal) => {{
        let request = $request;
        let reply = $api
            .0
            .call(|channel| async move { $client::new(channel).$method(request).await })?;
        canonical_json(
            FILE_DESCRIPTOR_SET,
            concat!("hostkeeper.operator.v1.", $reply),
            &reply,
        )
        .map_err(|e| Status::internal(e.to_string()))
    }};
}

/// What an approval says; a field left out is empty or false.
#[derive(Default)]
pub struct Approval<'a> {
    /// The pending request, as `enrollment list` gives its `requestId`.
    pub request_id: &'a str,
    /// The SHA-256 of the request's key.
    pub key_sha256: &'a str,
    /// The host id: empty for the matching record's.
    pub host_id: &'a str,
    /// The role the host is approved for.
    pub role: &'a str,
    /// The services assigned to the host.
    pub services: &'a [&'a str],
    /// Take the host id from the host that holds it.
    pub replace: bool,
    /// Why, for the audit log.
    pub reason: &'a str,
}

/// The operator's channel to one coordinator's private listener.
pub struct OperatorApi(Operator);

/// `reply` as the JSON the test reads, or the text of the status it ended with: for the
/// diagnostics a failed test prints, which never panic.
pub fn shown(reply: Result<Value, Status>) -> String {
    match reply {
        Ok(json) => json.to_string(),
        Err(status) => format!("{status}"),
    }
}

/// `reply`, failing the test with `what` and the status when the call was refused.
///
/// # Panics
/// The call ended with a status.
pub fn must(what: &str, reply: Result<Value, Status>) -> Value {
    reply.unwrap_or_else(|status| panic!("{what} failed: {status}"))
}

impl OperatorApi {
    /// The channel to `endpoint` (`https://<host>:<port>`), trusting the CA in the PEM file
    /// `ca_file`, with the token in `token_file` (its text, less one trailing newline) on every
    /// call. Nothing is sent until the first call.
    ///
    /// # Panics
    /// A file cannot be read or the endpoint, the CA or the token is not usable.
    pub fn open(endpoint: &str, ca_file: &Path, token_file: &Path) -> OperatorApi {
        let ca = fs::read(ca_file).unwrap_or_else(|e| panic!("read {}: {e}", ca_file.display()));
        let token = fs::read_to_string(token_file)
            .unwrap_or_else(|e| panic!("read {}: {e}", token_file.display()));
        let token = token.strip_suffix('\n').unwrap_or(&token);
        let operator = Operator::connect(endpoint, &ca, token)
            .unwrap_or_else(|e| panic!("the operator's channel to {endpoint}: {e}"));
        OperatorApi(operator)
    }

    /// `Enrollment.List`: the pending requests, as `requests`.
    pub fn pending(&self) -> Result<Value, Status> {
        rpc!(
            self,
            EnrollmentClient.list,
            ListPendingRequest::default(),
            "ListPendingResponse"
        )
    }

    /// `Enrollment.Approve` as `approval` says.
    pub fn approve(&self, approval: &Approval) -> Result<Value, Status> {
        rpc!(
            self,
            EnrollmentClient.approve,
            ApproveRequest {
                request_id: approval.request_id.to_owned(),
                key_sha256: approval.key_sha256.to_owned(),
                host_id: approval.host_id.to_owned(),
                role: approval.role.to_owned(),
                services: approval.services.iter().map(|s| (*s).to_owned()).collect(),
                replace: approval.replace,
                reason: approval.reason.to_owned(),
            },
            "ApproveResponse"
        )
    }

    /// `Enrollment.AddRecord`: the inventory record of `host_id` of `role`, with the data MAC
    /// `mac` and the serial `serial`, to install as `platform`.
    pub fn add_record(
        &self,
        host_id: &str,
        role: &str,
        mac: &str,
        serial: &str,
        platform: Platform,
        notes: &str,
    ) -> Result<Value, Status> {
        rpc!(
            self,
            EnrollmentClient.add_record,
            AddRecordRequest {
                host_id: host_id.to_owned(),
                role: role.to_owned(),
                data_macs: vec![mac.to_owned()],
                serial: serial.to_owned(),
                notes: notes.to_owned(),
                platform: platform as i32,
                ..AddRecordRequest::default()
            },
            "AddRecordResponse"
        )
    }

    /// `Enrollment.ListRecords`: the inventory records, as `records`.
    pub fn list_records(&self) -> Result<Value, Status> {
        rpc!(
            self,
            EnrollmentClient.list_records,
            ListRecordsRequest::default(),
            "ListRecordsResponse"
        )
    }

    /// `Hosts.List`: the hosts the coordinator has heard from since it started and the enrolled
    /// hosts it has not, as `hosts`.
    pub fn list_hosts(&self) -> Result<Value, Status> {
        rpc!(
            self,
            HostsClient.list,
            ListHostsRequest::default(),
            "ListHostsResponse"
        )
    }

    /// `Hosts.Get` of `node_id`: the host, as `host`.
    pub fn get_host(&self, node_id: &str) -> Result<Value, Status> {
        rpc!(
            self,
            HostsClient.get,
            GetHostRequest {
                node_id: node_id.to_owned(),
            },
            "GetHostResponse"
        )
    }

    /// `Hosts.Retire` of `node_id`.
    pub fn retire(&self, node_id: &str, reason: &str) -> Result<Value, Status> {
        rpc!(
            self,
            HostsClient.retire,
            RetireRequest {
                node_id: node_id.to_owned(),
                reason: reason.to_owned(),
            },
            "RetireResponse"
        )
    }

    /// `Services.List`: the services the coordinator manages, as `services`.
    pub fn list_services(&self) -> Result<Value, Status> {
        rpc!(
            self,
            ServicesClient.list,
            ListServicesRequest::default(),
            "ListServicesResponse"
        )
    }

    /// `Services.Assign` of `service` to the host `node_id`.
    pub fn assign(&self, node_id: &str, service: &str) -> Result<Value, Status> {
        rpc!(
            self,
            ServicesClient.assign,
            AssignRequest {
                node_id: node_id.to_owned(),
                service: service.to_owned(),
                ..AssignRequest::default()
            },
            "AssignResponse"
        )
    }

    /// `Services.Unassign` of `service` from the host `node_id`, ordering the host to uninstall
    /// it when `uninstall`.
    pub fn unassign(&self, node_id: &str, service: &str, uninstall: bool) -> Result<Value, Status> {
        rpc!(
            self,
            ServicesClient.unassign,
            UnassignRequest {
                node_id: node_id.to_owned(),
                service: service.to_owned(),
                uninstall,
                ..UnassignRequest::default()
            },
            "UnassignResponse"
        )
    }

    /// `Releases.SetDesired` of `service` at `version` by targets alone: `targets` by platform and
    /// no release index and no index digest, as CI's set of a release whose entries are all native
    /// gives them.
    pub fn set_desired_targets_alone(
        &self,
        service: &str,
        version: &str,
        targets: BTreeMap<String, TargetList>,
    ) -> Result<Value, Status> {
        rpc!(
            self,
            ReleasesClient.set_desired,
            SetDesiredRequest {
                service: service.to_owned(),
                desired: Some(DesiredState {
                    version: version.to_owned(),
                    ..DesiredState::default()
                }),
                targets: targets.into_iter().collect(),
                ..SetDesiredRequest::default()
            },
            "SetDesiredResponse"
        )
    }

    /// `Repositories.Put` of `repository`: adds it to the coordinator's registry of package
    /// repositories, or replaces the one of its name. The reply is the repository as the registry
    /// now holds it, as `repository`, with the audit entry as `entry`.
    pub fn put_repository(&self, repository: Repository) -> Result<Value, Status> {
        rpc!(
            self,
            RepositoriesClient.put,
            PutRepositoryRequest {
                repository: Some(repository),
                reason: "privileged test".to_owned(),
            },
            "PutRepositoryResponse"
        )
    }

    /// `Releases.Force` of `service` to `version` on the host `node_id`.
    pub fn force(
        &self,
        service: &str,
        version: &str,
        node_id: &str,
        reason: &str,
    ) -> Result<Value, Status> {
        rpc!(
            self,
            ReleasesClient.force,
            ForceRequest {
                service: service.to_owned(),
                version: version.to_owned(),
                reason: reason.to_owned(),
                targets: Some(Targets::Hosts(HostList {
                    node_ids: vec![node_id.to_owned()],
                })),
                ..ForceRequest::default()
            },
            "ForceResponse"
        )
    }

    /// `Releases.Converge` of `service`: how far its hosts are from its desired version.
    pub fn converge(&self, service: &str) -> Result<Value, Status> {
        rpc!(
            self,
            ReleasesClient.converge,
            ConvergeRequest {
                service: service.to_owned(),
            },
            "ConvergeResponse"
        )
    }

    /// `Releases.Status` of `service`.
    pub fn status(&self, service: &str) -> Result<Value, Status> {
        rpc!(
            self,
            ReleasesClient.status,
            StatusRequest {
                service: service.to_owned(),
            },
            "StatusResponse"
        )
    }
}
