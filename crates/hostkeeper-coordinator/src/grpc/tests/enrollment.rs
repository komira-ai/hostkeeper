//! The adapter of `Enrollment` over the coordinator: each method answers what the coordinator's
//! handler answers, each refusal is the status of the value the cause names, and a call the
//! router admitted no operator for is refused as such.

use std::fmt::Debug;
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::enrollment_server::Enrollment as _;
use tonic::{Code, Request, Response, Status};

use super::operator::{World, as_operator};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, approval, enroll_request};
use crate::grpc::Admitted;
use crate::grpc::operator::EnrollmentService;
use crate::operator::Caller;
use crate::testutil::{OPERATOR, Rig};

/// A world whose coordinator has the enrollment of `enrollment`.
fn world_over(name: &str, enrollment: &EnrollmentRig) -> World {
    let rig = Rig::new(name);
    let started =
        Rig::start(&rig.dir, &rig.clock).with_enrollment(Arc::clone(&enrollment.enrollment));
    World::over(rig, started)
}

fn service(world: &World) -> EnrollmentService {
    EnrollmentService::new(world.coordinator.clone())
}

/// Asserts that `result` is a refusal of `code` whose message is exactly `token`.
fn assert_refused<T: Debug>(result: Result<Response<T>, Status>, code: Code, token: &str) {
    let status = result.expect_err("the call is refused");
    assert_eq!((status.code(), status.message()), (code, token));
}

/// A call that carries a bearer the router found to be CI's.
fn as_ci_caller<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(Admitted::Operator {
        caller: Caller::Ci,
        bearer: String::new(),
    });
    request
}

/// `message` as a call with no caller when `anonymous`, and as one by CI otherwise.
fn caller<T>(anonymous: bool, message: T) -> Request<T> {
    if anonymous {
        Request::new(message)
    } else {
        as_ci_caller(message)
    }
}

/// An add of `host` with the data MAC `mac`.
fn add_of(host: &str, mac: &str) -> proto::AddRecordRequest {
    proto::AddRecordRequest {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: vec![mac.to_owned()],
        ..proto::AddRecordRequest::default()
    }
}

/// Proves that every method of `Enrollment` refuses a call with no caller `unauthorized`
/// (`UNAUTHENTICATED`) and a call by CI `forbidden` (`PERMISSION_DENIED`), and changes nothing.
#[tokio::test]
async fn every_method_refuses_a_caller_that_is_not_the_operator() {
    let enrollment = EnrollmentRig::new("grpc-enrollment-callers");
    let world = world_over("grpc-enrollment-callers-coordinator", &enrollment);
    let s = service(&world);
    let key = HostKey::new();
    for (code, token, anonymous) in [
        (Code::Unauthenticated, "unauthorized", true),
        (Code::PermissionDenied, "forbidden", false),
    ] {
        assert_refused(
            s.list(caller(anonymous, proto::ListPendingRequest::default()))
                .await,
            code,
            token,
        );
        assert_refused(
            s.approve(caller(anonymous, approval(&key, "s1"))).await,
            code,
            token,
        );
        let reject = proto::RejectRequest {
            request_id: key.key_hash().to_hex(),
            reason: String::new(),
        };
        assert_refused(s.reject(caller(anonymous, reject)).await, code, token);
        assert_refused(
            s.add_record(caller(anonymous, add_of("db-1", "0c:c4:7a:00:00:02")))
                .await,
            code,
            token,
        );
        assert_refused(
            s.list_records(caller(anonymous, proto::ListRecordsRequest::default()))
                .await,
            code,
            token,
        );
        assert_refused(
            s.remove_record(caller(anonymous, proto::RemoveRecordRequest::default()))
                .await,
            code,
            token,
        );
    }
    let c = &world.coordinator;
    let listed = s
        .list_records(as_operator(c, proto::ListRecordsRequest::default()).await)
        .await
        .expect("listed");
    assert!(listed.get_ref().records.is_empty(), "nothing was recorded");
}

/// Proves that `List` and `Approve` answer what the coordinator's handlers answer: the pending
/// request is listed with its key hash, an approval of it names the host and says the
/// certificate comes at `Poll`, and an approval of a request nobody made is `NOT_FOUND`
/// `unknown_request`.
#[tokio::test]
async fn list_and_approve_answer_what_the_coordinator_answers() {
    let enrollment = EnrollmentRig::new("grpc-enrollment-flow");
    let world = world_over("grpc-enrollment-flow-coordinator", &enrollment);
    let c = &world.coordinator;
    let s = service(&world);
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");

    let listed = s
        .list(as_operator(c, proto::ListPendingRequest::default()).await)
        .await
        .expect("listed")
        .into_inner();
    let operator = c.caller(Some(OPERATOR)).await;
    let want = c
        .enrollment_list(&operator, &proto::ListPendingRequest::default())
        .expect("listed directly");
    assert_eq!(listed, want);
    assert_eq!(listed.requests.len(), 1);
    assert_eq!(listed.requests[0].key_sha256, key.key_hash().to_hex());

    let approved = s
        .approve(as_operator(c, approval(&key, "s1")).await)
        .await
        .expect("approved")
        .into_inner();
    assert_eq!(approved.host_id, "s1");
    assert!(approved.certificate_issued_at_poll);
    assert!(approved.entry.is_some(), "the approval is audited");

    assert_refused(
        s.approve(as_operator(c, approval(&HostKey::new(), "s2")).await)
            .await,
        Code::NotFound,
        "unknown_request",
    );
    assert_refused(
        s.list(
            as_operator(
                c,
                proto::ListPendingRequest {
                    limit: 1,
                    page_token: "not a token".to_owned(),
                },
            )
            .await,
        )
        .await,
        Code::InvalidArgument,
        "invalid_request",
    );
}

/// Proves that a coordinator with no enrollment lists no pending request, and refuses an
/// approval `NOT_FOUND` `unknown_request`.
#[tokio::test]
async fn a_coordinator_without_enrollment_lists_nothing_and_approves_nothing() {
    let world = World::new("grpc-enrollment-none");
    let c = &world.coordinator;
    let s = service(&world);
    let listed = s
        .list(as_operator(c, proto::ListPendingRequest::default()).await)
        .await
        .expect("listed");
    assert_eq!(*listed.get_ref(), proto::ListPendingResponse::default());
    assert_refused(
        s.approve(as_operator(c, approval(&HostKey::new(), "s1")).await)
            .await,
        Code::NotFound,
        "unknown_request",
    );
}

/// Proves the inventory calls: a record is added with its MAC in the one spelling and audited,
/// listed, refused when added again (`ALREADY_EXISTS` `record_exists`) or when another host id
/// claims its MAC (`record_conflict`) or is no host id (`INVALID_ARGUMENT` `invalid_host_id`),
/// and removed, after which a second removal is `NOT_FOUND` `no_record`.
#[tokio::test]
async fn records_are_added_listed_and_removed_and_each_refusal_has_its_status() {
    let world = World::new("grpc-enrollment-records");
    let c = &world.coordinator;
    let s = service(&world);

    let added = s
        .add_record(as_operator(c, add_of("db-1", "0C-C4-7A-00-00-02")).await)
        .await
        .expect("added")
        .into_inner();
    let record = added.record.expect("the record as kept");
    assert_eq!(
        (record.host_id.as_str(), record.data_macs.as_slice()),
        ("db-1", ["0c:c4:7a:00:00:02".to_owned()].as_slice())
    );
    assert!(added.entry.is_some(), "the add is audited");

    assert_refused(
        s.add_record(as_operator(c, add_of("db-1", "0c:c4:7a:00:00:09")).await)
            .await,
        Code::AlreadyExists,
        "record_exists",
    );
    assert_refused(
        s.add_record(as_operator(c, add_of("db-2", "0c:c4:7a:00:00:02")).await)
            .await,
        Code::AlreadyExists,
        "record_conflict",
    );
    assert_refused(
        s.add_record(as_operator(c, add_of("Not A Host!", "0c:c4:7a:00:00:03")).await)
            .await,
        Code::InvalidArgument,
        "invalid_host_id",
    );

    let listed = s
        .list_records(as_operator(c, proto::ListRecordsRequest::default()).await)
        .await
        .expect("listed")
        .into_inner();
    assert_eq!(listed.records, vec![record]);
    assert!(listed.next_page_token.is_empty());
    assert_refused(
        s.list_records(
            as_operator(
                c,
                proto::ListRecordsRequest {
                    limit: 1,
                    page_token: "Not A Host!".to_owned(),
                },
            )
            .await,
        )
        .await,
        Code::InvalidArgument,
        "invalid_request",
    );

    let remove = |host: &str| proto::RemoveRecordRequest {
        host_id: host.to_owned(),
        reason: "rebuilt".to_owned(),
    };
    let removed = s
        .remove_record(as_operator(c, remove("db-1")).await)
        .await
        .expect("removed");
    assert!(removed.get_ref().entry.is_some(), "the removal is audited");
    assert_refused(
        s.remove_record(as_operator(c, remove("db-1")).await).await,
        Code::NotFound,
        "no_record",
    );
}
