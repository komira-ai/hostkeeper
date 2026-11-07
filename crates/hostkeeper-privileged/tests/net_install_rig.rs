#![cfg(target_os = "linux")]
//! The parts of the network-install rig (`tests/netinstall/`) that need no root, in the
//! pull-request set: the router's DHCP the end-to-end tests fake ([`netinstall::router::Router`]),
//! a pure function of a packet and the leases given, and the reading of a serial console into
//! lines ([`netinstall::events::Lines`]). The end-to-end tests themselves run as root in
//! net-install.yml only. The rig drives Linux's network namespaces, so this file is Linux's only.

mod e2e;
mod netinstall;
mod support;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use netinstall::events::{Lines, clean};
use netinstall::router::{Boot, Router, Segment};

const MAC_A: [u8; 6] = [0x52, 0x54, 0x00, 0x4e, 0x10, 0x01];
const MAC_B: [u8; 6] = [0x52, 0x54, 0x00, 0x4e, 0x10, 0x02];
const ROUTER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
const PXE: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 3);

fn segment(size: u8) -> Segment {
    Segment {
        router: ROUTER,
        prefix: 24,
        first: Ipv4Addr::new(192, 0, 2, 100),
        size,
    }
}

fn router_mode() -> Option<Boot> {
    Some(Boot {
        server: PXE,
        file: "hostkeeper/boot.efi".to_owned(),
    })
}

/// A client's BOOTREQUEST of message type `kind` from `mac`, its `ciaddr` and its options
/// (code and value) after option 53, as a PXE firmware sends it.
fn request(kind: u8, mac: [u8; 6], ciaddr: Ipv4Addr, options: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = vec![0u8; 236];
    out[0] = 1;
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    out[10] = 0x80;
    out[12..16].copy_from_slice(&ciaddr.octets());
    out[28..34].copy_from_slice(&mac);
    out.extend_from_slice(&[99, 130, 83, 99, 53, 1, kind]);
    out.extend_from_slice(&[60, 32]);
    out.extend_from_slice(b"PXEClient:Arch:00007:UNDI:003016");
    out.extend_from_slice(&[93, 2, 0, 7]);
    for (code, value) in options {
        out.push(*code);
        out.push(u8::try_from(value.len()).expect("short"));
        out.extend_from_slice(value);
    }
    out.push(255);
    out
}

fn discover(mac: [u8; 6]) -> Vec<u8> {
    request(1, mac, Ipv4Addr::UNSPECIFIED, &[])
}

/// What a reply says: its fields and its options.
#[derive(Debug, PartialEq, Eq)]
struct Reply {
    op: u8,
    xid: [u8; 4],
    flags: [u8; 2],
    yiaddr: Ipv4Addr,
    siaddr: Ipv4Addr,
    chaddr: [u8; 6],
    file: String,
    options: BTreeMap<u8, Vec<u8>>,
}

fn read(reply: &[u8]) -> Reply {
    let addr = |at: usize| Ipv4Addr::new(reply[at], reply[at + 1], reply[at + 2], reply[at + 3]);
    assert_eq!(reply[236..240], [99, 130, 83, 99], "the magic cookie");
    let mut options = BTreeMap::new();
    let mut at = 240;
    while reply[at] != 255 {
        let len = usize::from(reply[at + 1]);
        options.insert(reply[at], reply[at + 2..at + 2 + len].to_vec());
        at += 2 + len;
    }
    assert_eq!(at, reply.len() - 1, "the end option is last");
    let file = &reply[108..236];
    let end = file.iter().position(|b| *b == 0).unwrap_or(file.len());
    Reply {
        op: reply[0],
        xid: reply[4..8].try_into().expect("four bytes"),
        flags: reply[10..12].try_into().expect("two bytes"),
        yiaddr: addr(16),
        siaddr: addr(20),
        chaddr: reply[28..34].try_into().expect("six bytes"),
        file: String::from_utf8(file[..end].to_vec()).expect("a name"),
        options,
    }
}

/// The options of an offer or an ack of the router: its type, its identifier, the lease, the
/// mask and the router, and in router mode the boot server and file.
fn given(kind: u8, boot: bool) -> BTreeMap<u8, Vec<u8>> {
    let mut options = BTreeMap::from([
        (53, vec![kind]),
        (54, ROUTER.octets().to_vec()),
        (51, 86_400u32.to_be_bytes().to_vec()),
        (1, vec![255, 255, 255, 0]),
        (3, ROUTER.octets().to_vec()),
    ]);
    if boot {
        options.insert(66, b"192.0.2.3".to_vec());
        options.insert(67, b"hostkeeper/boot.efi".to_vec());
    }
    options
}

/// Catches a fake router whose offer names a boot file in proxy mode (the firmware might prefer
/// it to the proxy offer), one that names none or another server in router mode, an offer that
/// does not echo the client's transaction, flags and hardware address, and a pool that gives a
/// client a new address at each DISCOVER.
#[test]
fn the_router_offers_the_pool_in_order_and_names_the_boot_file_only_in_router_mode() {
    let mut proxy = Router::new(segment(50), None);
    let answer = proxy.answer(&discover(MAC_A));
    assert_eq!(
        answer.line,
        "DHCPDISCOVER from 52:54:00:4e:10:01: offer 192.0.2.100, no boot file"
    );
    assert_eq!(
        read(&answer.reply.expect("an offer")),
        Reply {
            op: 2,
            xid: [0xde, 0xad, 0xbe, 0xef],
            flags: [0x80, 0],
            yiaddr: Ipv4Addr::new(192, 0, 2, 100),
            siaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: MAC_A,
            file: String::new(),
            options: given(2, false),
        }
    );
    let second = proxy.answer(&discover(MAC_B));
    assert_eq!(
        second.line,
        "DHCPDISCOVER from 52:54:00:4e:10:02: offer 192.0.2.101, no boot file"
    );
    let again = proxy.answer(&discover(MAC_A));
    assert_eq!(
        again.line, "DHCPDISCOVER from 52:54:00:4e:10:01: offer 192.0.2.100, no boot file",
        "the same MAC keeps its address"
    );

    let mut router = Router::new(segment(50), router_mode());
    let answer = router.answer(&discover(MAC_A));
    assert_eq!(
        answer.line,
        "DHCPDISCOVER from 52:54:00:4e:10:01: offer 192.0.2.100, boot file hostkeeper/boot.efi on \
         192.0.2.3"
    );
    let reply = read(&answer.reply.expect("an offer"));
    assert_eq!(
        (reply.siaddr, reply.file.as_str(), reply.options),
        (PXE, "hostkeeper/boot.efi", given(2, true))
    );
}

/// Catches a router that acks a REQUEST for another address or for another server, that cannot
/// ack the INIT-REBOOT or renewing REQUEST of a client it has not seen (a router started again
/// between two steps of a test), or whose NAK gives an address or a boot file.
#[test]
fn the_router_acks_its_own_address_and_naks_or_ignores_any_other() {
    let lease = Ipv4Addr::new(192, 0, 2, 100).octets();
    let other = Ipv4Addr::new(192, 0, 2, 7).octets();
    let ours = ROUTER.octets();
    let theirs = Ipv4Addr::new(192, 0, 2, 9).octets();
    let mut router = Router::new(segment(50), router_mode());

    // Selecting: the server identifier is the router's, the address the one offered.
    router.answer(&discover(MAC_A));
    let selecting = request(
        3,
        MAC_A,
        Ipv4Addr::UNSPECIFIED,
        &[(54, &ours), (50, &lease)],
    );
    let answer = router.answer(&selecting);
    assert_eq!(
        answer.line,
        "DHCPREQUEST from 52:54:00:4e:10:01: ack 192.0.2.100, boot file hostkeeper/boot.efi on \
         192.0.2.3"
    );
    let reply = read(&answer.reply.expect("an ack"));
    assert_eq!(
        (reply.yiaddr, reply.siaddr, reply.options),
        (Ipv4Addr::new(192, 0, 2, 100), PXE, given(5, true))
    );

    // For another server: no answer at all.
    let elsewhere = request(
        3,
        MAC_A,
        Ipv4Addr::UNSPECIFIED,
        &[(54, &theirs), (50, &lease)],
    );
    let answer = router.answer(&elsewhere);
    assert_eq!(
        answer.line,
        "DHCPREQUEST from 52:54:00:4e:10:01: not answered: it is for the server 192.0.2.9"
    );
    assert_eq!(answer.reply, None);

    // Another address: a NAK, which gives nothing.
    let wrong = request(3, MAC_A, Ipv4Addr::UNSPECIFIED, &[(50, &other)]);
    let answer = router.answer(&wrong);
    assert_eq!(
        answer.line,
        "DHCPREQUEST from 52:54:00:4e:10:01: nak: it asks for 192.0.2.7, and its address is \
         192.0.2.100"
    );
    let reply = read(&answer.reply.expect("a nak"));
    assert_eq!(
        (
            reply.yiaddr,
            reply.siaddr,
            reply.file.as_str(),
            reply.options
        ),
        (
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            "",
            BTreeMap::from([(53, vec![6]), (54, ours.to_vec())])
        )
    );
    // Neither an address nor a ciaddr: a NAK.
    let nothing = request(3, MAC_A, Ipv4Addr::UNSPECIFIED, &[]);
    assert_eq!(
        router.answer(&nothing).line,
        "DHCPREQUEST from 52:54:00:4e:10:01: nak: it names no address"
    );

    // A router that never saw the client: INIT-REBOOT (option 50, no server identifier) and
    // renewing (ciaddr, neither option) of the address it would give are acked.
    let mut fresh = Router::new(segment(50), None);
    let reboot = request(3, MAC_A, Ipv4Addr::UNSPECIFIED, &[(50, &lease)]);
    assert_eq!(
        fresh.answer(&reboot).line,
        "DHCPREQUEST from 52:54:00:4e:10:01: ack 192.0.2.100, no boot file"
    );
    let mut fresh = Router::new(segment(50), None);
    let renew = request(3, MAC_A, Ipv4Addr::new(192, 0, 2, 100), &[]);
    let answer = fresh.answer(&renew);
    assert_eq!(
        answer.line,
        "DHCPREQUEST from 52:54:00:4e:10:01: ack 192.0.2.100, no boot file"
    );
    assert_eq!(
        read(&answer.reply.expect("an ack")).options,
        given(5, false)
    );
}

/// Catches a router that answers what is not a client's request, or gives out more addresses
/// than its pool has.
#[test]
fn the_router_answers_nothing_but_a_clients_discover_or_request_within_its_pool() {
    let mut router = Router::new(segment(1), None);
    router.answer(&discover(MAC_A));
    let full = router.answer(&discover(MAC_B));
    assert_eq!(
        (full.line.as_str(), full.reply),
        (
            "DHCPDISCOVER from 52:54:00:4e:10:02: not answered: the pool of 1 addresses is used up",
            None
        )
    );
    let lease = Ipv4Addr::new(192, 0, 2, 101).octets();
    let full = router.answer(&request(3, MAC_B, Ipv4Addr::UNSPECIFIED, &[(50, &lease)]));
    assert_eq!(
        full.line,
        "DHCPREQUEST from 52:54:00:4e:10:02: nak: the pool of 1 addresses is used up"
    );
    let release = router.answer(&request(7, MAC_A, Ipv4Addr::UNSPECIFIED, &[]));
    assert_eq!(
        (release.line.as_str(), release.reply),
        (
            "DHCP message type 7 from 52:54:00:4e:10:01: not answered",
            None
        )
    );
    let mut reply = discover(MAC_A);
    reply[0] = 2;
    let ours = router.answer(&reply);
    assert_eq!(
        (ours.line.as_str(), ours.reply),
        ("not answered: a BOOTREPLY", None)
    );
    let short = router.answer(&[1, 1, 6]);
    assert_eq!(
        (short.line.as_str(), short.reply),
        (
            "not answered: the packet is truncated: 3 bytes, fewer than the 240 of the header \
             and the magic cookie",
            None
        )
    );
}

/// Catches a console reader that keeps a serial line's carriage return or the terminal
/// sequences a getty draws with (so the key hash line never equals its text), that loses a
/// line split across two reads, or that never yields the login prompt, which has no line end.
#[test]
fn a_serial_console_reads_as_clean_lines_and_its_login_prompt_as_one() {
    let mut lines = Lines::default();
    assert_eq!(
        lines.push(b"\x1b[2J\x1b[Hhostkeeper enrollment: pending"),
        Vec::<String>::new()
    );
    assert_eq!(
        lines.push(b" approval\r\n  key sha256:  ab\x1b[0m12\r\n\r\nnet-1 log"),
        vec![
            "hostkeeper enrollment: pending approval".to_owned(),
            "key sha256:  ab12".to_owned(),
            String::new(),
        ]
    );
    assert_eq!(lines.push(b"in: "), vec!["net-1 login:".to_owned()]);
    assert_eq!(
        clean(b"\x1b(B>>Start PXE over IPv4.\x07\r"),
        ">>Start PXE over IPv4."
    );
    assert_eq!(clean(b"\x1b"), "");
}
