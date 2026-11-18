//! Power and boot device through the BMC, over the scripted double of the `ipmitool` port.

use hostkeeper_enrollment::names::MacAddress;

use super::{
    BOOT_FLAGS_READ, Bmc, BmcError, BootDevice, BootFlags, BootRequest, MAC_READ, POWER_STATUS,
    PowerAction, PowerAnswer, PowerState, Step, parse_boot_flags, parse_mac,
};
use crate::credential::BmcCredential;
use crate::ipmi::{PROMPT, Settings, ShellError, ToolText};
use crate::scripted::{Scripted, Seen};
use crate::targets::Address;

/// The MAC of the host's inventory record, and the answer of a BMC that has it.
const MAC: &str = "0c:c4:7a:00:00:02";
pub(crate) const MAC_ANSWER: &str = " 11 0c c4 7a 00 00 02\n";
/// The answer of a BMC that has another MAC.
pub(crate) const OTHER_MAC_ANSWER: &str = " 11 0c c4 7a 00 00 09\n";
/// What the tool prints when it cannot open a session, whatever the cause, at its default
/// verbosity.
pub(crate) const NO_SESSION: &str = "Error: Unable to establish IPMI v2 / RMCP+ session\n";
pub(crate) const PASSWORD: &str = "a long random pass";

pub(crate) struct Fixture {
    pub(crate) tool: Scripted,
    address: Address,
    credential: BmcCredential,
    settings: Settings,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self {
            tool: Scripted::new(),
            address: Address::Ip("192.0.2.12".parse().expect("an address")),
            credential: BmcCredential::parse(&format!(
                "user = \"ADMIN\"\npassword = \"{PASSWORD}\"\n"
            ))
            .expect("a credential"),
            settings: Settings::default(),
        }
    }

    pub(crate) fn bmc(&self) -> Bmc<'_> {
        Bmc {
            tool: &self.tool,
            address: &self.address,
            credential: &self.credential,
            settings: &self.settings,
            mac: MacAddress::parse(MAC).expect("a MAC"),
        }
    }
}

/// The arguments of a session at the fixture's BMC with the default settings, `-v` among them
/// when `verbose`.
pub(crate) fn args(verbose: bool) -> Vec<String> {
    let mut args: Vec<String> = [
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
    ]
    .map(str::to_owned)
    .to_vec();
    if verbose {
        args.push("-v".to_owned());
    }
    args.push("shell".to_owned());
    args
}

pub(crate) fn opened(verbose: bool) -> Seen {
    Seen::Open {
        args: args(verbose),
        escape: None,
        password: PASSWORD.to_owned(),
    }
}

pub(crate) fn asked(command: &str) -> Seen {
    Seen::Ask(command.to_owned())
}

/// Proves: a cycle of a host that is on is one session that reads the BMC's MAC, then the power
/// state, then sends exactly `chassis power cycle`, and answers the state before and the command
/// sent; the session's arguments are exactly the design's, the password not among them.
///
/// Catches: a command sent before the MAC check or in a second session, a command other than
/// the one asked, a state read after the command, and a changed argument list.
#[test]
fn power_cycle_builds_exactly_the_typed_command() {
    let fixture = Fixture::new();
    fixture.tool.session(&[
        MAC_ANSWER,
        "Chassis Power is on\n",
        "Chassis Power Control: Cycle\n",
    ]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Cycle),
        Ok(PowerAnswer {
            state: PowerState::On,
            sent: PowerAction::Cycle,
        })
    );
    assert_eq!(
        fixture.tool.seen(),
        [
            opened(false),
            asked(MAC_READ),
            asked(POWER_STATUS),
            asked("chassis power cycle"),
            Seen::Closed,
        ]
    );
}

/// Proves: each action is sent as its own `chassis power` command and is taken only with its
/// own acknowledgement, to a host that is on or off alike (but `cycle` to one that is off), and
/// `status` sends nothing after the state.
///
/// Catches: two actions swapped (off sent for soft, or reset for cycle), an acknowledgement of
/// another action taken as this one's, and a status that sends a command.
#[test]
fn every_power_action_sends_its_command_and_needs_its_acknowledgement() {
    let cases = [
        (PowerAction::On, "off", "Up/On"),
        (PowerAction::Off, "on", "Down/Off"),
        (PowerAction::Off, "off", "Down/Off"),
        (PowerAction::Soft, "on", "Soft"),
        (PowerAction::Reset, "on", "Reset"),
    ];
    for (action, state, acknowledgement) in cases {
        let fixture = Fixture::new();
        let status = format!("Chassis Power is {state}\n");
        let taken = format!("Chassis Power Control: {acknowledgement}\n");
        fixture.tool.session(&[MAC_ANSWER, &status, &taken]);
        let before = if state == "on" {
            PowerState::On
        } else {
            PowerState::Off
        };
        assert_eq!(
            fixture.bmc().power(action),
            Ok(PowerAnswer {
                state: before,
                sent: action,
            }),
            "{action:?}"
        );
        let command = format!("chassis power {}", action.as_str());
        assert_eq!(
            fixture.tool.seen(),
            [
                opened(false),
                asked(MAC_READ),
                asked(POWER_STATUS),
                asked(&command),
                Seen::Closed,
            ],
            "{action:?}"
        );

        let fixture = Fixture::new();
        fixture
            .tool
            .session(&[MAC_ANSWER, &status, "Chassis Power Control: Diag\n"]);
        assert_eq!(
            fixture.bmc().power(action),
            Err(BmcError::Failed(Step::PowerCommand)),
            "{action:?}"
        );
    }

    let fixture = Fixture::new();
    fixture
        .tool
        .session(&[MAC_ANSWER, "Chassis Power is off\n"]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Ok(PowerAnswer {
            state: PowerState::Off,
            sent: PowerAction::Status,
        })
    );
    assert_eq!(
        fixture.tool.seen(),
        [
            opened(false),
            asked(MAC_READ),
            asked(POWER_STATUS),
            Seen::Closed
        ]
    );
}

/// Proves: a cycle of a host that is off sends `chassis power on`, and the answer says so.
///
/// Catches: a cycle sent to an off host, which some BMCs ignore, leaving the host off while the
/// operator is told it cycled.
#[test]
fn cycle_on_an_off_host_sends_on() {
    let fixture = Fixture::new();
    fixture.tool.session(&[
        MAC_ANSWER,
        "Chassis Power is off\n",
        "Chassis Power Control: Up/On\n",
    ]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Cycle),
        Ok(PowerAnswer {
            state: PowerState::Off,
            sent: PowerAction::On,
        })
    );
    assert_eq!(
        fixture.tool.seen()[3],
        asked("chassis power on"),
        "{:?}",
        fixture.tool.seen()
    );
}

/// Proves: the boot device command names the device and, as asked, `options=persistent`,
/// `options=efiboot` or both joined by a comma, then reads the flags back in the same session.
///
/// Catches: a persistent request sent as a one-time override (the node-02 incident's boot order
/// would not stay), the EFI option dropped on a UEFI board, and options written as two words.
#[test]
fn bootdev_persistent_and_efi_build_their_options() {
    let cases = [
        (BootDevice::Disk, false, false, "chassis bootdev disk"),
        (
            BootDevice::Disk,
            true,
            false,
            "chassis bootdev disk options=persistent",
        ),
        (
            BootDevice::Pxe,
            false,
            true,
            "chassis bootdev pxe options=efiboot",
        ),
        (
            BootDevice::Bios,
            true,
            true,
            "chassis bootdev bios options=persistent,efiboot",
        ),
    ];
    for (device, persistent, efi, command) in cases {
        let fixture = Fixture::new();
        let taken = format!("Set Boot Device to {}\n", device.as_str());
        fixture.tool.session(&[
            MAC_ANSWER,
            &taken,
            "Boot parameter version: 1\nBoot parameter data: 0000000000\n",
        ]);
        let request = BootRequest {
            device,
            persistent,
            efi,
        };
        assert_eq!(
            fixture.bmc().set_boot_device(request),
            Ok(BootFlags::default())
        );
        assert_eq!(
            fixture.tool.seen(),
            [
                opened(false),
                asked(MAC_READ),
                asked(command),
                asked(BOOT_FLAGS_READ),
                Seen::Closed,
            ]
        );
    }
}

/// Proves: the answer is the flags the BMC holds, read from `Boot parameter data:`: valid, for
/// every boot, UEFI and the device; a device the API does not name is none, and flags that are
/// not valid are all empty, whatever their other bits.
///
/// Catches: the request echoed as the answer (a persistent set taken on faith), the persistent
/// and EFI bits swapped, and a device read from the wrong bits.
#[test]
fn the_boot_flags_are_read_back_into_the_answer() {
    let read = |data: &str| {
        let fixture = Fixture::new();
        let flags = format!(
            "Boot parameter version: 1\nBoot parameter 5 is valid/unlocked\nBoot parameter data: {data}\n Boot Flags :\n   - Boot Flag Valid\n"
        );
        fixture
            .tool
            .session(&[MAC_ANSWER, "Set Boot Device to disk\n", &flags]);
        fixture.bmc().set_boot_device(BootRequest {
            device: BootDevice::Disk,
            persistent: true,
            efi: true,
        })
    };
    let flags = |device, persistent, efi| {
        Ok(BootFlags {
            device,
            persistent,
            efi,
        })
    };
    assert_eq!(
        read("e008000000"),
        flags(Some(BootDevice::Disk), true, true)
    );
    assert_eq!(
        read("C004000000"),
        flags(Some(BootDevice::Pxe), true, false)
    );
    assert_eq!(
        read("a018000000"),
        flags(Some(BootDevice::Bios), false, true)
    );
    assert_eq!(read("8014000000"), flags(None, false, false));
    assert_eq!(read("6008000000"), flags(None, false, false));
}

/// Proves: a boot device answer that does not acknowledge the device, and flags that cannot be
/// read, are each a failure at their step.
///
/// Catches: an unacknowledged set answered as done, and unreadable flags answered as "no
/// override".
#[test]
fn an_unread_boot_answer_fails_at_its_step() {
    let request = BootRequest {
        device: BootDevice::Disk,
        persistent: true,
        efi: false,
    };
    let fixture = Fixture::new();
    fixture
        .tool
        .session(&[MAC_ANSWER, "Set Boot Device to pxe\n"]);
    assert_eq!(
        fixture.bmc().set_boot_device(request),
        Err(BmcError::Failed(Step::BootDevice))
    );
    let fixture = Fixture::new();
    fixture.tool.session(&[
        MAC_ANSWER,
        "Set Boot Device to disk\n",
        "Boot parameter 5\n",
    ]);
    assert_eq!(
        fixture.bmc().set_boot_device(request),
        Err(BmcError::Failed(Step::BootFlags))
    );
}

/// Proves: when the BMC at the address reports another MAC, the call answers `address_mismatch`
/// and the session sends nothing after the MAC read, for power and for the boot device.
///
/// Catches: a power command sent to the wrong server's BMC because a lease moved.
#[test]
fn a_mac_that_differs_from_the_records_sends_nothing() {
    let fixture = Fixture::new();
    fixture.tool.session(&[OTHER_MAC_ANSWER]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Off),
        Err(BmcError::AddressMismatch)
    );
    fixture.tool.session(&[OTHER_MAC_ANSWER]);
    assert_eq!(
        fixture.bmc().set_boot_device(BootRequest {
            device: BootDevice::Pxe,
            persistent: false,
            efi: false,
        }),
        Err(BmcError::AddressMismatch)
    );
    let once = [opened(false), asked(MAC_READ), Seen::Closed];
    assert_eq!(fixture.tool.seen(), [once.clone(), once].concat());
}

/// Proves: the MAC read and the command are in one session: one open, then every command, then
/// the session's end, with no second open.
///
/// Catches: the check made in a session of its own, which leaves a window in which the address
/// can change between the check and the command (decision D8).
#[test]
fn the_mac_read_runs_in_the_commands_session() {
    let fixture = Fixture::new();
    fixture.tool.session(&[
        MAC_ANSWER,
        "Chassis Power is on\n",
        "Chassis Power Control: Reset\n",
    ]);
    fixture.bmc().power(PowerAction::Reset).expect("reset");
    let seen = fixture.tool.seen();
    let opens = seen
        .iter()
        .filter(|one| matches!(one, Seen::Open { .. }))
        .count();
    assert_eq!(opens, 1, "{seen:?}");
    assert_eq!(seen.first(), Some(&opened(false)));
    assert_eq!(seen.last(), Some(&Seen::Closed));
}

/// Proves: a session the BMC refuses for the credential is `bmc_auth_failed`, told by the one
/// run with `-v` that sends no command, and the error's text is exactly its token, with nothing
/// of the tool's text (which holds the user name).
///
/// Catches: a wrong password told as an unreachable BMC, the tool's text in an answer or a log
/// line, and a command sent in the diagnosing run.
#[test]
fn a_wrong_password_maps_to_bmc_auth_failed_without_the_tools_text() {
    let fixture = Fixture::new();
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.end_at_start(
        "RAKP 2 HMAC is invalid\nError: Unable to establish IPMI v2 / RMCP+ session\n",
    );
    let refused = fixture.bmc().power(PowerAction::Status);
    assert_eq!(refused, Err(BmcError::AuthFailed));
    assert_eq!(
        refused.map_err(|why| why.to_string()),
        Err("bmc_auth_failed".to_owned())
    );
    assert_eq!(fixture.tool.seen(), [opened(false), opened(true)]);

    for refusal in [
        "RAKP 2 message indicates an error : unauthorized name\n",
        "RAKP 2 message indicates an error : invalid integrity check value\n",
    ] {
        let fixture = Fixture::new();
        fixture.tool.end_at_start(NO_SESSION);
        fixture.tool.end_at_start(refusal);
        assert_eq!(
            fixture.bmc().power(PowerAction::On),
            Err(BmcError::AuthFailed),
            "{refusal}"
        );
    }
}

/// Proves: a session that no run can open, for no reason the tool names, is
/// `device_unreachable`; so is a diagnosing run that opens one after all (it is dropped with no
/// command) and a tool that gave no prompt in time.
///
/// Catches: an unreachable BMC told as a refused credential, a command sent when the second run
/// happens to open a session, and a tool hung forever.
#[test]
fn a_bmc_that_does_not_answer_is_unreachable() {
    let fixture = Fixture::new();
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.end_at_start(NO_SESSION);
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Unreachable)
    );
    assert_eq!(fixture.tool.seen(), [opened(false), opened(true)]);

    let fixture = Fixture::new();
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.session(&[]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Off),
        Err(BmcError::Unreachable)
    );
    assert_eq!(
        fixture.tool.seen(),
        [opened(false), opened(true), Seen::Closed]
    );

    let fixture = Fixture::new();
    fixture.tool.refuse(ShellError::Deadline);
    assert_eq!(
        fixture.bmc().power(PowerAction::Off),
        Err(BmcError::Unreachable)
    );
    assert_eq!(fixture.tool.seen(), [opened(false)]);

    let fixture = Fixture::new();
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.refuse(ShellError::Deadline);
    assert_eq!(
        fixture.bmc().power(PowerAction::Off),
        Err(BmcError::Unreachable)
    );
}

/// Proves: a BMC with no free session is `bmc_busy` at once, with no diagnosing run; and a tool
/// that cannot be started is a failure at the start, by its first run or its second.
///
/// Catches: a busy BMC asked again at once, and a missing tool told as an unreachable BMC.
#[test]
fn a_full_bmc_is_busy_and_a_missing_tool_fails_at_the_start() {
    let fixture = Fixture::new();
    fixture.tool.end_at_start(
        "Error in open session response message : insufficient resources for session\n",
    );
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Busy)
    );
    assert_eq!(fixture.tool.seen(), [opened(false)]);

    let fixture = Fixture::new();
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Failed(Step::Start))
    );
    assert_eq!(
        BmcError::Failed(Step::Start).to_string(),
        "bmc_failed at Start"
    );

    let fixture = Fixture::new();
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.refuse(ShellError::Start);
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Failed(Step::Start))
    );
}

/// Proves: a tool that ends or stalls in the middle of a session is named as at its start (no
/// free session is busy, anything else unreachable), and an answer that cannot be read is a
/// failure at its step, with nothing sent after it.
///
/// Catches: a lost session answered as a power state, and a command sent after a MAC that could
/// not be read.
#[test]
fn a_session_lost_or_unread_midway_sends_nothing_more() {
    let ended = |text: &str| Err(ShellError::Ended(ToolText::new(text.to_owned())));
    let fixture = Fixture::new();
    fixture
        .tool
        .session_with(vec![ended("insufficient resources for session")]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Busy)
    );
    let fixture = Fixture::new();
    fixture
        .tool
        .session_with(vec![Ok(MAC_ANSWER.to_owned()), ended("")]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Unreachable)
    );
    let fixture = Fixture::new();
    fixture.tool.session_with(vec![Err(ShellError::Deadline)]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Unreachable)
    );
    // Past its script the double's program ends with nothing on its standard error.
    let fixture = Fixture::new();
    fixture.tool.session(&[MAC_ANSWER]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Status),
        Err(BmcError::Unreachable)
    );

    let fixture = Fixture::new();
    fixture.tool.session(&["Unable to send RAW command\n"]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Off),
        Err(BmcError::Failed(Step::MacRead))
    );
    assert_eq!(
        fixture.tool.seen(),
        [opened(false), asked(MAC_READ), Seen::Closed]
    );
    let fixture = Fixture::new();
    fixture
        .tool
        .session(&[MAC_ANSWER, "Chassis Power is unknown\n"]);
    assert_eq!(
        fixture.bmc().power(PowerAction::Off),
        Err(BmcError::Failed(Step::PowerStatus))
    );
    assert_eq!(fixture.tool.unused(), 0);
}

/// Proves: the MAC is read from the lines of two-digit hex bytes only, across lines, and is
/// exactly the revision and six octets: an echoed command, a prompt and other lines are passed
/// over, and a count other than seven bytes is no MAC.
///
/// Catches: the echoed command's bytes read as the MAC, and a short or long answer compared as a
/// MAC.
#[test]
fn the_mac_is_the_six_octets_after_the_revision() {
    let echoed = format!("{PROMPT}{MAC_READ}\n 11 0c c4 7a\n 00 00 02\n");
    assert_eq!(parse_mac(&echoed), Some([0x0c, 0xc4, 0x7a, 0, 0, 2]));
    assert_eq!(
        parse_mac(" 11 0C C4 7A 00 00 02"),
        Some([0x0c, 0xc4, 0x7a, 0, 0, 2])
    );
    assert_eq!(parse_mac(" 11 0c c4 7a 00 00"), None);
    assert_eq!(parse_mac(" 11 0c c4 7a 00 00 02 03"), None);
    assert_eq!(parse_mac(" 11 0c c4 7a 00 00 0g"), None);
    assert_eq!(parse_mac("\n\n"), None);
    assert_eq!(parse_mac(""), None);
}

/// Proves: boot flags that are missing, short, of an odd length, or not hex are unread, and a
/// valid line is read wherever it is among the others.
///
/// Catches: a short answer read past its end, and a stray line read as the flags.
#[test]
fn boot_flags_are_read_only_from_a_whole_data_line() {
    for unread in [
        "",
        "Boot parameter version: 1\n",
        "Boot parameter data: 80\n",
        "Boot parameter data: 80080\n",
        "Boot parameter data: zz08000000\n",
        "Boot parameter data: 80zz000000\n",
        "Boot parameter data: 8\u{e9}0800000\n",
    ] {
        assert_eq!(parse_boot_flags(unread), None, "{unread:?}");
    }
    assert_eq!(
        parse_boot_flags("x\n  Boot parameter data: c008  \ny\n"),
        Some(BootFlags {
            device: Some(BootDevice::Disk),
            persistent: true,
            efi: false,
        })
    );
}

/// Proves: the names an audit line and a command use for each action and device.
///
/// Catches: an action audited under another's name.
#[test]
fn actions_and_devices_have_their_names() {
    let actions = [
        PowerAction::Status,
        PowerAction::On,
        PowerAction::Off,
        PowerAction::Soft,
        PowerAction::Cycle,
        PowerAction::Reset,
    ]
    .map(PowerAction::as_str);
    assert_eq!(actions, ["status", "on", "off", "soft", "cycle", "reset"]);
    let devices = [BootDevice::Pxe, BootDevice::Disk, BootDevice::Bios].map(BootDevice::as_str);
    assert_eq!(devices, ["pxe", "disk", "bios"]);
    assert_eq!(PowerAction::Status.acknowledgement(), "");
}
