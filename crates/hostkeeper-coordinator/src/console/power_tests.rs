//! `Console/Power` and `Console/SetBootDevice` over the scripted BMC tool: who may call, what is
//! checked before the BMC is reached, what is audited, and what the BMC's answers become.

use hostkeeper_console::bmc::{MAC_READ, POWER_STATUS};
use hostkeeper_console::ipmi::ShellError;
use hostkeeper_console::scripted::Seen;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use super::support::{Fixture, MAC, NODE, NODE_BMC_ANSWER, PASSWORD, caller, line, write_files};
use crate::operator::Caller;
use crate::store::audit_failure;

fn power(host: &str, action: proto::PowerAction, reason: &str) -> proto::ConsolePowerRequest {
    proto::ConsolePowerRequest {
        host_id: host.to_owned(),
        action: action.into(),
        reason: reason.to_owned(),
    }
}

fn boot(device: proto::BootDevice, persistent: bool, efi: bool) -> proto::SetBootDeviceRequest {
    proto::SetBootDeviceRequest {
        host_id: NODE.to_owned(),
        device: device.into(),
        persistent,
        efi,
        reason: "boot order".to_owned(),
    }
}

/// The arguments of a session at `node-02`'s BMC.
fn bmc_args() -> Vec<String> {
    [
        "-I",
        "lanplus",
        "-H",
        "192.0.2.12",
        "-U",
        "ADMIN",
        "-E",
        "-C",
        "3",
        "-L",
        "ADMINISTRATOR",
        "-N",
        "1",
        "-R",
        "2",
        "shell",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Proves: a cycle by an operator allowed power reaches the BMC of the host's record at the
/// targets file's address, as the credential's user with the password for the environment only,
/// answers the state before and the command sent, and writes exactly one `power_action` line by
/// the operator, for the host, with the reason and the action.
///
/// Catches: a power command unaudited, audited under another name or host, or without its
/// action; and a BMC reached at another address than the record's MAC maps to.
#[test]
fn power_writes_one_audit_line_with_actor_host_and_action() {
    let fixture = Fixture::new("console-power-audit");
    fixture.tool.session(&[
        NODE_BMC_ANSWER,
        "Chassis Power is on\n",
        "Chassis Power Control: Cycle\n",
    ]);
    let answer = fixture.rig.coordinator.console_power(
        &caller("alice"),
        &power(NODE, proto::PowerAction::Cycle, "  stuck at the PXE menu  "),
    );
    assert_eq!(
        answer,
        Ok(proto::ConsolePowerResponse {
            state: proto::PowerState::On.into(),
            sent: proto::PowerAction::Cycle.into(),
        })
    );
    assert_eq!(
        fixture.lines(),
        [line(
            "alice",
            "power_action",
            NODE,
            "stuck at the PXE menu; action=cycle"
        )]
    );
    assert_eq!(
        fixture.tool.seen(),
        [
            Seen::Open {
                args: bmc_args(),
                escape: None,
                password: PASSWORD.to_owned(),
            },
            Seen::Ask(MAC_READ.to_owned()),
            Seen::Ask(POWER_STATUS.to_owned()),
            Seen::Ask("chassis power cycle".to_owned()),
            Seen::Closed,
        ]
    );
}

/// Proves: a cycle of a host that is off answers that `on` was sent, and the off state.
///
/// Catches: the answer telling the operator a cycle was sent when the BMC was asked to power on.
#[test]
fn cycle_on_an_off_host_answers_on() {
    let fixture = Fixture::new("console-power-off-cycle");
    fixture.tool.session(&[
        NODE_BMC_ANSWER,
        "Chassis Power is off\n",
        "Chassis Power Control: Up/On\n",
    ]);
    let answer = fixture.rig.coordinator.console_power(
        &caller("alice"),
        &power(NODE, proto::PowerAction::Cycle, "boot order"),
    );
    assert_eq!(
        answer,
        Ok(proto::ConsolePowerResponse {
            state: proto::PowerState::Off.into(),
            sent: proto::PowerAction::On.into(),
        })
    );
}

/// Proves: the boot device call sends the device with its options, answers the flags read back
/// (here not what was asked: the BMC holds a next-boot override), and writes one
/// `boot_device_set` line with the device and both options; an answer with no valid override is
/// unspecified.
///
/// Catches: the request echoed as the answer, and a line that leaves out whether the override
/// was persistent.
#[test]
fn the_boot_flags_are_read_back_into_the_answer() {
    let fixture = Fixture::new("console-boot-read-back");
    fixture.tool.session(&[
        NODE_BMC_ANSWER,
        "Set Boot Device to disk\n",
        "Boot parameter data: a008000000\n",
    ]);
    fixture.tool.session(&[
        NODE_BMC_ANSWER,
        "Set Boot Device to pxe\n",
        "Boot parameter data: 0000000000\n",
    ]);
    let coordinator = &fixture.rig.coordinator;
    assert_eq!(
        coordinator
            .console_set_boot_device(&caller("alice"), &boot(proto::BootDevice::Disk, true, true)),
        Ok(proto::SetBootDeviceResponse {
            device: proto::BootDevice::Disk.into(),
            persistent: false,
            efi: true,
        })
    );
    assert_eq!(
        coordinator.console_set_boot_device(
            &caller("alice"),
            &boot(proto::BootDevice::Pxe, false, false)
        ),
        Ok(proto::SetBootDeviceResponse::default())
    );
    assert_eq!(
        fixture.tool.seen()[2],
        Seen::Ask("chassis bootdev disk options=persistent,efiboot".to_owned())
    );
    assert_eq!(
        fixture.lines(),
        [
            line(
                "alice",
                "boot_device_set",
                NODE,
                "boot order; device=disk; persistent=true; efi=true"
            ),
            line(
                "alice",
                "boot_device_set",
                NODE,
                "boot order; device=pxe; persistent=false; efi=false"
            ),
        ]
    );
    fixture.tool.session(&[
        NODE_BMC_ANSWER,
        "Set Boot Device to pxe\n",
        "Boot parameter data: 8004000000\n",
    ]);
    assert_eq!(
        coordinator
            .console_set_boot_device(
                &caller("alice"),
                &boot(proto::BootDevice::Pxe, false, false)
            )
            .map(|flags| flags.device()),
        Ok(proto::BootDevice::Pxe)
    );
    let bios = boot(proto::BootDevice::Bios, false, false);
    fixture.tool.session(&[
        NODE_BMC_ANSWER,
        "Set Boot Device to bios\n",
        "Boot parameter data: 8018000000\n",
    ]);
    assert_eq!(
        coordinator
            .console_set_boot_device(&caller("alice"), &bios)
            .map(|flags| flags.device()),
        Ok(proto::BootDevice::Bios)
    );
}

/// Proves: each of status, on, off, soft and reset is sent as its own command and answered as
/// sent.
///
/// Catches: an action of the proto mapped to another command.
#[test]
fn every_power_action_reaches_the_bmc_as_itself() {
    let fixture = Fixture::new("console-power-actions");
    let cases = [
        (proto::PowerAction::Status, None),
        (proto::PowerAction::On, Some(("on", "Up/On"))),
        (proto::PowerAction::Off, Some(("off", "Down/Off"))),
        (proto::PowerAction::Soft, Some(("soft", "Soft"))),
        (proto::PowerAction::Reset, Some(("reset", "Reset"))),
    ];
    for (action, sent) in cases {
        let acknowledgement = sent.map(|(_, ack)| format!("Chassis Power Control: {ack}\n"));
        let mut answers = vec![NODE_BMC_ANSWER, "Chassis Power is on\n"];
        answers.extend(acknowledgement.as_deref());
        fixture.tool.session(&answers);
        let answer = fixture
            .rig
            .coordinator
            .console_power(&caller("alice"), &power(NODE, action, "checking"));
        assert_eq!(answer.map(|answer| answer.sent()), Ok(action), "{action:?}");
        if let Some((command, _)) = sent {
            let seen = fixture.tool.seen();
            assert_eq!(
                seen[seen.len() - 2],
                Seen::Ask(format!("chassis power {command}"))
            );
        }
    }
}

/// Proves: `Power` and `SetBootDevice` without a reason, or with one that is blank, are
/// `reason_required`; over 256 bytes once trimmed `reason_too_long` (256 is taken); with a control
/// character `invalid_request`; and `status` is not exempt. None of them writes a line or reaches
/// the BMC.
///
/// Catches: a power action made with no reason in its audit line, and a status read let through
/// without one.
#[test]
fn every_power_and_boot_device_call_needs_a_reason() {
    let fixture = Fixture::new("console-power-reason");
    let coordinator = &fixture.rig.coordinator;
    let alice = caller("alice");
    for (reason, refusal) in [
        ("", Refusal::ReasonRequired),
        (" \t ", Refusal::ReasonRequired),
        ("line\nbreak", Refusal::InvalidRequest),
    ] {
        assert_eq!(
            coordinator.console_power(&alice, &power(NODE, proto::PowerAction::Status, reason)),
            Err(refusal),
            "{reason:?}"
        );
        let mut request = boot(proto::BootDevice::Disk, true, false);
        request.reason = reason.to_owned();
        assert_eq!(
            coordinator.console_set_boot_device(&alice, &request),
            Err(refusal),
            "{reason:?}"
        );
    }
    assert_eq!(fixture.tool.seen(), []);
    assert_eq!(fixture.lines(), []);
}

/// Proves: a reason of 257 bytes once trimmed is refused `reason_too_long` and touches nothing;
/// one of exactly 256 bytes, with spaces around it, is taken whole.
///
/// Catches: a cap that is off by one, and a reason cut silently instead of refused.
#[test]
fn a_reason_over_256_bytes_is_refused() {
    let fixture = Fixture::new("console-power-reason-cap");
    let coordinator = &fixture.rig.coordinator;
    let long = "r".repeat(257);
    assert_eq!(
        coordinator.console_power(
            &caller("alice"),
            &power(NODE, proto::PowerAction::Status, &long)
        ),
        Err(Refusal::ReasonTooLong)
    );
    assert_eq!(fixture.tool.seen(), []);
    let exact = format!(" {} ", "r".repeat(256));
    fixture
        .tool
        .session(&[NODE_BMC_ANSWER, "Chassis Power is on\n"]);
    assert!(
        coordinator
            .console_power(
                &caller("alice"),
                &power(NODE, proto::PowerAction::Status, &exact)
            )
            .is_ok()
    );
    let lines = fixture.lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].3.starts_with(&"r".repeat(256)), "{lines:?}");
}

/// Proves: with the operators file given, a CI token is `forbidden` and no token
/// `unauthorized` for every call, a name not in the file is `no_console_access`, and none of them
/// is audited or reaches the BMC.
///
/// Catches: a console call that lets a CI token through, and a refusal of someone outside the
/// file written to the audit (section 12 audits refusals of operators in the file only).
#[test]
fn a_ci_token_is_forbidden() {
    let fixture = Fixture::new("console-power-ci");
    let coordinator = &fixture.rig.coordinator;
    let status = power(NODE, proto::PowerAction::Status, "checking");
    let disk = boot(proto::BootDevice::Disk, false, false);
    for (who, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
        (caller("carol"), Refusal::NoConsoleAccess),
    ] {
        assert_eq!(coordinator.console_power(&who, &status), Err(refusal));
        assert_eq!(
            coordinator.console_set_boot_device(&who, &disk),
            Err(refusal)
        );
    }
    assert_eq!(fixture.tool.seen(), []);
    assert_eq!(fixture.lines(), []);
}

/// Proves: an operator whose entry says `power = false` is refused `power_not_permitted` for
/// power and the boot device, nothing reaches the BMC, and each refusal is one `console_denied`
/// line naming the call and the cause.
///
/// Catches: an operator allowed only to watch the console cycling a host, and a denial left out
/// of the audit.
#[test]
fn power_calls_need_the_power_flag() {
    let fixture = Fixture::new("console-power-flag");
    let coordinator = &fixture.rig.coordinator;
    let bob = caller("bob");
    assert_eq!(
        coordinator.console_power(&bob, &power(NODE, proto::PowerAction::Off, "testing")),
        Err(Refusal::PowerNotPermitted)
    );
    assert_eq!(
        coordinator.console_set_boot_device(&bob, &boot(proto::BootDevice::Pxe, false, false)),
        Err(Refusal::PowerNotPermitted)
    );
    assert_eq!(fixture.tool.seen(), []);
    assert_eq!(
        fixture.lines(),
        [
            line(
                "bob",
                "console_denied",
                NODE,
                "testing; call=power; cause=power_not_permitted"
            ),
            line(
                "bob",
                "console_denied",
                NODE,
                "boot order; call=set_boot_device; cause=power_not_permitted"
            ),
        ]
    );
}

/// Proves: a request whose host id is not a node id, or whose action or device is unspecified or
/// a number this release does not know, is `invalid_request`, unaudited and unsent.
///
/// Catches: a zero action read as `status` (or a future one as some other), sent to a BMC.
#[test]
fn an_unnamed_action_device_or_host_is_an_invalid_request() {
    let fixture = Fixture::new("console-power-invalid");
    let coordinator = &fixture.rig.coordinator;
    let alice = caller("alice");
    let mut requests = vec![
        power("Not A Host!", proto::PowerAction::Status, "x"),
        power(NODE, proto::PowerAction::Unspecified, "x"),
    ];
    let mut future = power(NODE, proto::PowerAction::Status, "x");
    future.action = 99;
    requests.push(future);
    for request in requests {
        assert_eq!(
            coordinator.console_power(&alice, &request),
            Err(Refusal::InvalidRequest),
            "{request:?}"
        );
    }
    let mut future = boot(proto::BootDevice::Disk, false, false);
    future.device = 99;
    for request in [boot(proto::BootDevice::Unspecified, false, false), future] {
        assert_eq!(
            coordinator.console_set_boot_device(&alice, &request),
            Err(Refusal::InvalidRequest)
        );
    }
    let mut host = boot(proto::BootDevice::Disk, false, false);
    host.host_id = String::new();
    assert_eq!(
        coordinator.console_set_boot_device(&alice, &host),
        Err(Refusal::InvalidRequest)
    );
    assert_eq!(fixture.tool.seen(), []);
    assert_eq!(fixture.lines(), []);
}

/// Proves: a host that is not enrolled is `host_not_found`; a Mac, a host whose record has no BMC
/// MAC and a host that matches no record are `no_bmc_mac`; a BMC MAC the targets file does not
/// list is `no_address`; a host its claims match two records of is `ambiguous_record`; each is
/// one `console_denied` line with its cause, and none reaches a BMC.
///
/// Catches: a power command sent to an address guessed for a host, or to the BMC of one of two
/// records.
#[test]
fn a_host_without_one_known_bmc_is_refused_by_name() {
    let fixture = Fixture::new("console-power-hosts");
    fixture.record("db-1", &["0c:c4:7a:00:00:32"], "", "");
    fixture.enroll("m-db1", &["0c:c4:7a:00:00:32"], "", "", "db-1");
    fixture.record("db-2", &["0c:c4:7a:00:00:42"], "0c:c4:7a:00:00:41", "");
    fixture.enroll("m-db2", &["0c:c4:7a:00:00:42"], "", "", "db-2");
    fixture.enroll("m-web", &["0c:c4:7a:00:00:52"], "", "", "web-1");
    fixture.record("amb-a", &["0c:c4:7a:00:00:62"], "", "");
    fixture.record("amb-b", &["0c:c4:7a:00:00:72"], "", "");
    fixture.enroll(
        "m-amb",
        &["0c:c4:7a:00:00:62", "0c:c4:7a:00:00:72"],
        "",
        "",
        "amb-1",
    );
    let coordinator = &fixture.rig.coordinator;
    let cases = [
        ("node-99", Refusal::HostNotFound, "host_not_found"),
        (MAC, Refusal::NoBmcMac, "no_bmc_mac"),
        ("db-1", Refusal::NoBmcMac, "no_bmc_mac"),
        ("web-1", Refusal::NoBmcMac, "no_bmc_mac"),
        ("db-2", Refusal::NoAddress, "no_address"),
        ("amb-1", Refusal::AmbiguousRecord, "ambiguous_record"),
    ];
    let mut expected = Vec::new();
    for (host, refusal, token) in cases {
        assert_eq!(
            coordinator.console_power(
                &caller("alice"),
                &power(host, proto::PowerAction::Status, "why")
            ),
            Err(refusal),
            "{host}"
        );
        expected.push(line(
            "alice",
            "console_denied",
            host,
            &format!("why; call=power; cause={token}"),
        ));
    }
    assert_eq!(fixture.tool.seen(), []);
    assert_eq!(fixture.lines(), expected);
}

/// Proves: each failure of the BMC becomes its refusal, after the `power_action` line, with one
/// `console_denied` line naming it: another MAC at the address, no answer, no free session, a
/// refused credential, an answer that cannot be read.
///
/// Catches: a BMC error told as success, the tool's text in an answer, and a failed command left
/// out of the audit.
#[test]
fn a_bmc_failure_is_its_refusal_and_is_audited() {
    let fixture = Fixture::new("console-power-bmc-failures");
    let no_session = "Error: Unable to establish IPMI v2 / RMCP+ session\n";
    fixture.tool.session(&[" 11 0c c4 7a 00 00 09\n"]);
    fixture.tool.end_at_start(no_session);
    fixture.tool.end_at_start(no_session);
    fixture
        .tool
        .end_at_start("insufficient resources for session\n");
    fixture.tool.end_at_start(no_session);
    fixture.tool.end_at_start("RAKP 2 HMAC is invalid\n");
    fixture.tool.session(&["garbage\n"]);
    fixture.tool.refuse(ShellError::Start);
    let cases = [
        (Refusal::AddressMismatch, "address_mismatch"),
        (Refusal::DeviceUnreachable, "device_unreachable"),
        (Refusal::BmcBusy, "bmc_busy"),
        (Refusal::BmcAuthFailed, "bmc_auth_failed"),
        (Refusal::BmcFailed, "bmc_failed"),
        (Refusal::BmcFailed, "bmc_failed"),
    ];
    let mut expected = Vec::new();
    for (refusal, token) in cases {
        assert_eq!(
            fixture.rig.coordinator.console_power(
                &caller("alice"),
                &power(NODE, proto::PowerAction::Reset, "hung")
            ),
            Err(refusal),
            "{token}"
        );
        expected.push(line("alice", "power_action", NODE, "hung; action=reset"));
        expected.push(line(
            "alice",
            "console_denied",
            NODE,
            &format!("hung; call=power; cause={token}"),
        ));
    }
    assert_eq!(fixture.tool.unused(), 0);
    assert_eq!(fixture.lines(), expected);
    fixture.tool.end_at_start(no_session);
    fixture.tool.end_at_start(no_session);
    assert_eq!(
        fixture.rig.coordinator.console_set_boot_device(
            &caller("alice"),
            &boot(proto::BootDevice::Disk, true, false)
        ),
        Err(Refusal::DeviceUnreachable)
    );
}

/// Proves: when the audit line cannot be written, `Power` and `SetBootDevice` are refused
/// `audit_unavailable` and the BMC is never reached.
///
/// Catches: a power command sent with no record of who sent it (decision D4: no line, no
/// action).
#[test]
fn no_audit_line_means_no_power_action() {
    let fixture = Fixture::new("console-power-no-audit");
    fixture.fake.refuse(|_| Some(audit_failure()));
    let coordinator = &fixture.rig.coordinator;
    assert_eq!(
        coordinator.console_power(&caller("alice"), &power(NODE, proto::PowerAction::Off, "x")),
        Err(Refusal::AuditUnavailable)
    );
    assert_eq!(
        coordinator
            .console_set_boot_device(&caller("alice"), &boot(proto::BootDevice::Pxe, true, true)),
        Err(Refusal::AuditUnavailable)
    );
    assert_eq!(fixture.tool.seen(), []);
}

/// Proves: a second call to one BMC waits for the first: it is told to wait while the first's
/// session is open, and its own session opens only after the first's has closed; both are
/// answered.
///
/// Catches: two commands to one BMC at once (a cycle racing a boot device set), which this
/// firmware handles one session at a time and the operator could not order.
#[test]
fn commands_to_one_bmc_are_serialized() {
    let fixture = Fixture::new("console-power-serialized");
    let (waits, waited) = std::sync::mpsc::channel();
    let coordinator = &fixture.rig.coordinator;
    coordinator.console().tell_waits(waits);
    let first = fixture
        .tool
        .held_session(&[NODE_BMC_ANSWER, "Chassis Power is on\n"]);
    fixture
        .tool
        .session(&[NODE_BMC_ANSWER, "Chassis Power is off\n"]);
    let call = || {
        coordinator
            .console_power(
                &caller("alice"),
                &power(NODE, proto::PowerAction::Status, "x"),
            )
            .map(|answer| answer.state())
    };
    let opens = |seen: &[Seen]| {
        seen.iter()
            .filter(|one| matches!(one, Seen::Open { .. }))
            .count()
    };
    std::thread::scope(|scope| {
        let one = scope.spawn(call);
        first.entered.recv().expect("the first session began");
        let two = scope.spawn(call);
        waited.recv().expect("the second call waits for the BMC");
        assert_eq!(opens(&fixture.tool.seen()), 1);
        first.release.send(()).expect("released");
        assert_eq!(one.join().expect("joined"), Ok(proto::PowerState::On));
        assert_eq!(two.join().expect("joined"), Ok(proto::PowerState::Off));
    });
    let seen = fixture.tool.seen();
    let closed = seen.iter().position(|one| *one == Seen::Closed);
    let second = seen
        .iter()
        .enumerate()
        .filter(|(_, one)| matches!(one, Seen::Open { .. }))
        .nth(1)
        .map(|(at, _)| at);
    assert!(closed < second, "{seen:?}");
}

/// Proves: once the operators file is changed to take power from an operator and read again on
/// the reload tick, that operator's power call is refused.
///
/// Catches: a grant read from the file as it was at start.
#[test]
fn power_follows_the_operators_file_in_use() {
    let fixture = Fixture::new("console-power-file");
    let files = write_files(
        &fixture.rig.dir,
        "[[operator]]\nname = \"alice\"\npower = false\n",
    );
    assert_eq!(files, fixture.files);
    fixture.rig.coordinator.console().files().reload();
    assert_eq!(
        fixture
            .rig
            .coordinator
            .console_power(&caller("alice"), &power(NODE, proto::PowerAction::On, "x")),
        Err(Refusal::PowerNotPermitted)
    );
}
