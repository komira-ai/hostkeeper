//! The `Console` adapter over the coordinator: each method answers what its handler answers, off
//! the runtime's threads, and a refusal is the status of its value.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::console_server::Console as _;
use tonic::{Code, Request};

use crate::console::support::{Fixture, NODE, NODE_BMC_ANSWER, caller};
use crate::grpc::Admitted;
use crate::grpc::operator_console::ConsoleService;
use crate::operator::Caller;

/// A request of `message` the router admitted for `caller`.
fn from<T>(caller: Caller, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .extensions_mut()
        .insert(Admitted::Operator { caller });
    request
}

/// Proves: through the adapter, an operator in the console operators file opens, lists and
/// closes a host's session and reads its power; a boot device call whose tool cannot start is
/// `UNAVAILABLE` `bmc_failed`; and a CI caller is `PERMISSION_DENIED` `forbidden`.
///
/// Catches: an adapter that calls another handler than its method's, or loses a refusal's code.
#[tokio::test]
async fn each_console_method_answers_what_its_handler_answers() {
    let mut fixture = Fixture::new("grpc-console-adapter");
    fixture
        .tool
        .session(&[NODE_BMC_ANSWER, "Chassis Power is on\n"]);
    let coordinator = std::mem::replace(
        &mut fixture.rig.coordinator,
        crate::marks_support::start(&fixture.rig.dir, &fixture.rig.clock, ""),
    );
    let service = ConsoleService::new(Arc::new(coordinator));
    let alice = || caller("alice");
    let opened = service
        .open(from(
            alice(),
            proto::OpenConsoleRequest {
                host_id: NODE.to_owned(),
                reason: "watch".to_owned(),
            },
        ))
        .await
        .expect("opened")
        .into_inner();
    assert_eq!(opened.kind(), proto::ConsoleKind::Serial);
    let listed = service
        .list(from(alice(), proto::ListConsoleSessionsRequest {}))
        .await
        .expect("listed")
        .into_inner();
    assert_eq!(listed.sessions.len(), 1);
    service
        .close(from(
            alice(),
            proto::CloseConsoleRequest {
                host_id: NODE.to_owned(),
            },
        ))
        .await
        .expect("closed");
    let power = service
        .power(from(
            alice(),
            proto::ConsolePowerRequest {
                host_id: NODE.to_owned(),
                action: proto::PowerAction::Status.into(),
                reason: "checking".to_owned(),
            },
        ))
        .await
        .expect("read")
        .into_inner();
    assert_eq!(power.state(), proto::PowerState::On);
    let boot = proto::SetBootDeviceRequest {
        host_id: NODE.to_owned(),
        device: proto::BootDevice::Disk.into(),
        persistent: true,
        efi: false,
        reason: "boot order".to_owned(),
    };
    let refused = service
        .set_boot_device(from(alice(), boot.clone()))
        .await
        .expect_err("no tool");
    assert_eq!(
        (refused.code(), refused.message()),
        (Code::Unavailable, "bmc_failed")
    );
    let refused = service
        .set_boot_device(from(Caller::Ci, boot))
        .await
        .expect_err("CI");
    assert_eq!(
        (refused.code(), refused.message()),
        (Code::PermissionDenied, "forbidden")
    );
}
