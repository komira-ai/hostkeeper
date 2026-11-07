//! A DHCP packet (RFC 2131, options of RFC 2132) as this crate reads and writes it.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use crate::{ClientArch, Mac};

/// The longest packet read: the UDP payload of a 1500-byte Ethernet frame (1500 less 20 bytes
/// of IPv4 header and 8 of UDP). A network-boot client sends far less; a longer datagram is
/// refused as oversized, so a caller reads into a buffer at least one byte longer than this.
pub const MAX_PACKET: usize = 1472;

/// The fixed BOOTP header before the magic cookie (RFC 2131 section 2).
const HEADER: usize = 236;
/// The magic cookie that starts the options (RFC 2131 section 3): 99, 130, 83, 99.
const COOKIE: [u8; 4] = [99, 130, 83, 99];
/// The shortest packet: the header and the cookie.
const MIN_PACKET: usize = HEADER + COOKIE.len();
/// The shortest reply sent: the BOOTP minimum of RFC 1542 section 2.1, which older PXE ROMs
/// hold servers to.
const MIN_REPLY: usize = 300;

/// The option codes read and written.
pub(crate) mod code {
    pub const PAD: u8 = 0;
    pub const VENDOR_OPTIONS: u8 = 43;
    pub const REQUESTED_ADDRESS: u8 = 50;
    pub const MESSAGE_TYPE: u8 = 53;
    pub const SERVER_ID: u8 = 54;
    pub const VENDOR_CLASS: u8 = 60;
    pub const BOOT_FILE: u8 = 67;
    pub const CLIENT_ARCH: u8 = 93;
    pub const CLIENT_UUID: u8 = 97;
    pub const END: u8 = 255;
}

/// Why a datagram is not a DHCP packet this crate reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DhcpError {
    /// Shorter than the header and the magic cookie.
    #[error(
        "the packet is truncated: {0} bytes, fewer than the 240 of the header and the magic cookie"
    )]
    Truncated(usize),
    /// Longer than [`MAX_PACKET`].
    #[error("the packet is oversized: {0} bytes, more than {MAX_PACKET}")]
    Oversized(usize),
    /// The magic cookie is not the one RFC 2131 gives.
    #[error("the packet has no DHCP magic cookie")]
    NoCookie,
    /// `htype` and `hlen` are not those of Ethernet (1 and 6).
    #[error("the hardware address is not Ethernet: htype {htype}, hlen {hlen}")]
    NotEthernet {
        /// The `htype` field.
        htype: u8,
        /// The `hlen` field.
        hlen: u8,
    },
    /// An option's length runs past the end of the packet.
    #[error("option {0} runs past the end of the packet")]
    OptionTruncated(u8),
    /// An option this crate reads has a length its RFC does not allow.
    #[error("option {code} has {len} bytes, which its definition does not allow")]
    BadOption {
        /// The option code.
        code: u8,
        /// Its length, after the parts of a split option are joined.
        len: usize,
    },
    /// No option 53: a BOOTP packet, not DHCP.
    #[error("the packet has no DHCP message type (option 53)")]
    NoMessageType,
}

/// The DHCP message type of option 53.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    /// 1.
    Discover,
    /// 2.
    Offer,
    /// 3.
    Request,
    /// 5.
    Ack,
    /// Any other value.
    Other(u8),
}

impl MessageType {
    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Discover,
            2 => Self::Offer,
            3 => Self::Request,
            5 => Self::Ack,
            other => Self::Other(other),
        }
    }
}

/// The message type of a proxy reply: its option 53 value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplyKind {
    /// DHCPOFFER, to a DISCOVER.
    Offer = 2,
    /// DHCPACK, to a REQUEST on UDP 4011.
    Ack = 5,
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discover => f.write_str("DHCPDISCOVER"),
            Self::Offer => f.write_str("DHCPOFFER"),
            Self::Request => f.write_str("DHCPREQUEST"),
            Self::Ack => f.write_str("DHCPACK"),
            Self::Other(other) => write!(f, "DHCP message type {other}"),
        }
    }
}

/// The network-boot class of a client, from the start of its vendor class (option 60).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootClass {
    /// `PXEClient`: UEFI PXE, which loads its boot file by TFTP.
    Pxe,
    /// `HTTPClient`: UEFI HTTP boot, which loads its boot file from an `http://` URL.
    Http,
}

impl BootClass {
    /// The class name a reply carries in option 60.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pxe => "PXEClient",
            Self::Http => "HTTPClient",
        }
    }

    /// The class of a vendor class: one that is the class name, or starts with it and a `:`
    /// (`PXEClient:Arch:00007:UNDI:003016`). Case matters, as in the PXE specification.
    fn of(vendor_class: &[u8]) -> Option<Self> {
        [Self::Pxe, Self::Http].into_iter().find(|class| {
            let name = class.name().as_bytes();
            vendor_class == name
                || (vendor_class.starts_with(name) && vendor_class.get(name.len()) == Some(&b':'))
        })
    }
}

/// A BOOTREQUEST read from a client (`op` 1). A packet whose `op` is not 1 reads as a
/// [`Packet`] too; [`Packet::is_request`] says which.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    op: u8,
    xid: [u8; 4],
    flags: [u8; 2],
    ciaddr: Ipv4Addr,
    giaddr: Ipv4Addr,
    chaddr: [u8; 16],
    message_type: MessageType,
    options: BTreeMap<u8, Vec<u8>>,
}

fn addr(bytes: &[u8], at: usize) -> Ipv4Addr {
    let mut octets = [0u8; 4];
    octets.copy_from_slice(&bytes[at..at + 4]);
    Ipv4Addr::from(octets)
}

/// The options after the cookie. A split option (RFC 3396) is joined in order; pad (0) is
/// skipped; reading stops at end (255), or at the end of the packet when there is none. The
/// `sname` and `file` fields are not read for options (option 52, overload, is not followed): a
/// client whose option 93 is there states no architecture, and is placed as one that states
/// none.
fn read_options(bytes: &[u8]) -> Result<BTreeMap<u8, Vec<u8>>, DhcpError> {
    let mut options: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut at = MIN_PACKET;
    while let Some(&code) = bytes.get(at) {
        match code {
            code::END => break,
            code::PAD => at += 1,
            code => {
                let len = usize::from(*bytes.get(at + 1).ok_or(DhcpError::OptionTruncated(code))?);
                let value = bytes
                    .get(at + 2..at + 2 + len)
                    .ok_or(DhcpError::OptionTruncated(code))?;
                options.entry(code).or_default().extend_from_slice(value);
                at += 2 + len;
            }
        }
    }
    Ok(options)
}

/// Checks the length of each option this crate reads.
fn check_lengths(options: &BTreeMap<u8, Vec<u8>>) -> Result<(), DhcpError> {
    for (&code, value) in options {
        let len = value.len();
        let fits = match code {
            code::MESSAGE_TYPE => len == 1,
            code::REQUESTED_ADDRESS | code::SERVER_ID => len == 4,
            code::CLIENT_ARCH => len >= 2 && len % 2 == 0,
            code::CLIENT_UUID => len == 17,
            _ => true,
        };
        if !fits {
            return Err(DhcpError::BadOption { code, len });
        }
    }
    Ok(())
}

impl Packet {
    /// Reads a DHCP packet.
    ///
    /// # Errors
    /// [`DhcpError`] for a truncated, oversized or malformed packet.
    pub fn parse(bytes: &[u8]) -> Result<Self, DhcpError> {
        if bytes.len() < MIN_PACKET {
            return Err(DhcpError::Truncated(bytes.len()));
        }
        if bytes.len() > MAX_PACKET {
            return Err(DhcpError::Oversized(bytes.len()));
        }
        if bytes[HEADER..MIN_PACKET] != COOKIE {
            return Err(DhcpError::NoCookie);
        }
        let (htype, hlen) = (bytes[1], bytes[2]);
        if (htype, hlen) != (1, 6) {
            return Err(DhcpError::NotEthernet { htype, hlen });
        }
        let options = read_options(bytes)?;
        check_lengths(&options)?;
        let message_type = options
            .get(&code::MESSAGE_TYPE)
            .map(|value| MessageType::from_byte(value[0]))
            .ok_or(DhcpError::NoMessageType)?;
        let mut xid = [0u8; 4];
        xid.copy_from_slice(&bytes[4..8]);
        let mut chaddr = [0u8; 16];
        chaddr.copy_from_slice(&bytes[28..44]);
        Ok(Self {
            op: bytes[0],
            xid,
            flags: [bytes[10], bytes[11]],
            ciaddr: addr(bytes, 12),
            giaddr: addr(bytes, 24),
            chaddr,
            message_type,
            options,
        })
    }

    /// Whether `op` is 1, BOOTREQUEST: a packet from a client.
    #[must_use]
    pub fn is_request(&self) -> bool {
        self.op == 1
    }

    /// The client's hardware address (`chaddr`).
    #[must_use]
    pub fn mac(&self) -> Mac {
        let mut octets = [0u8; 6];
        octets.copy_from_slice(&self.chaddr[..6]);
        Mac::new(octets)
    }

    /// The DHCP message type (option 53).
    #[must_use]
    pub fn message_type(&self) -> MessageType {
        self.message_type
    }

    /// The relay agent's address (`giaddr`); unspecified when no relay forwarded the packet.
    #[must_use]
    pub fn relay(&self) -> Ipv4Addr {
        self.giaddr
    }

    /// The network-boot class of the vendor class (option 60), if it is one.
    #[must_use]
    pub fn boot_class(&self) -> Option<BootClass> {
        self.options
            .get(&code::VENDOR_CLASS)
            .and_then(|value| BootClass::of(value))
    }

    /// The architecture of option 93: its first value, when it has one.
    #[must_use]
    pub fn arch(&self) -> Option<ClientArch> {
        self.options
            .get(&code::CLIENT_ARCH)
            .map(|value| ClientArch::new(u16::from_be_bytes([value[0], value[1]])))
    }

    /// The requested address (option 50).
    #[must_use]
    pub fn requested_address(&self) -> Option<Ipv4Addr> {
        self.options
            .get(&code::REQUESTED_ADDRESS)
            .map(|value| addr(value, 0))
    }

    /// The server identifier (option 54): the server a REQUEST is for.
    #[must_use]
    pub fn server_id(&self) -> Option<Ipv4Addr> {
        self.options
            .get(&code::SERVER_ID)
            .map(|value| addr(value, 0))
    }

    /// The client's machine identifier (option 97), which a PXE reply echoes.
    fn uuid(&self) -> Option<&[u8]> {
        self.options.get(&code::CLIENT_UUID).map(Vec::as_slice)
    }

    /// The proxy reply to this packet: a BOOTREPLY of `kind` that gives no address
    /// (`yiaddr` 0), with `siaddr` and option 54 `server`, the class name in option 60, the
    /// client's option 97 when it sent one, option 43 with PXE discovery control 8 (use the
    /// boot file of this reply, without boot server discovery), and `boot_file` in option 67
    /// and in the `file` field. `ciaddr` is the request's (0 in a DISCOVER, the client's address
    /// in a REQUEST to UDP 4011). Padded to the BOOTP minimum of 300 bytes.
    pub(crate) fn reply(
        &self,
        kind: ReplyKind,
        server: Ipv4Addr,
        class: BootClass,
        boot_file: &str,
    ) -> Vec<u8> {
        let mut out = vec![0u8; HEADER];
        out[0] = 2;
        out[1] = 1;
        out[2] = 6;
        out[4..8].copy_from_slice(&self.xid);
        out[10..12].copy_from_slice(&self.flags);
        out[12..16].copy_from_slice(&self.ciaddr.octets());
        out[20..24].copy_from_slice(&server.octets());
        out[28..44].copy_from_slice(&self.chaddr);
        let file = boot_file.as_bytes();
        // The `file` field holds 128 bytes and ends with a NUL; option 67 holds the whole name.
        for (slot, byte) in out[108..HEADER - 1].iter_mut().zip(file) {
            *slot = *byte;
        }
        out.extend_from_slice(&COOKIE);
        put(&mut out, code::MESSAGE_TYPE, &[kind as u8]);
        put(&mut out, code::SERVER_ID, &server.octets());
        put(&mut out, code::VENDOR_CLASS, class.name().as_bytes());
        if let Some(uuid) = self.uuid() {
            put(&mut out, code::CLIENT_UUID, uuid);
        }
        // PXE 2.1 vendor options: sub-option 6, PXE_DISCOVERY_CONTROL, of one byte 8; then end.
        put(&mut out, code::VENDOR_OPTIONS, &[6, 1, 8, code::END]);
        put(&mut out, code::BOOT_FILE, file);
        out.push(code::END);
        if out.len() < MIN_REPLY {
            out.resize(MIN_REPLY, code::PAD);
        }
        out
    }
}

/// Appends option `code` with `value`, split into parts of at most 255 bytes (RFC 3396).
fn put(out: &mut Vec<u8>, code: u8, value: &[u8]) {
    for part in value.chunks(255) {
        out.push(code);
        out.push(u8::try_from(part.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(part);
    }
}
