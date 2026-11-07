//! Packets as a client sends them, built byte by byte here rather than by the crate, and a
//! reader of the options of a reply that is independent of the crate's.

use std::net::Ipv4Addr;

use hostkeeper_pxe_packets::dhcp::{AnswerSet, DhcpService, Mode, Outcome, Port, Reply, Server};
use hostkeeper_pxe_packets::{BootSets, Mac};
use hostkeeper_wire::Platform;

/// The server of every test: a documentation address.
pub const SERVER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
/// The HTTP port of every test.
pub const HTTP_PORT: u16 = 8080;
/// Another DHCP server: the router.
pub const ROUTER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
/// The address the router offers.
pub const OFFERED: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 77);

pub const RECORDED: Mac = Mac::new([0x0c, 0xc4, 0x7a, 0x1f, 0xab, 0x02]);
pub const ENROLLED: Mac = Mac::new([0x0c, 0xc4, 0x7a, 0x1f, 0xab, 0x03]);

pub const PXE_CLASS: &[u8] = b"PXEClient:Arch:00007:UNDI:003016";
pub const HTTP_CLASS: &[u8] = b"HTTPClient:Arch:00016:UNDI:003001";

/// The DHCP message types a client sends.
pub const DISCOVER: u8 = 1;
pub const REQUEST: u8 = 3;
pub const RELEASE: u8 = 7;
pub const INFORM: u8 = 8;

/// A client's packet, field by field.
#[derive(Clone)]
pub struct ClientPacket {
    pub op: u8,
    pub htype: u8,
    pub hlen: u8,
    pub xid: [u8; 4],
    pub flags: [u8; 2],
    pub ciaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    pub mac: Mac,
    pub cookie: [u8; 4],
    /// The options in order, each written as code, length, value.
    pub options: Vec<(u8, Vec<u8>)>,
    /// Whether the end option follows them.
    pub end: bool,
}

impl ClientPacket {
    /// A packet of `message_type` from `mac`, with no other option.
    pub fn new(message_type: u8, mac: Mac) -> Self {
        Self {
            op: 1,
            htype: 1,
            hlen: 6,
            xid: [0xde, 0xad, 0xbe, 0xef],
            flags: [0x80, 0x00],
            ciaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            mac,
            cookie: [99, 130, 83, 99],
            options: vec![(53, vec![message_type])],
            end: true,
        }
    }

    /// A PXE client's DISCOVER: vendor class `class`, option 93 `arch`.
    pub fn discover(mac: Mac, class: &[u8], arch: u16) -> Self {
        Self::new(DISCOVER, mac)
            .with(60, class)
            .with(93, &arch.to_be_bytes())
    }

    /// A PXE client's REQUEST to the proxy on UDP 4011, from the address the router gave.
    pub fn proxy_request(mac: Mac, class: &[u8], arch: u16) -> Self {
        let mut packet = Self::new(REQUEST, mac)
            .with(60, class)
            .with(93, &arch.to_be_bytes());
        packet.ciaddr = OFFERED;
        packet.flags = [0, 0];
        packet
    }

    /// A selecting REQUEST, broadcast to the router: options 50 and 54.
    pub fn selecting(mac: Mac) -> Self {
        Self::new(REQUEST, mac)
            .with(50, &OFFERED.octets())
            .with(54, &ROUTER.octets())
    }

    /// This packet with the option `code` of `value` appended.
    pub fn with(mut self, code: u8, value: &[u8]) -> Self {
        self.options.push((code, value.to_vec()));
        self
    }

    /// This packet with every option `code` removed.
    pub fn without(mut self, code: u8) -> Self {
        self.options.retain(|(c, _)| *c != code);
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; 236];
        out[0] = self.op;
        out[1] = self.htype;
        out[2] = self.hlen;
        out[4..8].copy_from_slice(&self.xid);
        out[10..12].copy_from_slice(&self.flags);
        out[12..16].copy_from_slice(&self.ciaddr.octets());
        out[24..28].copy_from_slice(&self.giaddr.octets());
        out[28..34].copy_from_slice(&self.mac.octets());
        out.extend_from_slice(&self.cookie);
        for (code, value) in &self.options {
            out.push(*code);
            out.push(u8::try_from(value.len()).expect("a short option"));
            out.extend_from_slice(value);
        }
        if self.end {
            out.push(255);
        }
        out
    }
}

/// Both boot sets.
pub fn both_sets() -> BootSets {
    BootSets::new([Platform::LinuxX86_64, Platform::LinuxArm64]).expect("Linux boot sets")
}

/// The answer set of the recorded MAC with `platform`.
pub fn answers(platform: Option<Platform>) -> AnswerSet {
    AnswerSet::from([(RECORDED, platform)])
}

/// A service of `mode` naming the test server.
pub fn service(mode: Mode) -> DhcpService {
    DhcpService::new(
        mode,
        Server {
            address: SERVER,
            http_port: HTTP_PORT,
        },
    )
}

/// The proxy-mode outcome of `packet` on `port`, against `answers` and both boot sets.
pub fn proxy(port: Port, packet: &ClientPacket, answers: &AnswerSet) -> Outcome {
    service(Mode::Proxy).on_packet(port, &packet.bytes(), answers, &both_sets())
}

/// The reply of an outcome that must be one.
pub fn reply(outcome: Outcome) -> Reply {
    match outcome {
        Outcome::Reply(reply) => reply,
        other => panic!("expected a reply, got {other:?}"),
    }
}

/// The options of a reply, in order, read here and not by the crate. Parts of a split option
/// stay separate entries.
pub fn reply_options(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    assert_eq!(bytes[236..240], [99, 130, 83, 99], "the magic cookie");
    let mut out = Vec::new();
    let mut at = 240;
    loop {
        match bytes[at] {
            255 => break,
            0 => at += 1,
            code => {
                let len = usize::from(bytes[at + 1]);
                out.push((code, bytes[at + 2..at + 2 + len].to_vec()));
                at += 2 + len;
            }
        }
    }
    assert!(
        bytes[at + 1..].iter().all(|&b| b == 0),
        "only padding follows the end option"
    );
    out
}
