//! Proxy mode: who gets an offer or an ACK, and what it says.

use std::net::Ipv4Addr;

use hostkeeper_pxe_packets::dhcp::{
    AnswerSet, BootClass, DhcpError, Ignored, MessageType, Mode, Outcome, Packet, Port, ReplyTo,
};
use hostkeeper_pxe_packets::{BootSets, ClientArch};
use hostkeeper_wire::Platform;

use crate::build::{
    ClientPacket, ENROLLED, HTTP_CLASS, HTTP_PORT, INFORM, PXE_CLASS, RECORDED, RELEASE, REQUEST,
    ROUTER, SERVER, answers, both_sets, proxy, reply, reply_options, service,
};

const X86_SHIM: &str = "hostkeeper/shimx64.efi";
const ARM_SHIM: &str = "hostkeeper/shimaa64.efi";

/// The offer to a PXE client of architecture 7, byte for byte: a BOOTREPLY with the client's
/// xid, flags and chaddr, no address, this server as `siaddr`, the shim in the `file` field,
/// then options 53 (offer), 54, 60 (`PXEClient`), 43 (discovery control 8) and 67, padded to
/// 300 bytes.
/// Catches: a proxy that hands out an address and fights the router, and one whose offer a
/// firmware would not take for a proxy offer.
#[test]
fn the_offer_to_a_pxe_client_is_exactly_the_proxy_offer() {
    let discover = ClientPacket::discover(RECORDED, PXE_CLASS, 7);
    let offer = reply(proxy(
        Port::Dhcp,
        &discover,
        &answers(Some(Platform::LinuxX86_64)),
    ));
    let mut want = vec![2, 1, 6, 0, 0xde, 0xad, 0xbe, 0xef, 0, 0, 0x80, 0];
    want.extend_from_slice(&[0; 4]); // ciaddr
    want.extend_from_slice(&[0; 4]); // yiaddr: no address
    want.extend_from_slice(&SERVER.octets()); // siaddr
    want.extend_from_slice(&[0; 4]); // giaddr
    want.extend_from_slice(&RECORDED.octets());
    want.extend_from_slice(&[0; 10]);
    want.extend_from_slice(&[0; 64]); // sname
    let mut file = X86_SHIM.as_bytes().to_vec();
    file.resize(128, 0);
    want.extend_from_slice(&file);
    want.extend_from_slice(&[99, 130, 83, 99]);
    want.extend_from_slice(&[53, 1, 2]);
    want.extend_from_slice(&[54, 4]);
    want.extend_from_slice(&SERVER.octets());
    want.extend_from_slice(&[60, 9]);
    want.extend_from_slice(b"PXEClient");
    want.extend_from_slice(&[43, 4, 6, 1, 8, 255]);
    want.extend_from_slice(&[67, 22]);
    want.extend_from_slice(X86_SHIM.as_bytes());
    want.push(255);
    want.resize(300, 0);
    assert_eq!(offer.bytes, want);
    assert_eq!(offer.to, ReplyTo::Broadcast);
    assert_eq!(offer.mac, RECORDED);
    assert_eq!(offer.platform, Platform::LinuxX86_64);
    assert_eq!(offer.boot_file, X86_SHIM);
}

/// Every architecture installed, in each class: 7, 9 and 16 get the x86-64 shim, 11 and 19 the
/// arm64 shim, as a TFTP name for `PXEClient` and an `http://` URL for `HTTPClient`; each offer
/// gives no address and echoes the class name.
/// Catches: an arm64 machine sent the x86-64 shim, and an HTTP boot client sent a TFTP name.
#[test]
fn each_architecture_gets_the_shim_of_its_platform_in_each_class() {
    let cases = [
        (7, Platform::LinuxX86_64, X86_SHIM),
        (9, Platform::LinuxX86_64, X86_SHIM),
        (16, Platform::LinuxX86_64, X86_SHIM),
        (11, Platform::LinuxArm64, ARM_SHIM),
        (19, Platform::LinuxArm64, ARM_SHIM),
    ];
    for (arch, platform, shim) in cases {
        for (class, name, boot_file) in [
            (PXE_CLASS, "PXEClient", shim.to_owned()),
            (
                HTTP_CLASS,
                "HTTPClient",
                format!("http://192.0.2.10:{HTTP_PORT}/{shim}"),
            ),
        ] {
            let discover = ClientPacket::discover(RECORDED, class, arch);
            let offer = reply(proxy(Port::Dhcp, &discover, &answers(None)));
            assert_eq!(offer.platform, platform, "arch {arch}");
            assert_eq!(offer.boot_file, boot_file, "arch {arch}");
            assert_eq!(offer.bytes[16..20], [0; 4], "yiaddr, arch {arch}");
            let read = Packet::parse(&offer.bytes).expect("the offer reads back");
            assert!(!read.is_request());
            assert_eq!(read.message_type(), MessageType::Offer);
            assert_eq!(read.server_id(), Some(SERVER));
            let options = reply_options(&offer.bytes);
            assert!(
                options.contains(&(60, name.as_bytes().to_vec())),
                "{options:?}"
            );
            assert!(options.contains(&(43, vec![6, 1, 8, 255])), "{options:?}");
            assert!(
                options.contains(&(67, boot_file.clone().into_bytes())),
                "{options:?}"
            );
        }
    }
}

/// A vendor class that is the bare class name counts, and so does one split over two option
/// 60 parts (RFC 3396).
#[test]
fn a_bare_or_split_vendor_class_is_a_network_boot_client() {
    let bare = ClientPacket::discover(RECORDED, b"HTTPClient", 19);
    let offer = reply(proxy(Port::Dhcp, &bare, &answers(None)));
    assert_eq!(
        offer.boot_file,
        format!("http://192.0.2.10:{HTTP_PORT}/{ARM_SHIM}")
    );
    let split = ClientPacket::new(1, RECORDED)
        .with(60, b"PXECli")
        .with(93, &[0, 9])
        .with(60, b"ent:Arch:00009");
    let offer = reply(proxy(Port::Dhcp, &split, &answers(None)));
    assert_eq!(offer.boot_file, X86_SHIM);
    let read = Packet::parse(&split.bytes()).expect("a DISCOVER");
    assert_eq!(read.boot_class(), Some(BootClass::Pxe));
}

/// Option 93 with several values: the first is the client's architecture.
#[test]
fn the_first_value_of_option_93_is_the_architecture() {
    let discover = ClientPacket::new(1, RECORDED)
        .with(60, PXE_CLASS)
        .with(93, &[0, 11, 0, 7]);
    let offer = reply(proxy(Port::Dhcp, &discover, &answers(None)));
    assert_eq!(offer.boot_file, ARM_SHIM);
    let read = Packet::parse(&discover.bytes()).expect("a DISCOVER");
    assert_eq!(read.arch(), Some(ClientArch::ARM64_UEFI));
}

/// The client's machine identifier (option 97) is echoed when it sent one, and absent when it
/// did not.
#[test]
fn option_97_is_echoed_only_when_sent() {
    let mut uuid = vec![0u8];
    uuid.extend(1..=16u8);
    let with = ClientPacket::discover(RECORDED, PXE_CLASS, 7).with(97, &uuid);
    let offer = reply(proxy(Port::Dhcp, &with, &answers(None)));
    assert!(reply_options(&offer.bytes).contains(&(97, uuid)));
    let without = ClientPacket::discover(RECORDED, PXE_CLASS, 7);
    let offer = reply(proxy(Port::Dhcp, &without, &answers(None)));
    assert!(
        reply_options(&offer.bytes)
            .iter()
            .all(|(code, _)| *code != 97)
    );
}

/// A REQUEST on UDP 4011 for a MAC in the set gets an ACK with the same boot file as the
/// offer, its `ciaddr` echoed, sent back to its sender; with option 54 naming this server, and
/// with no option 54.
#[test]
fn a_request_to_4011_in_the_set_gets_an_ack_with_the_offer_boot_file() {
    for class in [PXE_CLASS, HTTP_CLASS] {
        let set = answers(Some(Platform::LinuxArm64));
        let offer = reply(proxy(
            Port::Dhcp,
            &ClientPacket::discover(RECORDED, class, 11),
            &set,
        ));
        for request in [
            ClientPacket::proxy_request(RECORDED, class, 11),
            ClientPacket::proxy_request(RECORDED, class, 11).with(54, &SERVER.octets()),
        ] {
            let ack = reply(proxy(Port::ProxyDhcp, &request, &set));
            assert_eq!(ack.boot_file, offer.boot_file);
            assert_eq!(ack.to, ReplyTo::Sender);
            assert_eq!(ack.bytes[12..16], [192, 0, 2, 77], "ciaddr echoed");
            assert_eq!(ack.bytes[16..20], [0; 4], "yiaddr");
            assert_eq!(ack.bytes[10..12], [0, 0], "flags echoed");
            let read = Packet::parse(&ack.bytes).expect("the ACK reads back");
            assert_eq!(read.message_type(), MessageType::Ack);
            let options = reply_options(&ack.bytes);
            assert!(options.contains(&(67, offer.boot_file.clone().into_bytes())));
        }
    }
}

#[test]
fn a_request_to_4011_for_a_mac_not_in_the_set_gets_nothing() {
    let request = ClientPacket::proxy_request(ENROLLED, PXE_CLASS, 7);
    assert_eq!(
        proxy(Port::ProxyDhcp, &request, &answers(None)),
        Outcome::Ignored(Ignored::NotInAnswerSet { mac: ENROLLED })
    );
}

#[test]
fn a_request_to_4011_naming_another_server_gets_nothing() {
    let request = ClientPacket::proxy_request(RECORDED, PXE_CLASS, 7).with(54, &ROUTER.octets());
    assert_eq!(
        proxy(Port::ProxyDhcp, &request, &answers(None)),
        Outcome::Ignored(Ignored::OtherServer {
            mac: RECORDED,
            server: ROUTER
        })
    );
}

/// No answer, and the reason, for every packet the design gives no offer.
/// Catches: an install offered to an enrolled host, an ordinary client or the installer's own
/// DHCP, a BIOS or 32-bit ARM client, a platform with no boot set, a record typed with the
/// wrong platform, and a relayed packet answered on the wrong segment.
#[test]
fn nothing_is_sent_for_a_packet_the_design_does_not_answer() {
    let x86 = Some(Platform::LinuxX86_64);
    let arm_only = BootSets::new([Platform::LinuxArm64]).expect("a boot set");
    let mut relayed = ClientPacket::discover(RECORDED, PXE_CLASS, 7);
    relayed.giaddr = Ipv4Addr::new(198, 51, 100, 1);
    let mut reply_op = ClientPacket::discover(RECORDED, PXE_CLASS, 7);
    reply_op.op = 2;
    let disc = |arch| ClientPacket::discover(RECORDED, PXE_CLASS, arch);
    let cases: Vec<(&str, Port, ClientPacket, AnswerSet, BootSets, Ignored)> = vec![
        (
            "a MAC not in the set",
            Port::Dhcp,
            ClientPacket::discover(ENROLLED, PXE_CLASS, 7),
            answers(x86),
            both_sets(),
            Ignored::NotInAnswerSet { mac: ENROLLED },
        ),
        (
            "the empty answer set",
            Port::Dhcp,
            disc(7),
            AnswerSet::new(),
            both_sets(),
            Ignored::NotInAnswerSet { mac: RECORDED },
        ),
        (
            "no vendor class",
            Port::Dhcp,
            disc(7).without(60),
            answers(x86),
            both_sets(),
            Ignored::NotNetworkBoot { mac: RECORDED },
        ),
        (
            "an ordinary client's vendor class",
            Port::Dhcp,
            disc(7).without(60).with(60, b"MSFT 5.0"),
            answers(x86),
            both_sets(),
            Ignored::NotNetworkBoot { mac: RECORDED },
        ),
        (
            "a class name with more letters",
            Port::Dhcp,
            disc(7).without(60).with(60, b"PXEClientX"),
            answers(x86),
            both_sets(),
            Ignored::NotNetworkBoot { mac: RECORDED },
        ),
        (
            "a class name in another case",
            Port::Dhcp,
            disc(7).without(60).with(60, b"pxeclient"),
            answers(x86),
            both_sets(),
            Ignored::NotNetworkBoot { mac: RECORDED },
        ),
        (
            "architecture 0, legacy BIOS",
            Port::Dhcp,
            disc(0),
            answers(None),
            both_sets(),
            Ignored::UnsupportedArch {
                mac: RECORDED,
                arch: ClientArch::new(0),
            },
        ),
        (
            "architecture 10, 32-bit ARM UEFI",
            Port::Dhcp,
            disc(10),
            answers(None),
            both_sets(),
            Ignored::UnsupportedArch {
                mac: RECORDED,
                arch: ClientArch::new(10),
            },
        ),
        (
            "an unlisted architecture",
            Port::Dhcp,
            disc(0xffff),
            answers(x86),
            both_sets(),
            Ignored::UnsupportedArch {
                mac: RECORDED,
                arch: ClientArch::new(0xffff),
            },
        ),
        (
            "an architecture with no boot set",
            Port::Dhcp,
            disc(9),
            answers(None),
            arm_only.clone(),
            Ignored::NoBootSet {
                mac: RECORDED,
                platform: Platform::LinuxX86_64,
            },
        ),
        (
            "a record platform with no boot set, no option 93",
            Port::Dhcp,
            disc(9).without(93),
            answers(x86),
            arm_only,
            Ignored::NoBootSet {
                mac: RECORDED,
                platform: Platform::LinuxX86_64,
            },
        ),
        (
            "a record whose platform disagrees",
            Port::Dhcp,
            disc(7),
            answers(Some(Platform::LinuxArm64)),
            both_sets(),
            Ignored::PlatformDisagrees {
                mac: RECORDED,
                record: Platform::LinuxArm64,
                firmware: Platform::LinuxX86_64,
            },
        ),
        (
            "a Mac's record platform",
            Port::Dhcp,
            disc(19),
            answers(Some(Platform::DarwinArm64)),
            both_sets(),
            Ignored::PlatformDisagrees {
                mac: RECORDED,
                record: Platform::DarwinArm64,
                firmware: Platform::LinuxArm64,
            },
        ),
        (
            "no architecture and no record platform",
            Port::Dhcp,
            disc(7).without(93),
            answers(None),
            both_sets(),
            Ignored::NoPlatform { mac: RECORDED },
        ),
        (
            "a relayed DISCOVER",
            Port::Dhcp,
            relayed,
            answers(x86),
            both_sets(),
            Ignored::Relayed {
                mac: RECORDED,
                relay: Ipv4Addr::new(198, 51, 100, 1),
            },
        ),
        (
            "a BOOTREPLY",
            Port::Dhcp,
            reply_op,
            answers(x86),
            both_sets(),
            Ignored::NotRequest { mac: RECORDED },
        ),
        (
            "a REQUEST on the DHCP port",
            Port::Dhcp,
            ClientPacket::proxy_request(RECORDED, PXE_CLASS, 7),
            answers(x86),
            both_sets(),
            Ignored::NotAnswered {
                mac: RECORDED,
                kind: MessageType::Request,
                port: Port::Dhcp,
                mode: Mode::Proxy,
            },
        ),
        (
            "a DISCOVER on the proxyDHCP port",
            Port::ProxyDhcp,
            disc(7),
            answers(x86),
            both_sets(),
            Ignored::NotAnswered {
                mac: RECORDED,
                kind: MessageType::Discover,
                port: Port::ProxyDhcp,
                mode: Mode::Proxy,
            },
        ),
        (
            "an INFORM",
            Port::Dhcp,
            ClientPacket::new(INFORM, RECORDED).with(60, PXE_CLASS),
            answers(x86),
            both_sets(),
            Ignored::NotAnswered {
                mac: RECORDED,
                kind: MessageType::Other(INFORM),
                port: Port::Dhcp,
                mode: Mode::Proxy,
            },
        ),
        (
            "a RELEASE on the proxyDHCP port",
            Port::ProxyDhcp,
            ClientPacket::new(RELEASE, RECORDED).with(60, PXE_CLASS),
            answers(x86),
            both_sets(),
            Ignored::NotAnswered {
                mac: RECORDED,
                kind: MessageType::Other(RELEASE),
                port: Port::ProxyDhcp,
                mode: Mode::Proxy,
            },
        ),
        (
            "a REQUEST to 4011 whose platform disagrees",
            Port::ProxyDhcp,
            ClientPacket::proxy_request(RECORDED, PXE_CLASS, 11),
            answers(x86),
            both_sets(),
            Ignored::PlatformDisagrees {
                mac: RECORDED,
                record: Platform::LinuxX86_64,
                firmware: Platform::LinuxArm64,
            },
        ),
        (
            "a packet with no message type (BOOTP)",
            Port::Dhcp,
            ClientPacket::new(REQUEST, RECORDED).without(53),
            answers(x86),
            both_sets(),
            Ignored::Malformed(DhcpError::NoMessageType),
        ),
    ];
    for (what, port, packet, set, boot, want) in cases {
        let got = service(Mode::Proxy).on_packet(port, &packet.bytes(), &set, &boot);
        assert_eq!(got, Outcome::Ignored(want), "{what}");
    }
}

/// A client that states no architecture is placed by its record's platform.
#[test]
fn a_client_with_no_option_93_takes_its_record_platform() {
    let discover = ClientPacket::discover(RECORDED, PXE_CLASS, 7).without(93);
    let offer = reply(proxy(
        Port::Dhcp,
        &discover,
        &answers(Some(Platform::LinuxArm64)),
    ));
    assert_eq!(offer.platform, Platform::LinuxArm64);
    assert_eq!(offer.boot_file, ARM_SHIM);
}

/// The answer set is the one passed with each packet: a MAC answered once is not answered when
/// the next set leaves it out, and the run keeps no set of its own.
#[test]
fn each_packet_is_answered_from_the_set_passed_with_it() {
    let mut dhcp = service(Mode::Proxy);
    let discover = ClientPacket::discover(RECORDED, PXE_CLASS, 7).bytes();
    let first = dhcp.on_packet(Port::Dhcp, &discover, &answers(None), &both_sets());
    assert!(matches!(first, Outcome::Reply(_)), "{first:?}");
    let next = dhcp.on_packet(Port::Dhcp, &discover, &AnswerSet::new(), &both_sets());
    assert_eq!(
        next,
        Outcome::Ignored(Ignored::NotInAnswerSet { mac: RECORDED })
    );
    assert_eq!(
        dhcp.lease(crate::build::OFFERED),
        None,
        "proxy mode learns nothing"
    );
}
