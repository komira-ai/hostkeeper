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

/// A probe of `node-02` by `alice`, reading 60 seconds at most and 5 without a byte.
fn probe_request() -> proto::ProbeConsoleRequest {
    proto::ProbeConsoleRequest {
        host_id: NODE.to_owned(),
        reason: "probe it".to_owned(),
        wait_secs: 60,
        quiet_secs: 5,
        max_bytes: 0,
    }
}

/// Proves: through the adapter, a probe answers what its handler found (a host at its login
/// prompt); a payload another session holds is `UNAVAILABLE` `bmc_sol_busy`; and a caller who
/// leaves while the probe reads does not cut it short: the read still ends at its quiet time, the
/// payload is deactivated, the session leaves the table and its close line is written.
///
/// Catches: an adapter whose probe, dropped with its caller, leaves the BMC's payload active and
/// the host's session listed until someone cleans up by hand.
#[tokio::test]
async fn a_probe_answers_and_cleans_up_when_its_caller_leaves() {
    use crate::console::attach_support::Live;
    use crate::console::stream_tests::{activated_then_deactivated, asked, serial};

    let (live, timers) = Live::timed("grpc-console-probe");
    let host = serial(&live);
    let service = ConsoleService::new(Arc::clone(&live.coordinator));
    let probing = service.probe(from(caller("alice"), probe_request()));
    let said = async {
        timers.armed(2).await;
        host.say(b"node-02 login: ").await;
        timers.armed(3).await;
        timers.fire(std::time::Duration::from_secs(5));
    };
    let (answer, ()) = tokio::join!(probing, said);
    let answer = answer.expect("probed").into_inner();
    assert_eq!(
        answer.findings[0].class(),
        proto::ConsoleProbeClass::LoginPrompt
    );

    let (live, _timers) = Live::timed("grpc-console-probe-busy");
    live.fixture.tool.session(&[NODE_BMC_ANSWER, " 01 01 00\n"]);
    let service = ConsoleService::new(Arc::clone(&live.coordinator));
    let refused = service
        .probe(from(caller("alice"), probe_request()))
        .await
        .expect_err("busy");
    assert_eq!(
        (refused.code(), refused.message()),
        (Code::Unavailable, "bmc_sol_busy")
    );

    let (live, timers) = Live::timed("grpc-console-probe-left");
    let _host = serial(&live);
    let service = ConsoleService::new(Arc::clone(&live.coordinator));
    let probing = service.probe(from(caller("alice"), probe_request()));
    tokio::select! {
        _ = probing => panic!("the probe ended before its read did"),
        () = timers.armed(2) => {}
    }
    timers.fire(std::time::Duration::from_secs(5));
    live.gone(NODE).await;
    assert_eq!(asked(&live), activated_then_deactivated());
    let (by, action, ..) = live.lines().pop().expect("a line");
    assert_eq!((by.as_str(), action.as_str()), ("alice", "console_close"));
}
