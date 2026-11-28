//! `Hardware.Log` on the operator API through `crate::grpc::route`: a host's records, written by
//! the coordinator with the serial of the inventory record the host is enrolled against, are
//! answered over gRPC and over the gateway the console calls; a caller that is no operator and a
//! request the coordinator cannot read are refused with their statuses; and the log a coordinator
//! keeps in its file is the one a coordinator started again on that file answers. No socket.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hyper::StatusCode;
use serde_json::Value;
use tonic::Code;

use super::hosts::world_over;
use super::operator::{AUDIT_UNIX, World};
use super::support::decode;
use super::targets::{gateway, grpc, refused};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enroll_request};
use crate::hardware_files::{FileLog, hardware_events_path};
use crate::hardware_log::tests::{event, health};
use crate::marks_support::operator;
use crate::testutil::{OPERATOR, Rig, ci_token};

/// The path of `Hardware.Log`.
const LOG: &str = "/hostkeeper.operator.v1.Hardware/Log";

/// The data MAC, BMC MAC and serial of the inventory record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";
const DB1_BMC: &str = "0c:c4:7a:00:00:01";
const DB1_SERIAL: &str = "SN-1";

/// A coordinator over its own enrollment, holding the inventory record `db-1`, against which a
/// host claiming its MACs is enrolled as `db-1`.
fn enrolled_db1(name: &str) -> (EnrollmentRig, World) {
    let enrollment = EnrollmentRig::new(&format!("{name}-enrollment"));
    let world = world_over(name, &enrollment);
    world
        .coordinator
        .add_record(
            &operator(),
            proto::AddRecordRequest {
                host_id: "db-1".to_owned(),
                data_macs: vec![DB1_DATA.to_owned()],
                bmc_mac: DB1_BMC.to_owned(),
                serial: DB1_SERIAL.to_owned(),
                ..proto::AddRecordRequest::default()
            },
        )
        .expect("recorded");
    let key = HostKey::new();
    let mut request = enroll_request(&key, "m-1");
    let claims = request.claims.as_mut().expect("claims");
    claims.addresses = vec!["192.0.2.10".to_owned(), DB1_DATA.to_owned()];
    claims.bmc_mac = DB1_BMC.to_owned();
    let posted = enrollment.enrollment.enroll(request).expect("posted");
    assert_eq!(posted.state, EnrollState::Pending as i32);
    let approved = world
        .coordinator
        .enrollment_approve(
            &operator(),
            proto::ApproveRequest {
                request_id: key.key_hash().to_hex(),
                key_sha256: key.key_hash().to_hex(),
                host_id: String::new(),
                role: "storage".to_owned(),
                reason: "built today".to_owned(),
                ..proto::ApproveRequest::default()
            },
        )
        .expect("approved");
    assert_eq!(approved.host_id, "db-1");
    (enrollment, world)
}

/// The host `name`.
fn host(name: &str) -> hostkeeper_wire::HostId {
    name.parse().expect("a host id")
}

/// Proves: the records of a host enrolled against an inventory record are written with that
/// record's serial, and of a host enrolled against none with no serial; `Hardware.Log` over gRPC
/// answers them newest first with the host, the serial, the coordinator's time, the boot and
/// every field the daemon sent, and filters by the serial; the gateway answers the same records
/// as JSON, times as strings and the enums by their names.
///
/// Catches: a log not served (no `Hardware` on the control listener), a record that does not
/// follow its machine (no serial), and the gateway and gRPC answering apart.
#[tokio::test]
async fn the_log_answers_a_hosts_records_with_its_machines_serial() {
    let (_enrollment, world) = enrolled_db1("grpc-hardware-log");
    let mut older = event("sel:01a4@1790000000");
    older.time_unix -= 60;
    world
        .coordinator
        .write_hardware_events(&host("db-1"), &health(vec![older.clone()]))
        .expect("written");
    world
        .coordinator
        .write_hardware_events(&host("web-1"), &health(vec![event("panic:2b6f")]))
        .expect("written");
    let (code, message, answer) = grpc(
        &world,
        LOG,
        Some(OPERATOR),
        proto::HardwareLogRequest::default(),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let listed: proto::HardwareLogResponse = decode(&answer.expect("an answer"));
    let stamped = |host: &str, serial: &str, event| proto::HardwareRecord {
        host_id: host.to_owned(),
        machine_serial: serial.to_owned(),
        observed_unix: AUDIT_UNIX,
        boot_id: "b7c1".to_owned(),
        by: String::new(),
        event: Some(event),
    };
    assert_eq!(
        listed,
        proto::HardwareLogResponse {
            records: vec![
                stamped("web-1", "", event("panic:2b6f")),
                stamped("db-1", DB1_SERIAL, older),
            ],
            next_page_token: String::new(),
        }
    );

    let (code, message, answer) = grpc(
        &world,
        LOG,
        Some(OPERATOR),
        proto::HardwareLogRequest {
            serial: Some(DB1_SERIAL.to_owned()),
            ..proto::HardwareLogRequest::default()
        },
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let by_serial: proto::HardwareLogResponse = decode(&answer.expect("an answer"));
    assert_eq!(by_serial.records, listed.records[1..]);

    let (status, body) = gateway(&world, LOG, OPERATOR, r#"{"hostId":"db-1"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let json: Value = serde_json::from_str(&body).expect("JSON");
    let record = &json["records"][0];
    assert_eq!(json["records"].as_array().map(Vec::len), Some(1), "{body}");
    assert_eq!(record["hostId"], "db-1");
    assert_eq!(record["machineSerial"], DB1_SERIAL);
    assert_eq!(record["observedUnix"], AUDIT_UNIX.to_string());
    assert_eq!(record["event"]["eventId"], "sel:01a4@1790000000");
    assert_eq!(record["event"]["component"], "HARDWARE_COMPONENT_MEMORY");
    assert_eq!(record["event"]["severity"], "HARDWARE_SEVERITY_CRITICAL");
}

/// Proves: `Hardware.Log` is `UNAUTHENTICATED` `unauthorized` with no bearer and
/// `PERMISSION_DENIED` `forbidden` with CI's token, over gRPC; and `INVALID_ARGUMENT`
/// `invalid_request` for a host id that is no node id and a page token this coordinator did not
/// give, over gRPC and the gateway alike.
///
/// Catches: the log read for a caller that may not read it, and a refusal the adapter answers
/// with another status than the proto's comment names.
#[tokio::test]
async fn the_log_refuses_with_the_status_of_each_refusal() {
    let world = World::new("grpc-hardware-refusals");
    let any = proto::HardwareLogRequest::default;
    let (code, message, _) = grpc(&world, LOG, None, any()).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unauthenticated, "unauthorized")
    );
    let ci = ci_token();
    let (code, message, _) = grpc(&world, LOG, Some(&ci), any()).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::PermissionDenied, "forbidden")
    );
    for request in [
        proto::HardwareLogRequest {
            host_id: Some("Not A Host".to_owned()),
            ..any()
        },
        proto::HardwareLogRequest {
            page_token: "a token".to_owned(),
            ..any()
        },
    ] {
        let (code, message, _) = grpc(&world, LOG, Some(OPERATOR), request).await;
        assert_eq!(
            (code, message.as_str()),
            (Code::InvalidArgument, "invalid_request")
        );
    }
    let (status, body) = gateway(&world, LOG, OPERATOR, r#"{"pageToken":"a token"}"#).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::BAD_REQUEST,
            refused("INVALID_ARGUMENT", "invalid_request")
        )
    );
}

/// Proves: the records a coordinator wrote to the file beside its state file are answered, the
/// same, by a coordinator started again on that file.
///
/// Catches: a log the coordinator keeps in its memory alone, lost at a restart.
#[tokio::test]
async fn a_coordinator_started_again_answers_the_log_it_kept() {
    let rig = Rig::new("grpc-hardware-restart");
    let path = hardware_events_path(&rig.dir.path().join("desired.json"));
    let on_file =
        || Rig::start(&rig.dir, &rig.clock).with_hardware_log(Arc::new(FileLog::new(path.clone())));
    let first = World::over(Rig::new("grpc-hardware-restart-first"), on_file());
    first
        .coordinator
        .write_hardware_events(&host("db-1"), &health(vec![event("e1"), event("e2")]))
        .expect("written");
    let (_, _, before) = grpc(
        &first,
        LOG,
        Some(OPERATOR),
        proto::HardwareLogRequest::default(),
    )
    .await;
    let before: proto::HardwareLogResponse = decode(&before.expect("an answer"));
    assert_eq!(before.records.len(), 2);
    drop(first);
    let again = World::over(Rig::new("grpc-hardware-restart-again"), on_file());
    let (code, message, after) = grpc(
        &again,
        LOG,
        Some(OPERATOR),
        proto::HardwareLogRequest::default(),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let after: proto::HardwareLogResponse = decode(&after.expect("an answer"));
    assert_eq!(after, before);
}
