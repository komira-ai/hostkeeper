//! The answer set as each answer reads it, end to end: a read that fails gives the empty set for
//! that answer and the next answer reads again; and the records, hosts and pending requests the
//! coordinator holds, with `--reinstall`, decide which MACs are answered.

use std::net::{SocketAddr, UdpSocket};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_pxe_seed::inputs::LinuxPlatform;
use hostkeeper_pxe_seed::menu::LOCAL_DISK_MENU;
use tonic::Code;

use super::fake;
use super::kit::{
    ENROLLED, Kit, NOW, RECORDED, Running, client, dhcp, options, receive, tftp_read, waiting,
};

const PXE: Option<&str> = Some("PXEClient:Arch:00007:UNDI:003016");

/// A run of `kit` in `mode` with `extra`, and the firmware socket its offers go to.
fn start(kit: &Kit, mode: &str, extra: &[&str]) -> (Running, UdpSocket) {
    let firmware = client();
    let reply_to: SocketAddr = firmware.local_addr().expect("its address");
    (kit.start(kit.args(mode, reply_to, extra)), firmware)
}

/// Sends a PXE DISCOVER of `mac` stating `arch`, and returns the line it is logged with.
fn discover(run: &mut Running, firmware: &UdpSocket, mac: &str, arch: u16) -> String {
    firmware
        .send_to(&dhcp(1, mac, PXE, Some(arch), None), run.dhcp)
        .expect("send");
    run.wait(&format!("DHCPDISCOVER from {mac}"))
}

/// The line of a DISCOVER of `mac` at architecture 7 that got no answer, for `why`.
fn unanswered(mac: &str, why: &str) -> String {
    format!(
        "{NOW} dhcp: DHCPDISCOVER from {mac}, architecture 7, PXEClient, on the DHCP port: no answer: {why}"
    )
}

/// The line of a DISCOVER of `mac` at architecture 7 that got the x86-64 offer.
fn offered(mac: &str) -> String {
    format!(
        "{NOW} dhcp: DHCPDISCOVER from {mac}, architecture 7, PXEClient, on the DHCP port: \
         proxy offer sent, boot file hostkeeper/shimx64.efi (linux-x86_64)"
    )
}

/// While the coordinator refuses the token, a proxy DISCOVER of the recorded MAC gets no offer:
/// the failed read is logged, and the answer is made from the empty set; once the coordinator
/// answers again, the next DISCOVER reads again and gets the offer. An answer that does not
/// parse (a record holding a MAC that is not one) also gives the empty set, logged.
///
/// Catches an answer made from an earlier read's set (an enrolled host installed from a stale
/// list), and a server that stops reading after one failure.
#[test]
fn a_failed_read_answers_nothing_and_the_next_answer_reads_again() {
    let kit = Kit::new("reads-failed");
    let (mut run, firmware) = start(&kit, "proxy", &[]);
    kit.fake()
        .set(|s| s.refuse = Some((Code::Unauthenticated, "unauthorized".to_owned())));
    let line = discover(&mut run, &firmware, RECORDED, 7);
    assert_eq!(
        line,
        unanswered(RECORDED, "not in the answer set: no record holds it")
    );
    let failed = run
        .seen
        .iter()
        .rev()
        .find(|l| l.contains("could not be read"))
        .cloned();
    assert_eq!(
        failed,
        Some(format!(
            "{NOW} the coordinator could not be read (Enrollment.ListRecords: Unauthenticated: \
             unauthorized): this answer is made from the empty answer set"
        ))
    );

    kit.fake().set(|s| s.refuse = None);
    assert_eq!(
        discover(&mut run, &firmware, RECORDED, 7),
        offered(RECORDED)
    );
    let (offer, _) = receive(&firmware);
    assert_eq!(options(&offer)[&67], b"hostkeeper/shimx64.efi");

    kit.fake()
        .set(|s| s.records.push(fake::record("bad-1", &["zz"], false)));
    assert_eq!(
        discover(&mut run, &firmware, RECORDED, 7),
        unanswered(RECORDED, "not in the answer set: no record holds it")
    );
    assert!(
        run.seen.contains(&format!(
            "{NOW} the coordinator could not be read (Enrollment.ListRecords: the record bad-1 \
             holds \"zz\", which is not a MAC): this answer is made from the empty answer set"
        )),
        "{:#?}",
        run.seen
    );
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(!waiting(&firmware), "only one offer was sent");
}

/// In router mode a menu read while the coordinator refuses the token is the local-disk menu,
/// with the failed read logged before it.
///
/// Catches a router-mode menu that installs from an answer set it could not read.
#[test]
fn a_failed_read_in_router_mode_gives_the_local_disk_menu() {
    let kit = Kit::new("reads-router");
    let (mut run, _firmware) = start(&kit, "router", &[]);
    kit.fake()
        .set(|s| s.refuse = Some((Code::Unavailable, "down".to_owned())));
    let menu = tftp_read(
        run.tftp,
        &format!("/grub/hostkeeper/x86_64/{RECORDED}"),
        &[],
    )
    .expect("the menu");
    assert_eq!(menu, LOCAL_DISK_MENU.as_bytes());
    let line = run.wait("menu grub/");
    let at = run.seen.len() - 1;
    assert_eq!(
        run.seen[at - 1],
        format!(
            "{NOW} the coordinator could not be read (Enrollment.ListRecords: Unavailable: \
             down): this answer is made from the empty answer set"
        )
    );
    assert_eq!(
        line,
        format!(
            "{NOW} menu grub/hostkeeper/x86_64/{RECORDED}: local disk: {RECORDED}: not in the \
             answer set: no record holds it"
        )
    );
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
}

/// `--reinstall old-1` puts the enrolled host's record back in the answer set: the start says
/// so, its MAC gets the proxy offer and the install menu as host `old-1`, though a host is
/// enrolled against the record and its host id is held as approved.
///
/// Catches a reinstall that needs the record removed first, and one the opt-in does not reach.
#[test]
fn reinstall_answers_an_enrolled_host() {
    let kit = Kit::new("reads-reinstall");
    let (mut run, firmware) = start(&kit, "proxy", &["--reinstall", "old-1"]);
    assert_eq!(
        run.wait("--reinstall old-1"),
        format!(
            "{NOW} --reinstall old-1: its record is installed even while a host or a pending \
             request holds it"
        )
    );
    assert_eq!(
        discover(&mut run, &firmware, ENROLLED, 7),
        offered(ENROLLED)
    );
    let (offer, _) = receive(&firmware);
    assert_eq!(&offer[28..34], &super::kit::octets(ENROLLED));
    let menu = tftp_read(
        run.tftp,
        &format!("/grub/hostkeeper/x86_64/{ENROLLED}"),
        &[],
    )
    .expect("the menu");
    assert_ne!(menu, LOCAL_DISK_MENU.as_bytes());
    assert_eq!(
        run.wait("menu grub/"),
        format!("{NOW} menu grub/hostkeeper/x86_64/{ENROLLED}: install linux-x86_64 as host old-1")
    );
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
}

/// A pending request that matches the record of `new-1` leaves its MAC out, naming the request,
/// unless `--reinstall new-1` names it; so does a `host_held` of the record other than `NONE`
/// (retired, approved with no heartbeat since the coordinator started, denied, or not said),
/// though the record is not enrolled and no `Hosts.List` is read.
///
/// Catches a machine installed and waiting for approval that is wiped at its next reboot, and a
/// retired host's record installed after a coordinator restart forgot the host.
#[test]
fn a_pending_request_or_a_held_host_id_leaves_the_record_out() {
    let kit = Kit::new("reads-pending");
    kit.fake()
        .set(|s| s.pending = vec![fake::pending("req-1", "new-1")]);
    let (mut run, firmware) = start(&kit, "proxy", &[]);
    assert_eq!(
        discover(&mut run, &firmware, RECORDED, 7),
        unanswered(
            RECORDED,
            "not in the answer set: the record of host new-1: the pending request req-1 matches it"
        )
    );
    kit.fake().set(|s| s.pending.clear());
    for (held, why) in [
        (proto::HostHeld::Retired, "that id is retired"),
        (proto::HostHeld::Approved, "a host is approved as that id"),
        (
            proto::HostHeld::Denied,
            "that id is denied, its retirement not final yet",
        ),
        (
            proto::HostHeld::Unspecified,
            "the coordinator does not say whether a host holds that id",
        ),
    ] {
        kit.fake().set(|s| s.records[0].host_held = held.into());
        assert_eq!(
            discover(&mut run, &firmware, RECORDED, 7),
            unanswered(
                RECORDED,
                &format!("not in the answer set: the record of host new-1: {why}")
            )
        );
    }
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(!waiting(&firmware));

    kit.fake().set(|s| {
        s.records[0].host_held = proto::HostHeld::None.into();
        s.pending = vec![fake::pending("req-1", "new-1")];
    });
    let (mut run, firmware) = start(&kit, "proxy", &["--reinstall", "new-1"]);
    assert_eq!(
        discover(&mut run, &firmware, RECORDED, 7),
        offered(RECORDED)
    );
    let (offer, _) = receive(&firmware);
    assert_eq!(options(&offer)[&67], b"hostkeeper/shimx64.efi");
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
}

/// A MAC two records hold is left out, naming both; a firmware stating arm64 (11) when only
/// linux-x86_64 is sealed gets no offer, naming the platform with no boot set.
///
/// Catches a MAC given one of two records' installs at random, and an arm64 machine sent a
/// boot file this server does not have.
#[test]
fn a_shared_mac_or_a_platform_with_no_boot_set_is_not_answered() {
    let kit = Kit::with("reads-shared", &[LinuxPlatform::X86_64]);
    kit.fake()
        .set(|s| s.records.push(fake::record("twin-1", &[RECORDED], false)));
    let (mut run, firmware) = start(&kit, "proxy", &[]);
    assert_eq!(
        discover(&mut run, &firmware, RECORDED, 7),
        unanswered(
            RECORDED,
            "not in the answer set: the records of hosts new-1, twin-1 hold the same MAC"
        )
    );
    kit.fake()
        .set(|s| s.records.retain(|r| r.host_id != "twin-1"));
    assert_eq!(
        discover(&mut run, &firmware, RECORDED, 11),
        format!(
            "{NOW} dhcp: DHCPDISCOVER from {RECORDED}, architecture 11, PXEClient, on the DHCP \
             port: no answer: {RECORDED} is linux-arm64, which has no boot set"
        )
    );
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(!waiting(&firmware));
}

/// A record whose window is `CLOSED`, `USED` or `UNSPECIFIED` leaves its MAC out, naming the
/// state; `OPEN` is offered; `--reinstall new-1` serves a closed or a used record.
///
/// Catches a machine installed after its record's window ended or its approval was used, and
/// `--reinstall` held back by the window.
#[test]
fn a_record_whose_window_is_not_open_is_not_answered_unless_reinstalled() {
    let kit = Kit::new("reads-window");
    let (mut run, firmware) = start(&kit, "proxy", &[]);
    assert_eq!(
        discover(&mut run, &firmware, RECORDED, 7),
        offered(RECORDED)
    );
    let _ = receive(&firmware);
    for (state, why) in [
        (proto::RecordState::Closed, "its approval window is closed"),
        (proto::RecordState::Used, "an approval used it"),
        (
            proto::RecordState::Unspecified,
            "the coordinator does not say whether its approval window is open",
        ),
    ] {
        kit.fake().set(|s| s.records[0].state = state.into());
        assert_eq!(
            discover(&mut run, &firmware, RECORDED, 7),
            unanswered(
                RECORDED,
                &format!("not in the answer set: the record of host new-1: {why}")
            )
        );
    }
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(!waiting(&firmware));

    for state in [proto::RecordState::Closed, proto::RecordState::Used] {
        kit.fake().set(|s| s.records[0].state = state.into());
        let (mut run, firmware) = start(&kit, "proxy", &["--reinstall", "new-1"]);
        assert_eq!(
            discover(&mut run, &firmware, RECORDED, 7),
            offered(RECORDED)
        );
        let (offer, _) = receive(&firmware);
        assert_eq!(options(&offer)[&67], b"hostkeeper/shimx64.efi");
        let (result, _) = run.stop();
        assert_eq!(result, Ok(()));
    }
}
