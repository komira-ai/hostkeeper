//! Truncated, oversized and malformed DHCP packets: refused by name, in each mode and on each
//! port, and never answered or learned from.

use hostkeeper_pxe_packets::dhcp::{DhcpError, Ignored, MAX_PACKET, Mode, Outcome, Packet, Port};

use crate::build::{
    ClientPacket, DISCOVER, OFFERED, PXE_CLASS, RECORDED, answers, both_sets, reply, service,
};

/// A well-formed DISCOVER with `n` bytes of pad options before its end option.
fn padded(n: usize) -> Vec<u8> {
    let mut bytes = ClientPacket::discover(RECORDED, PXE_CLASS, 7).bytes();
    let end = bytes.pop();
    bytes.extend(std::iter::repeat_n(0u8, n));
    bytes.extend(end);
    bytes
}

/// The malformed packets, each with the error it is refused with.
fn malformed() -> Vec<(&'static str, Vec<u8>, DhcpError)> {
    let discover = || ClientPacket::discover(RECORDED, PXE_CLASS, 7);
    let mut no_cookie = discover();
    no_cookie.cookie = [99, 130, 83, 98];
    let mut token_ring = discover();
    token_ring.htype = 6;
    let mut long_hlen = discover();
    long_hlen.hlen = 16;
    let mut past_end = discover().bytes();
    past_end.pop();
    past_end.extend_from_slice(&[60, 9, b'P']);
    let mut no_length = discover().bytes();
    no_length.pop();
    no_length.push(60);
    let full = padded(MAX_PACKET - padded(0).len());
    let oversized = padded(MAX_PACKET - padded(0).len() + 1);
    assert_eq!(full.len(), MAX_PACKET);
    let short = discover().bytes()[..239].to_vec();
    let bad_length = |code: u8, value: &[u8]| discover().without(code).with(code, value).bytes();
    vec![
        ("an empty datagram", Vec::new(), DhcpError::Truncated(0)),
        ("a truncated header", short, DhcpError::Truncated(239)),
        (
            "one byte over the bound",
            oversized,
            DhcpError::Oversized(MAX_PACKET + 1),
        ),
        (
            "a wrong magic cookie",
            no_cookie.bytes(),
            DhcpError::NoCookie,
        ),
        (
            "a hardware type that is not Ethernet",
            token_ring.bytes(),
            DhcpError::NotEthernet { htype: 6, hlen: 6 },
        ),
        (
            "a hardware length that is not 6",
            long_hlen.bytes(),
            DhcpError::NotEthernet { htype: 1, hlen: 16 },
        ),
        (
            "an option past the end",
            past_end,
            DhcpError::OptionTruncated(60),
        ),
        (
            "an option with no length",
            no_length,
            DhcpError::OptionTruncated(60),
        ),
        (
            "a message type of two bytes",
            bad_length(53, &[1, 1]),
            DhcpError::BadOption { code: 53, len: 2 },
        ),
        (
            "an option 93 of one byte",
            bad_length(93, &[7]),
            DhcpError::BadOption { code: 93, len: 1 },
        ),
        (
            "an option 93 of three bytes",
            bad_length(93, &[0, 7, 0]),
            DhcpError::BadOption { code: 93, len: 3 },
        ),
        (
            "an empty option 93",
            bad_length(93, &[]),
            DhcpError::BadOption { code: 93, len: 0 },
        ),
        (
            "an option 50 of three bytes",
            bad_length(50, &[192, 0, 2]),
            DhcpError::BadOption { code: 50, len: 3 },
        ),
        (
            "an option 54 of five bytes",
            bad_length(54, &[192, 0, 2, 1, 0]),
            DhcpError::BadOption { code: 54, len: 5 },
        ),
        (
            "an option 97 of sixteen bytes",
            bad_length(97, &[0; 16]),
            DhcpError::BadOption { code: 97, len: 16 },
        ),
        (
            "no message type",
            discover().without(53).bytes(),
            DhcpError::NoMessageType,
        ),
    ]
}

/// Each malformed packet is refused with its own error, in each mode and on each port; the
/// router learns nothing from it.
#[test]
fn a_malformed_packet_is_refused_by_name_in_each_mode() {
    for (what, bytes, error) in malformed() {
        assert_eq!(Packet::parse(&bytes), Err(error.clone()), "{what}");
        for mode in [Mode::Proxy, Mode::Router] {
            for port in [Port::Dhcp, Port::ProxyDhcp] {
                let mut dhcp = service(mode);
                let got = dhcp.on_packet(port, &bytes, &answers(None), &both_sets());
                assert_eq!(
                    got,
                    Outcome::Ignored(Ignored::Malformed(error.clone())),
                    "{what}"
                );
                assert_eq!(dhcp.lease(OFFERED), None, "{what}");
            }
        }
    }
}

/// The bounds themselves are packets: 240 bytes (the header and the cookie) reads to the
/// message type check, and [`MAX_PACKET`] bytes reads.
#[test]
fn a_packet_at_either_bound_is_read() {
    let mut header_only = ClientPacket::new(DISCOVER, RECORDED).bytes();
    header_only.truncate(240);
    assert_eq!(Packet::parse(&header_only), Err(DhcpError::NoMessageType));
    let full = padded(MAX_PACKET - padded(0).len());
    let offer =
        reply(service(Mode::Proxy).on_packet(Port::Dhcp, &full, &answers(None), &both_sets()));
    assert_eq!(offer.mac, RECORDED);
}

/// Options after the end option are not read, and a packet whose options run to its last byte
/// with no end option is read to there.
#[test]
fn options_end_at_the_end_option_or_the_last_byte() {
    let mut after_end = ClientPacket::discover(RECORDED, PXE_CLASS, 7).bytes();
    after_end.extend_from_slice(&[93, 2, 0, 11]);
    let packet = Packet::parse(&after_end).expect("a DISCOVER");
    assert_eq!(packet.arch().map(|a| a.value()), Some(7));
    let mut no_end = ClientPacket::discover(RECORDED, PXE_CLASS, 11);
    no_end.end = false;
    let packet = Packet::parse(&no_end.bytes()).expect("a DISCOVER");
    assert_eq!(packet.arch().map(|a| a.value()), Some(11));
}
