//! The reasons `serve` logs, each by exact equality: what a person reads when a machine got no
//! answer.

use std::net::Ipv4Addr;

use hostkeeper_pxe_packets::ClientArch;
use hostkeeper_pxe_packets::dhcp::{DhcpError, Ignored, MAX_PACKET, MessageType, Mode, Port};
use hostkeeper_pxe_packets::tftp::{Refused, TftpError};
use hostkeeper_wire::Platform;

use crate::build::{RECORDED, ROUTER};

#[test]
fn each_dhcp_reason_reads_as_logged() {
    let mac = RECORDED;
    let cases: Vec<(Ignored, &str)> = vec![
        (
            DhcpError::Truncated(3).into(),
            "the packet is truncated: 3 bytes, fewer than the 240 of the header and the magic cookie",
        ),
        (
            DhcpError::Oversized(MAX_PACKET + 1).into(),
            "the packet is oversized: 1473 bytes, more than 1472",
        ),
        (
            DhcpError::NoCookie.into(),
            "the packet has no DHCP magic cookie",
        ),
        (
            DhcpError::NotEthernet { htype: 6, hlen: 6 }.into(),
            "the hardware address is not Ethernet: htype 6, hlen 6",
        ),
        (
            DhcpError::OptionTruncated(60).into(),
            "option 60 runs past the end of the packet",
        ),
        (
            DhcpError::BadOption { code: 93, len: 3 }.into(),
            "option 93 has 3 bytes, which its definition does not allow",
        ),
        (
            DhcpError::NoMessageType.into(),
            "the packet has no DHCP message type (option 53)",
        ),
        (
            Ignored::NotRequest { mac },
            "a BOOTREPLY from 0c:c4:7a:1f:ab:02, not a client's request",
        ),
        (
            Ignored::NotAnswered {
                mac,
                kind: MessageType::Request,
                port: Port::Dhcp,
                mode: Mode::Proxy,
            },
            "a DHCPREQUEST from 0c:c4:7a:1f:ab:02 on the DHCP port is not answered in proxy mode",
        ),
        (
            Ignored::NotAnswered {
                mac,
                kind: MessageType::Discover,
                port: Port::ProxyDhcp,
                mode: Mode::Router,
            },
            "a DHCPDISCOVER from 0c:c4:7a:1f:ab:02 on the proxyDHCP port is not answered in \
             router mode",
        ),
        (
            Ignored::NotAnswered {
                mac,
                kind: MessageType::Other(8),
                port: Port::Dhcp,
                mode: Mode::Proxy,
            },
            "a DHCP message type 8 from 0c:c4:7a:1f:ab:02 on the DHCP port is not answered in \
             proxy mode",
        ),
        (
            Ignored::Relayed {
                mac,
                relay: Ipv4Addr::new(198, 51, 100, 1),
            },
            "a packet from 0c:c4:7a:1f:ab:02 forwarded by the relay 198.51.100.1",
        ),
        (
            Ignored::NotNetworkBoot { mac },
            "0c:c4:7a:1f:ab:02 is not a network-boot client",
        ),
        (
            Ignored::NotInAnswerSet { mac },
            "0c:c4:7a:1f:ab:02 is not in the answer set",
        ),
        (
            Ignored::NoPlatform { mac },
            "0c:c4:7a:1f:ab:02 states no architecture and its record no platform",
        ),
        (
            Ignored::UnsupportedArch {
                mac,
                arch: ClientArch::new(0),
            },
            "0c:c4:7a:1f:ab:02 states the architecture 0, which is not installed",
        ),
        (
            Ignored::PlatformDisagrees {
                mac,
                record: Platform::LinuxArm64,
                firmware: Platform::LinuxX86_64,
            },
            "the record of 0c:c4:7a:1f:ab:02 is linux-arm64, and its firmware states linux-x86_64",
        ),
        (
            Ignored::NoBootSet {
                mac,
                platform: Platform::LinuxArm64,
            },
            "0c:c4:7a:1f:ab:02 is linux-arm64, which has no boot set",
        ),
        (
            Ignored::OtherServer {
                mac,
                server: ROUTER,
            },
            "the request of 0c:c4:7a:1f:ab:02 names the server 192.0.2.1",
        ),
        (
            Ignored::NoAddress { mac },
            "the request of 0c:c4:7a:1f:ab:02 names no address",
        ),
    ];
    for (ignored, want) in cases {
        assert_eq!(ignored.to_string(), want);
    }
    for (kind, want) in [
        (MessageType::Offer, "DHCPOFFER"),
        (MessageType::Ack, "DHCPACK"),
    ] {
        assert_eq!(kind.to_string(), want);
    }
}

#[test]
fn each_tftp_reason_reads_as_logged() {
    let mac = RECORDED;
    let cases: Vec<(Refused, &str)> = vec![
        (
            TftpError::Truncated(1).into(),
            "the packet is truncated: 1 bytes",
        ),
        (
            TftpError::Oversized(513).into(),
            "the request is oversized: 513 bytes, more than 512",
        ),
        (TftpError::NotRequest(4).into(), "opcode 4 is not a request"),
        (
            TftpError::Unterminated.into(),
            "a field of the request has no terminating NUL",
        ),
        (
            TftpError::NotText.into(),
            "a field of the request is not printable ASCII",
        ),
        (
            TftpError::NoValue("blksize".to_owned()).into(),
            "the option \"blksize\" has no value",
        ),
        (
            TftpError::DuplicateOption("tsize".to_owned()).into(),
            "the option \"tsize\" is named twice",
        ),
        (
            Refused::Write("grub/grub.cfg".to_owned()),
            "a write request for \"grub/grub.cfg\"",
        ),
        (
            Refused::Mode("netascii".to_owned()),
            "the mode \"netascii\": only octet is served",
        ),
        (
            Refused::Unsafe("../etc/passwd".to_owned()),
            "the name \"../etc/passwd\" is not a relative path of plain components",
        ),
        (
            Refused::Unknown("/etc/passwd".to_owned()),
            "\"/etc/passwd\" is not served",
        ),
        (
            Refused::NoLease,
            "hostkeeper/boot.efi from an address with no lease",
        ),
        (
            Refused::NoPlatform { mac },
            "hostkeeper/boot.efi for 0c:c4:7a:1f:ab:02, which stated no architecture and has no \
             record platform",
        ),
        (
            Refused::UnsupportedArch {
                mac,
                arch: ClientArch::new(10),
            },
            "hostkeeper/boot.efi for 0c:c4:7a:1f:ab:02, whose architecture 10 is not installed",
        ),
        (
            Refused::NoBootSet {
                mac,
                platform: Platform::LinuxX86_64,
            },
            "hostkeeper/boot.efi for 0c:c4:7a:1f:ab:02, which is linux-x86_64, with no boot set",
        ),
        (
            Refused::TooLarge {
                size: 33_553_920,
                blksize: 512,
            },
            "33553920 bytes need more than 65535 blocks of 512",
        ),
    ];
    for (refused, want) in cases {
        assert_eq!(refused.to_string(), want);
    }
}
