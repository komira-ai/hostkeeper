//! The PROXY protocol header and the sources of `--heartbeat-proxy-protocol-from`, without a
//! listener: what a source matches, why a value is refused, what each header names, and that
//! reading a header takes nothing past it.

use std::net::{IpAddr, SocketAddr};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{Proxies, ProxySource, ProxySourceError, Refused, read, version_1, version_2};

/// The signature a version 2 header starts with.
const SIGNATURE: &[u8; 12] = b"\r\n\r\n\0\r\nQUIT\n";

fn ip(text: &str) -> IpAddr {
    text.parse().expect("an IP address")
}

fn socket(text: &str) -> SocketAddr {
    text.parse().expect("a socket address")
}

fn source(text: &str) -> ProxySource {
    text.parse().expect("a source")
}

/// A version 2 header: the signature, `version_command`, `family`, then `addresses` and its
/// length.
pub(crate) fn v2(version_command: u8, family: u8, addresses: &[u8]) -> Vec<u8> {
    let mut header = SIGNATURE.to_vec();
    header.push(version_command);
    header.push(family);
    let length = u16::try_from(addresses.len()).expect("short addresses");
    header.extend_from_slice(&length.to_be_bytes());
    header.extend_from_slice(addresses);
    header
}

/// The addresses of a version 2 header of TCP over IPv4 from `source` to `destination`.
pub(crate) fn tcp4(source: SocketAddr, destination: SocketAddr) -> Vec<u8> {
    let (SocketAddr::V4(source), SocketAddr::V4(destination)) = (source, destination) else {
        panic!("IPv4 addresses");
    };
    let mut bytes = source.ip().octets().to_vec();
    bytes.extend_from_slice(&destination.ip().octets());
    bytes.extend_from_slice(&source.port().to_be_bytes());
    bytes.extend_from_slice(&destination.port().to_be_bytes());
    bytes
}

/// The addresses of a version 2 header of TCP over IPv6 from `source` to `destination`.
fn tcp6(source: SocketAddr, destination: SocketAddr) -> Vec<u8> {
    let (SocketAddr::V6(source), SocketAddr::V6(destination)) = (source, destination) else {
        panic!("IPv6 addresses");
    };
    let mut bytes = source.ip().octets().to_vec();
    bytes.extend_from_slice(&destination.ip().octets());
    bytes.extend_from_slice(&source.port().to_be_bytes());
    bytes.extend_from_slice(&destination.port().to_be_bytes());
    bytes
}

/// Proves: a single address trusts that address only, written as IPv4 or as the IPv4 address
/// written as IPv6 a dual-stack listener names a peer by; a network trusts each address in it and
/// none outside it nor of the other family; `/32` and `/128` are single addresses.
#[test]
fn a_source_contains_its_address_or_the_addresses_of_its_network() {
    let one = source("192.0.2.10");
    assert!(one.contains(ip("192.0.2.10")));
    assert!(one.contains(ip("::ffff:192.0.2.10")));
    assert!(!one.contains(ip("192.0.2.11")));
    assert!(!one.contains(ip("2001:db8::1")));
    assert_eq!(source("192.0.2.10/32"), one);

    let network = source("192.0.2.0/24");
    assert!(network.contains(ip("192.0.2.0")));
    assert!(network.contains(ip("192.0.2.255")));
    assert!(!network.contains(ip("198.51.100.1")));

    let v6 = source("2001:db8::/48");
    assert!(v6.contains(ip("2001:db8:0:ffff::5")));
    assert!(!v6.contains(ip("2001:db8:1::1")));
    assert!(!v6.contains(ip("192.0.2.1")));

    let host6 = source("2001:db8::1");
    assert!(host6.contains(ip("2001:db8::1")));
    assert!(!host6.contains(ip("2001:db8::2")));
    assert_eq!(source("2001:db8::1/128"), host6);
}

/// Proves: each value that is no source is refused with the reason, word for word: no address, a
/// prefix that is no number of the family's bits, `/0`, bits past the prefix, and an IPv4
/// address written as IPv6.
///
/// Catches: a `/0` (or a typo such as `/` or `/+8`) that would trust every peer to name any
/// address it likes.
#[test]
fn a_value_that_is_no_source_is_refused_with_its_reason() {
    let refused = |text: &str| {
        let why = text.parse::<ProxySource>().expect_err("refused");
        (why.clone(), why.to_string())
    };
    assert_eq!(
        refused("proxy.example"),
        (
            ProxySourceError::Address("proxy.example".to_owned()),
            r#""proxy.example" is no IP address"#.to_owned()
        )
    );
    assert_eq!(
        refused("192.0.2.0/33"),
        (
            ProxySourceError::Prefix("33".to_owned()),
            r#""33" is no prefix length of the address (IPv4 1 to 32, IPv6 1 to 128)"#.to_owned()
        )
    );
    assert_eq!(
        refused("192.0.2.0/+8").0,
        ProxySourceError::Prefix("+8".to_owned())
    );
    assert_eq!(
        refused("192.0.2.0/").0,
        ProxySourceError::Prefix(String::new())
    );
    assert_eq!(
        refused("2001:db8::/129").0,
        ProxySourceError::Prefix("129".to_owned())
    );
    assert_eq!(
        refused("0.0.0.0/0"),
        (
            ProxySourceError::Everything,
            "a /0 network trusts every source; name the proxy's address or its network".to_owned()
        )
    );
    assert_eq!(refused("::/0").0, ProxySourceError::Everything);
    assert_eq!(
        refused("192.0.2.10/24"),
        (
            ProxySourceError::HostBits("192.0.2.10/24".to_owned()),
            "192.0.2.10/24 has bits set past its prefix; write the network's address".to_owned()
        )
    );
    assert_eq!(
        refused("2001:db8::1/64").0,
        ProxySourceError::HostBits("2001:db8::1/64".to_owned())
    );
    assert_eq!(
        refused("::ffff:192.0.2.10"),
        (
            ProxySourceError::Mapped("::ffff:192.0.2.10".to_owned()),
            "::ffff:192.0.2.10 is an IPv4 address written as IPv6; write it as IPv4".to_owned()
        )
    );
}

/// Proves: a connection is read for a header only when its peer is one of the sources; one with
/// no peer address never is, and with no source none is.
#[test]
fn only_a_peer_among_the_sources_is_trusted() {
    let proxies = Proxies::new(&[source("192.0.2.10"), source("198.51.100.0/24")]);
    assert!(proxies.trusts(Some(socket("192.0.2.10:443"))));
    assert!(proxies.trusts(Some(socket("198.51.100.7:1"))));
    assert!(proxies.trusts(Some(socket("[::ffff:192.0.2.10]:443"))));
    assert!(!proxies.trusts(Some(socket("192.0.2.11:443"))));
    assert!(!proxies.trusts(None));
    assert!(!Proxies::default().trusts(Some(socket("192.0.2.10:443"))));
}

/// Proves: a version 2 `PROXY` header of TCP names its source address and port, over IPv4 and
/// IPv6, with TLVs after the addresses; `LOCAL`, `AF_UNSPEC` and a stream over `AF_UNIX` name
/// none; another version, another command, a datagram family and addresses cut short are
/// refused, each with its reason.
#[test]
fn a_version_2_header_names_its_source_or_none_or_is_refused() {
    let from = socket("203.0.113.5:40000");
    let to = socket("192.0.2.10:443");
    assert_eq!(version_2(0x21, 0x11, &tcp4(from, to)), Ok(Some(from)));
    let mut with_tlvs = tcp4(from, to);
    with_tlvs.extend_from_slice(&[0x04, 0x00, 0x01, 0xAA]);
    assert_eq!(version_2(0x21, 0x11, &with_tlvs), Ok(Some(from)));

    let from6 = socket("[2001:db8::5]:40000");
    let to6 = socket("[2001:db8::10]:443");
    assert_eq!(version_2(0x21, 0x21, &tcp6(from6, to6)), Ok(Some(from6)));

    assert_eq!(version_2(0x20, 0x11, &tcp4(from, to)), Ok(None));
    assert_eq!(version_2(0x20, 0x00, &[]), Ok(None));
    assert_eq!(version_2(0x21, 0x00, &[]), Ok(None));
    assert_eq!(version_2(0x21, 0x31, &[0; 216]), Ok(None));

    assert_eq!(
        version_2(0x11, 0x11, &tcp4(from, to)),
        Err("is of a version other than 2")
    );
    assert_eq!(
        version_2(0x22, 0x11, &tcp4(from, to)),
        Err("has a command other than LOCAL and PROXY")
    );
    assert_eq!(
        version_2(0x21, 0x12, &tcp4(from, to)),
        Err("names a family and protocol other than a stream over IPv4, IPv6 or UNIX")
    );
    assert_eq!(
        version_2(0x21, 0x11, &tcp4(from, to)[..9]),
        Err("is shorter than its IPv4 addresses")
    );
    assert_eq!(
        version_2(0x21, 0x21, &tcp6(from6, to6)[..15]),
        Err("is shorter than its IPv6 addresses")
    );
    assert_eq!(
        version_2(0x21, 0x21, &tcp6(from6, to6)[..33]),
        Err("is shorter than its IPv6 addresses")
    );
}

/// Proves: a version 1 header names its source over `TCP4` and `TCP6`; `UNKNOWN` names none
/// whatever follows; a line without its `\r\n` or that is not UTF-8, an address of the other
/// family, a port that is no decimal number below 65536, a field too many or too few and another
/// protocol are refused, each with its reason.
#[test]
fn a_version_1_header_names_its_source_or_none_or_is_refused() {
    let line = |text: &str| format!("{text}\r\n").into_bytes();
    assert_eq!(
        version_1(&line("PROXY TCP4 203.0.113.5 192.0.2.10 40000 443")),
        Ok(Some(socket("203.0.113.5:40000")))
    );
    assert_eq!(
        version_1(&line("PROXY TCP6 2001:db8::5 2001:db8::10 0 65535")),
        Ok(Some(socket("[2001:db8::5]:0")))
    );
    assert_eq!(version_1(&line("PROXY UNKNOWN")), Ok(None));
    assert_eq!(
        version_1(&line("PROXY UNKNOWN 2001:db8::5 2001:db8::10 1 2")),
        Ok(None)
    );

    let refused = |text: &str| version_1(&line(text));
    assert_eq!(
        version_1(b"PROXY TCP4 203.0.113.5 192.0.2.10 40000 443"),
        Err("is no line of text")
    );
    assert_eq!(version_1(b"PROXY \xff\r\n"), Err("is no line of text"));
    let v4 = Err("names an address that is no IPv4 address");
    assert_eq!(refused("PROXY TCP4 2001:db8::5 192.0.2.10 1 2"), v4);
    assert_eq!(refused("PROXY TCP4 203.0.113.5 2001:db8::10 1 2"), v4);
    let v6 = Err("names an address that is no IPv6 address");
    assert_eq!(refused("PROXY TCP6 203.0.113.5 2001:db8::10 1 2"), v6);
    assert_eq!(refused("PROXY TCP6 2001:db8::5 192.0.2.10 1 2"), v6);
    let port = Err("names a port that is no number from 0 to 65535");
    assert_eq!(refused("PROXY TCP4 203.0.113.5 192.0.2.10 65536 443"), port);
    assert_eq!(refused("PROXY TCP4 203.0.113.5 192.0.2.10 +1 443"), port);
    assert_eq!(refused("PROXY TCP4 203.0.113.5 192.0.2.10 1 -443"), port);
    assert_eq!(refused("PROXY TCP6 2001:db8::5 2001:db8::10 1 x"), port);
    let shape = Err("is not `PROXY TCP4`, `PROXY TCP6` or `PROXY UNKNOWN` and its fields");
    assert_eq!(refused("PROXY TCP4 203.0.113.5 192.0.2.10 40000"), shape);
    assert_eq!(
        refused("PROXY TCP4 203.0.113.5 192.0.2.10 40000 443 1"),
        shape
    );
    assert_eq!(
        refused("PROXY TCP4  203.0.113.5 192.0.2.10 40000 443"),
        shape
    );
    assert_eq!(
        refused("PROXY UDP4 203.0.113.5 192.0.2.10 40000 443"),
        shape
    );
}

/// What [`read`] makes of `sent` (after which the stream ends), and every byte left after it.
async fn read_all(sent: &[u8]) -> (Result<Option<SocketAddr>, String>, Vec<u8>) {
    let (mut near, mut far) = tokio::io::duplex(1 << 17);
    near.write_all(sent).await.expect("sent");
    drop(near);
    let header = read(&mut far).await.map_err(|why| why.to_string());
    let mut rest = Vec::new();
    far.read_to_end(&mut rest).await.expect("the rest");
    (header, rest)
}

/// Proves: a header of either version is read up to its last byte and no further, so what
/// follows it (the TLS handshake) is left whole, whether the header is the shortest of its kind
/// or carries TLVs.
///
/// Catches: a read that takes the first bytes of the handshake with the header, which would fail
/// every handshake after a header.
#[tokio::test]
async fn reading_a_header_takes_nothing_past_it() {
    const HELLO: &[u8] = b"\x16\x03\x01\x02\x00rest of the handshake";
    let from = socket("203.0.113.5:40000");
    let mut tlvs = tcp4(from, socket("192.0.2.10:443"));
    tlvs.extend_from_slice(&[0x04, 0x00, 0x01, 0xAA]);
    for (header, named) in [
        (v2(0x21, 0x11, &tlvs), Some(from)),
        (v2(0x20, 0x00, &[]), None),
        (
            b"PROXY TCP4 203.0.113.5 192.0.2.10 40000 443\r\n".to_vec(),
            Some(from),
        ),
        (b"PROXY UNKNOWN\r\n".to_vec(), None),
    ] {
        let mut sent = header.clone();
        sent.extend_from_slice(HELLO);
        assert_eq!(
            read_all(&sent).await,
            (Ok(named), HELLO.to_vec()),
            "{header:?}"
        );
    }
}

/// Proves: a stream that does not start with a header (a TLS handshake, or anything else), a
/// version 1 line longer than the specification's 107 bytes, a malformed header and a stream
/// that ends inside the header are each refused, with the reason the warning states.
#[tokio::test]
async fn a_stream_without_a_whole_header_is_refused() {
    let no_header = Err("the connection does not start with a PROXY protocol header".to_owned());
    assert_eq!(
        read_all(b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03\x00")
            .await
            .0,
        no_header
    );
    assert_eq!(read_all(b"PROXY:TCP4 203.0.113.5").await.0, no_header);

    let long = format!("PROXY UNKNOWN {}\r\n", "x".repeat(100));
    assert_eq!(
        read_all(long.as_bytes()).await,
        (
            Err("the PROXY protocol header is longer than 107 bytes".to_owned()),
            long.as_bytes()[107..].to_vec()
        )
    );
    let longest = format!("PROXY UNKNOWN {}\r\n", "x".repeat(91));
    assert_eq!(longest.len(), 107);
    assert_eq!(read_all(longest.as_bytes()).await.0, Ok(None));

    assert_eq!(
        read_all(&v2(0x11, 0x11, &[0; 12])).await.0,
        Err("the PROXY protocol header is of a version other than 2".to_owned())
    );
    assert_eq!(
        read_all(b"PROXY TCP4 x y 1 2\r\n").await.0,
        Err("the PROXY protocol header names an address that is no IPv4 address".to_owned())
    );

    let ended = Err("the PROXY protocol header could not be read: early eof".to_owned());
    assert_eq!(read_all(b"PROXY").await.0, ended);
    assert_eq!(read_all(SIGNATURE).await.0, ended);
    assert_eq!(
        read_all(
            &v2(
                0x21,
                0x11,
                &tcp4(socket("203.0.113.5:1"), socket("192.0.2.10:2"))
            )[..20]
        )
        .await
        .0,
        ended
    );
    // A byte at a time, a version 1 line that ends early says so in tokio's words.
    assert_eq!(
        read_all(b"PROXY TCP4 203.0.113.5").await.0,
        Err("the PROXY protocol header could not be read: unexpected end of file".to_owned())
    );
    assert!(matches!(
        read(&mut tokio::io::empty()).await,
        Err(Refused::Io(_))
    ));
}
