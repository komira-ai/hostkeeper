//! The `Console` calls while the coordinator reads no console operators file: each is answered
//! with a refusal of the caller, never `UNIMPLEMENTED`, and none writes or contacts anything.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use crate::marks_support::{operator, rig};
use crate::operator::Caller;
use crate::testutil::Rig;

/// What each of the five calls answers `caller`, with every request field set, in the order of
/// the proto.
fn answers(rig: &Rig, caller: &Caller) -> [Result<(), Refusal>; 5] {
    let coordinator = &rig.coordinator;
    let reason = "boot order".to_owned();
    let host_id = "node-02".to_owned();
    [
        coordinator
            .console_open(
                caller,
                &proto::OpenConsoleRequest {
                    host_id: host_id.clone(),
                    reason: reason.clone(),
                },
            )
            .map(drop),
        coordinator
            .console_power(
                caller,
                &proto::ConsolePowerRequest {
                    host_id: host_id.clone(),
                    action: proto::PowerAction::Status.into(),
                    reason: reason.clone(),
                },
            )
            .map(drop),
        coordinator
            .console_set_boot_device(
                caller,
                &proto::SetBootDeviceRequest {
                    host_id: host_id.clone(),
                    device: proto::BootDevice::Disk.into(),
                    persistent: true,
                    efi: true,
                    reason,
                },
            )
            .map(drop),
        coordinator
            .console_list(caller, &proto::ListConsoleSessionsRequest {})
            .map(drop),
        coordinator
            .console_close(caller, &proto::CloseConsoleRequest { host_id })
            .map(drop),
    ]
}

/// Proves: with no console operators file, every `Console` call refuses an operator
/// `no_console_access`, a CI token `forbidden` and no token `unauthorized`, whatever the request
/// holds, and none of them writes an audit line.
///
/// Catches: a console call that answers an operator outside the operators file (no file means
/// nobody), one that lets a CI token through, and a refusal of someone not in the file that is
/// audited (section 12 of the design audits refusals of operators in the file only).
#[test]
fn every_console_call_refuses_while_no_operators_file_names_anyone() {
    let rig = rig("console-nobody", "");
    let before = rig.coordinator.ops().audit_log().expect("the log");
    let cases = [
        (operator(), Refusal::NoConsoleAccess),
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ];
    for (caller, refusal) in cases {
        assert_eq!(answers(&rig, &caller), [Err(refusal); 5], "{caller:?}");
    }
    assert_eq!(rig.coordinator.ops().audit_log().expect("the log"), before);
}
