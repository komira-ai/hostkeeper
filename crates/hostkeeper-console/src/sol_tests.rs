//! A serial console through the BMC, over the scripted double of the `ipmitool` port.

use std::io::{Read, Write};

use super::{
    BIT_RATE, PAYLOAD_STATUS, SOL_DEACTIVATE, SOL_INFO, bit_rate_repairs, ended, payload_active,
};
use crate::bmc::tests::{Fixture, MAC_ANSWER, NO_SESSION, OTHER_MAC_ANSWER, args, asked, opened};
use crate::bmc::{BmcError, Step};
use crate::ipmi::{SOL_ESCAPE, ShellError, ToolText};
use crate::scripted::Seen;

/// `sol info` with the two bit rates `volatile` and `stored`.
fn info(volatile: &str, stored: &str) -> String {
    format!(
        "Set in progress                 : set-complete\nEnabled                         : true\n\
         Volatile Bit Rate (kbps)        : {volatile}\nNon-Volatile Bit Rate (kbps)    : {stored}\n\
         Payload Port                    : 623\n"
    )
}

/// The open of the activating run: the default arguments with `sol activate` in place of
/// `shell`, and the escape byte.
fn activation() -> Seen {
    let mut args = args(false);
    args.pop();
    args.extend(["sol".to_owned(), "activate".to_owned()]);
    Seen::Open {
        args,
        escape: Some(SOL_ESCAPE),
        password: crate::bmc::tests::PASSWORD.to_owned(),
    }
}

/// Proves: the checks are one shell session that reads the MAC, then the payload's status, then
/// `sol info`, and ends; then the activation is a run of its own, `sol activate` and not `shell`,
/// with the escape byte 0xff, handed over as its streams: what the host says is read from it and
/// what is written reaches the host; ending it answers the tool's standard error; nothing is set
/// when both rates are 115.2.
///
/// Catches: a payload taken without its status read first, an activation inside a shell (whose
/// prompt would take a client's bytes as commands once the SOL ended), an activation without the
/// escape byte (the default `~.` would let any client end the session), and a rate set that was
/// not needed.
#[test]
fn sol_activates_in_the_checked_session_with_the_escape_byte() {
    let fixture = Fixture::new();
    let mut host =
        fixture
            .tool
            .sol_session(&[MAC_ANSWER, " 01 00 00\n", &info(BIT_RATE, BIT_RATE)]);
    let activated = fixture.bmc().activate_sol().expect("activated");
    assert_eq!(
        fixture.tool.seen(),
        [
            opened(false),
            asked(crate::bmc::MAC_READ),
            asked(PAYLOAD_STATUS),
            asked(SOL_INFO),
            Seen::Closed,
            activation(),
        ]
    );
    let crate::ipmi::Activated {
        mut input,
        mut output,
        end,
    } = activated;
    input.write_all(b"root\r").expect("written");
    input.flush().expect("flushed");
    assert_eq!(host.typed.blocking_recv(), Some(b"root\r".to_vec()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime.block_on(host.say(b"login: "));
    let mut read = [0u8; 4];
    assert_eq!(output.read(&mut read).expect("read"), 4);
    assert_eq!(&read, b"logi");
    assert_eq!(output.read(&mut read).expect("read"), 3);
    assert_eq!(&read[..3], b"n: ");
    host.exit("SOL session closed by BMC\n");
    runtime.block_on(host.say(b"after the end"));
    assert_eq!(output.read(&mut read).expect("the end"), 0);
    assert_eq!(end().as_str(), "SOL session closed by BMC\n");
    assert_eq!(fixture.tool.seen().last(), Some(&Seen::Closed));
    drop(host);
    assert_eq!(
        input.write_all(b"x").map_err(|why| why.kind()),
        Err(std::io::ErrorKind::BrokenPipe)
    );
}

/// Proves: a payload whose status shows an active instance is `bmc_sol_busy` and the session ends
/// right after the status: no `sol info`, no `sol activate`, no `sol deactivate` (a holder
/// outside the coordinator keeps its payload); a status that cannot be read fails at its step.
///
/// Catches: a lab user's SOL session taken over (on this firmware a second activate takes the
/// payload), or deactivated by the coordinator.
#[test]
fn a_foreign_sol_holder_is_bmc_sol_busy_and_is_not_deactivated() {
    for (answer, expected) in [
        (" 01 01 00\n", BmcError::SolBusy),
        (" 01 00 80\n", BmcError::SolBusy),
        (
            "Unable to send RAW command\n",
            BmcError::Failed(Step::PayloadStatus),
        ),
    ] {
        let fixture = Fixture::new();
        fixture.tool.session(&[MAC_ANSWER, answer]);
        assert_eq!(
            fixture.bmc().activate_sol().err(),
            Some(expected),
            "{answer}"
        );
        assert_eq!(
            fixture.tool.seen(),
            [
                opened(false),
                asked(crate::bmc::MAC_READ),
                asked(PAYLOAD_STATUS),
                Seen::Closed,
            ]
        );
    }
}

/// Proves: each bit rate that is not 115.2 is set, the volatile and the stored one each by its
/// own command, in the checks' session before the activation; a rate `sol info` does not give is
/// left alone.
///
/// Catches: a console at another bit rate than the host's port, which shows nothing readable.
#[test]
fn a_wrong_bit_rate_is_set_to_115_2_before_the_activation() {
    let fixture = Fixture::new();
    let _host =
        fixture
            .tool
            .sol_session(&[MAC_ANSWER, " 01 00 00\n", &info("9.6", "19.2"), "", ""]);
    fixture.bmc().activate_sol().expect("activated");
    let seen = fixture.tool.seen();
    assert_eq!(
        seen[3..],
        [
            asked(SOL_INFO),
            asked("sol set volatile-bit-rate 115.2"),
            asked("sol set non-volatile-bit-rate 115.2"),
            Seen::Closed,
            activation(),
        ]
    );
    assert_eq!(
        bit_rate_repairs(&info(BIT_RATE, "9.6")),
        ["sol set non-volatile-bit-rate 115.2"]
    );
    assert_eq!(bit_rate_repairs("Enabled : true\n"), Vec::<&str>::new());
}

/// Proves: the activation refuses as every BMC call does before its checks pass: another MAC
/// sends nothing after the MAC read, a full BMC is busy, a tool that cannot be started for the
/// activation fails at the start, and a session lost at its MAC read is unreachable.
///
/// Catches: a payload read or activated at a BMC whose MAC is not the host's.
#[test]
fn the_activation_refuses_before_its_checks_pass() {
    let fixture = Fixture::new();
    fixture.tool.session(&[OTHER_MAC_ANSWER]);
    assert_eq!(
        fixture.bmc().activate_sol().err(),
        Some(BmcError::AddressMismatch)
    );
    assert_eq!(fixture.tool.seen().len(), 3);
    let fixture = Fixture::new();
    fixture
        .tool
        .end_at_start("Error: insufficient resources for session\n");
    assert_eq!(fixture.bmc().activate_sol().err(), Some(BmcError::Busy));
    let fixture = Fixture::new();
    fixture
        .tool
        .session(&[MAC_ANSWER, " 01 00 00\n", &info(BIT_RATE, BIT_RATE)]);
    assert_eq!(
        fixture.bmc().activate_sol().err(),
        Some(BmcError::Failed(Step::Start))
    );
    let fixture = Fixture::new();
    fixture.tool.session_with(vec![
        Ok(MAC_ANSWER.to_owned()),
        Err(ShellError::Ended(ToolText::new(NO_SESSION.to_owned()))),
    ]);
    fixture.tool.end_at_start(NO_SESSION);
    assert_eq!(
        fixture.bmc().activate_sol().err(),
        Some(BmcError::Unreachable)
    );
    assert_eq!(args(false).last().map(String::as_str), Some("shell"));
}

/// Proves: the deactivation is a session of its own, without the escape byte, that reads the MAC
/// and then sends `sol deactivate`; at a BMC with another MAC it sends nothing.
///
/// Catches: a payload left active after the coordinator's stream ended, which holds the host
/// `bmc_sol_busy` until someone deactivates it by hand, and a deactivate sent to the wrong BMC.
#[test]
fn sol_deactivates_after_the_mac_read() {
    let fixture = Fixture::new();
    fixture.tool.session(&[MAC_ANSWER, ""]);
    assert_eq!(fixture.bmc().deactivate_sol(), Ok(()));
    assert_eq!(
        fixture.tool.seen(),
        [
            opened(false),
            asked(crate::bmc::MAC_READ),
            asked(SOL_DEACTIVATE),
            Seen::Closed,
        ]
    );
    let fixture = Fixture::new();
    fixture.tool.session(&[OTHER_MAC_ANSWER]);
    assert_eq!(
        fixture.bmc().deactivate_sol(),
        Err(BmcError::AddressMismatch)
    );
}

/// Proves: a stream that the BMC ended for another session is `bmc_sol_busy`, in any case of the
/// tool's text; an activation whose session could not be opened is `bmc_busy` or
/// `bmc_auth_failed` as its text says; any other end is `device_unreachable`. The payload status reads its three bytes
/// only.
///
/// Catches: a payload another session took reported as a dead BMC, and the coordinator then
/// deactivating that session's payload.
#[test]
fn an_ended_stream_names_its_cause() {
    let text = |t: &str| ToolText::new(t.to_owned());
    assert_eq!(
        ended(&text(
            "Error sending SOL data: FAIL\nSOL Session closed by BMC\n"
        )),
        BmcError::SolBusy
    );
    assert_eq!(ended(&text("")), BmcError::Unreachable);
    assert_eq!(
        ended(&text("insufficient resources for session\n")),
        BmcError::Busy
    );
    assert_eq!(
        ended(&text("RAKP 2 HMAC is invalid\n")),
        BmcError::AuthFailed
    );
    assert_eq!(ended(&text("Error: no response\n")), BmcError::Unreachable);
    assert_eq!(payload_active(" 01 00 00"), Some(false));
    assert_eq!(payload_active(" 01 00 00 00"), None);
    assert_eq!(payload_active(""), None);
}
