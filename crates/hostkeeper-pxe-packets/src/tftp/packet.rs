//! TFTP packets (RFC 1350) and the options of a read request (RFC 2347, 2348, 2349).

/// The longest request read: RFC 2347 holds a request with its options to 512 bytes.
pub const MAX_REQUEST: usize = 512;

/// The largest block size accepted: what fits a 1500-byte Ethernet frame (1500 less 20 bytes
/// of IPv4 header, 8 of UDP and 4 of TFTP header), so no block is fragmented. A client that
/// asks for more is given this (RFC 2348 lets the server answer a smaller size).
pub const MAX_BLKSIZE: u16 = 1468;

/// The block size of RFC 1350, when no `blksize` is negotiated.
pub const DEFAULT_BLKSIZE: u16 = 512;

/// The smallest and largest `blksize` RFC 2348 allows a client to ask for.
const BLKSIZE_RANGE: std::ops::RangeInclusive<u32> = 8..=65464;

/// The opcodes of RFC 1350 and RFC 2347.
pub(crate) mod opcode {
    pub const RRQ: u16 = 1;
    pub const WRQ: u16 = 2;
    pub const DATA: u16 = 3;
    pub const ACK: u16 = 4;
    pub const ERROR: u16 = 5;
    pub const OACK: u16 = 6;
}

/// The error codes of RFC 1350 this server sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// 0, not defined: the message says what.
    NotDefined,
    /// 1, file not found.
    FileNotFound,
    /// 2, access violation.
    AccessViolation,
    /// 4, illegal TFTP operation.
    IllegalOperation,
    /// 5, unknown transfer ID: a packet to a transfer's port from another client.
    UnknownTid,
}

impl ErrorCode {
    /// The code on the wire.
    #[must_use]
    pub const fn value(self) -> u16 {
        match self {
            Self::NotDefined => 0,
            Self::FileNotFound => 1,
            Self::AccessViolation => 2,
            Self::IllegalOperation => 4,
            Self::UnknownTid => 5,
        }
    }

    /// The message sent with the code. It names no file, MAC or platform: the reason is for
    /// the log, not the client.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotDefined => "not served",
            Self::FileNotFound => "file not found",
            Self::AccessViolation => "access violation",
            Self::IllegalOperation => "illegal TFTP operation",
            Self::UnknownTid => "unknown transfer ID",
        }
    }

    /// The ERROR packet of this code and its message.
    #[must_use]
    pub fn packet(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + self.message().len());
        out.extend_from_slice(&opcode::ERROR.to_be_bytes());
        out.extend_from_slice(&self.value().to_be_bytes());
        out.extend_from_slice(self.message().as_bytes());
        out.push(0);
        out
    }
}

/// Why a datagram is not a TFTP packet this crate reads.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TftpError {
    /// Too short to hold its opcode and the fields it needs.
    #[error("the packet is truncated: {0} bytes")]
    Truncated(usize),
    /// A request longer than [`MAX_REQUEST`].
    #[error("the request is oversized: {0} bytes, more than {MAX_REQUEST}")]
    Oversized(usize),
    /// Not a read or write request, on the server port.
    #[error("opcode {0} is not a request")]
    NotRequest(u16),
    /// A string field with no terminating NUL.
    #[error("a field of the request has no terminating NUL")]
    Unterminated,
    /// A string field that is not printable ASCII.
    #[error("a field of the request is not printable ASCII")]
    NotText,
    /// An option name with no value.
    #[error("the option {0:?} has no value")]
    NoValue(String),
    /// An option named twice.
    #[error("the option {0:?} is named twice")]
    DuplicateOption(String),
}

/// The options of a read request this server takes; every other option is left out of the
/// OACK, which RFC 2347 lets a server do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The block size the client asked for (RFC 2348), at most [`MAX_BLKSIZE`]; `None` when it
    /// asked for none or for a size outside 8..=65464.
    pub blksize: Option<u16>,
    /// Whether the client asked for the transfer size (RFC 2349) with a decimal value.
    pub tsize: bool,
}

/// A request read from the server port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Whether it is a write request (WRQ), which is always refused.
    pub write: bool,
    /// The file name, as the client sent it.
    pub name: String,
    /// The transfer mode, as the client sent it.
    pub mode: String,
    /// The options this server takes.
    pub options: Options,
}

fn decimal(value: &str) -> Option<u32> {
    if value.is_empty() || value.len() > 9 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// The NUL-terminated printable ASCII strings of `bytes`, which must end with a NUL.
fn fields(bytes: &[u8]) -> Result<Vec<String>, TftpError> {
    let body = bytes.strip_suffix(&[0]).ok_or(TftpError::Unterminated)?;
    body.split(|&b| b == 0)
        .map(|field| {
            if field.iter().all(|b| (0x20..0x7f).contains(b)) {
                Ok(String::from_utf8_lossy(field).into_owned())
            } else {
                Err(TftpError::NotText)
            }
        })
        .collect()
}

/// Reads a request.
///
/// # Errors
/// [`TftpError`] for a packet that is not a well-formed RRQ or WRQ.
pub fn parse_request(bytes: &[u8]) -> Result<Request, TftpError> {
    if bytes.len() > MAX_REQUEST {
        return Err(TftpError::Oversized(bytes.len()));
    }
    let (op, rest) = bytes
        .split_first_chunk::<2>()
        .ok_or(TftpError::Truncated(bytes.len()))?;
    let op = u16::from_be_bytes(*op);
    if op != opcode::RRQ && op != opcode::WRQ {
        return Err(TftpError::NotRequest(op));
    }
    let mut fields = fields(rest)?.into_iter();
    let (Some(name), Some(mode)) = (fields.next(), fields.next()) else {
        return Err(TftpError::Truncated(bytes.len()));
    };
    let mut options = Options::default();
    let mut seen: Vec<String> = Vec::new();
    while let Some(option) = fields.next() {
        let value = fields
            .next()
            .ok_or_else(|| TftpError::NoValue(option.clone()))?;
        let option = option.to_ascii_lowercase();
        if seen.contains(&option) {
            return Err(TftpError::DuplicateOption(option));
        }
        match option.as_str() {
            "blksize" => {
                options.blksize = decimal(&value)
                    .filter(|size| BLKSIZE_RANGE.contains(size))
                    .map(|size| u16::try_from(size).unwrap_or(MAX_BLKSIZE).min(MAX_BLKSIZE));
            }
            "tsize" => options.tsize = decimal(&value).is_some(),
            _ => {}
        }
        seen.push(option);
    }
    Ok(Request {
        write: op == opcode::WRQ,
        name,
        mode,
        options,
    })
}

/// A packet from the client of a transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientPacket {
    /// ACK of a block (0 acknowledges the OACK).
    Ack(u16),
    /// ERROR: the client ends the transfer.
    Error {
        /// Its code.
        code: u16,
        /// Its message, with anything that is not printable ASCII replaced.
        message: String,
    },
    /// Any other packet, or one too short for its opcode.
    Illegal,
}

impl ClientPacket {
    /// Reads a packet the client of a transfer sent.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let op = bytes.get(..2).map(|op| u16::from_be_bytes([op[0], op[1]]));
        let word = bytes.get(2..4).map(|w| u16::from_be_bytes([w[0], w[1]]));
        match (op, word) {
            (Some(opcode::ACK), Some(block)) if bytes.len() == 4 => Self::Ack(block),
            (Some(opcode::ERROR), Some(code)) => {
                let text = &bytes[4..];
                let text = text.split(|&b| b == 0).next().unwrap_or_default();
                let message = text
                    .iter()
                    .map(|&b| {
                        if (0x20..0x7f).contains(&b) {
                            char::from(b)
                        } else {
                            '?'
                        }
                    })
                    .collect();
                Self::Error { code, message }
            }
            _ => Self::Illegal,
        }
    }
}

/// The DATA packet of `block` carrying `payload`.
#[must_use]
pub fn data_packet(block: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&opcode::DATA.to_be_bytes());
    out.extend_from_slice(&block.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// The OACK of the options taken (RFC 2347): `blksize` and `tsize` with their values.
pub(crate) fn oack(blksize: Option<u16>, tsize: Option<u64>) -> Vec<u8> {
    let mut out = opcode::OACK.to_be_bytes().to_vec();
    let pairs = [
        blksize.map(|size| ("blksize", size.to_string())),
        tsize.map(|size| ("tsize", size.to_string())),
    ];
    for (name, value) in pairs.into_iter().flatten() {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
    out
}
