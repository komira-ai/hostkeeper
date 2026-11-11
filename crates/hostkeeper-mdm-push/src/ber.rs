//! A small reader of BER, the encoding of the CMS reply of mdmcert.download: DER, and the BER of
//! an encoder that streams (indefinite lengths, and an octet string in chunks), which a plain DER
//! parser refuses.
//!
//! It reads what [`crate::p7m`] needs: the class, constructed bit and tag of a value, its content,
//! a value's children, an object identifier, an octet string and an integer's bytes. Tags above 30
//! are refused, and nesting deeper than [`MAX_DEPTH`].

/// How deep values may nest.
pub const MAX_DEPTH: usize = 16;

/// Why a value could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BerError {
    /// The bytes end inside a value.
    #[error("the encoding ends inside a value")]
    Truncated,
    /// A length that cannot be one: more than four length bytes, or an indefinite length on a
    /// primitive value.
    #[error("the encoding has a length that is not valid")]
    Length,
    /// A tag of more than one byte.
    #[error("the encoding has a tag this reader does not take")]
    Tag,
    /// Values nest deeper than [`MAX_DEPTH`].
    #[error("the encoding nests too deep")]
    Depth,
    /// An object identifier or an octet string is not well formed.
    #[error("the encoding of a value is not valid")]
    Value,
}

/// One value: its tag and its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tlv<'a> {
    /// The class: 0 universal, 1 application, 2 context-specific, 3 private.
    pub class: u8,
    /// Whether the value is constructed (has children).
    pub constructed: bool,
    /// The tag number.
    pub tag: u32,
    /// The content, without its header and, for an indefinite length, without its end marker.
    pub content: &'a [u8],
}

/// The universal tag of a SEQUENCE.
pub const SEQUENCE: u32 = 16;
/// The universal tag of a SET.
pub const SET: u32 = 17;
/// The universal tag of an OBJECT IDENTIFIER.
pub const OID: u32 = 6;
/// The universal tag of an OCTET STRING.
pub const OCTET_STRING: u32 = 4;
/// The universal tag of an INTEGER.
pub const INTEGER: u32 = 2;

impl Tlv<'_> {
    /// Whether this is the universal value `tag`.
    #[must_use]
    pub fn is_universal(&self, tag: u32) -> bool {
        self.class == 0 && self.tag == tag
    }

    /// Whether this is the context-specific value `[tag]`.
    #[must_use]
    pub fn is_context(&self, tag: u32) -> bool {
        self.class == 2 && self.tag == tag
    }
}

/// Reads the value at the start of `buf`: it and what follows it.
///
/// # Errors
/// [`BerError`].
pub fn read(buf: &[u8]) -> Result<(Tlv<'_>, &[u8]), BerError> {
    read_at(buf, 0)
}

fn read_at(buf: &[u8], depth: usize) -> Result<(Tlv<'_>, &[u8]), BerError> {
    if depth > MAX_DEPTH {
        return Err(BerError::Depth);
    }
    let (&first, rest) = buf.split_first().ok_or(BerError::Truncated)?;
    if first & 0x1f == 0x1f {
        return Err(BerError::Tag);
    }
    let (&len_byte, mut rest) = rest.split_first().ok_or(BerError::Truncated)?;
    let class = first >> 6;
    let constructed = first & 0x20 != 0;
    let tag = u32::from(first & 0x1f);
    let length = if len_byte < 0x80 {
        Some(usize::from(len_byte))
    } else if len_byte == 0x80 {
        None
    } else {
        let count = usize::from(len_byte & 0x7f);
        if count > 4 {
            return Err(BerError::Length);
        }
        let bytes = rest.get(..count).ok_or(BerError::Truncated)?;
        rest = &rest[count..];
        Some(
            bytes
                .iter()
                .fold(0_usize, |n, b| (n << 8) | usize::from(*b)),
        )
    };
    let (content, after) = match length {
        Some(len) => {
            if rest.len() < len {
                return Err(BerError::Truncated);
            }
            rest.split_at(len)
        }
        None => {
            if !constructed {
                return Err(BerError::Length);
            }
            let mut inner = rest;
            loop {
                if let Some(after) = inner.strip_prefix(&[0, 0]) {
                    let used = rest.len() - inner.len();
                    break (&rest[..used], after);
                }
                inner = read_at(inner, depth + 1)?.1;
            }
        }
    };
    Ok((
        Tlv {
            class,
            constructed,
            tag,
            content,
        },
        after,
    ))
}

/// The values `content` holds, one after the other, to its end.
///
/// # Errors
/// [`BerError`].
pub fn children(content: &[u8]) -> Result<Vec<Tlv<'_>>, BerError> {
    children_at(content, 1)
}

fn children_at(mut content: &[u8], depth: usize) -> Result<Vec<Tlv<'_>>, BerError> {
    let mut out = Vec::new();
    while !content.is_empty() {
        let (tlv, rest) = read_at(content, depth)?;
        out.push(tlv);
        content = rest;
    }
    Ok(out)
}

/// The bytes of an octet string: its content, or its chunks joined when it is constructed.
///
/// # Errors
/// [`BerError::Value`]: it is not an octet string, or a chunk is not one.
pub fn octets(tlv: &Tlv<'_>) -> Result<Vec<u8>, BerError> {
    octets_at(tlv, 0)
}

fn octets_at(tlv: &Tlv<'_>, depth: usize) -> Result<Vec<u8>, BerError> {
    if !tlv.constructed {
        return Ok(tlv.content.to_vec());
    }
    let mut out = Vec::new();
    for chunk in children_at(tlv.content, depth + 1)? {
        if !chunk.is_universal(OCTET_STRING) {
            return Err(BerError::Value);
        }
        out.extend(octets_at(&chunk, depth + 1)?);
    }
    Ok(out)
}

/// The dotted text of an object identifier.
///
/// # Errors
/// [`BerError::Value`]: it is not an object identifier.
pub fn oid(tlv: &Tlv<'_>) -> Result<String, BerError> {
    if !tlv.is_universal(OID) || tlv.constructed || tlv.content.is_empty() {
        return Err(BerError::Value);
    }
    let mut arcs: Vec<u64> = Vec::new();
    let mut value: u64 = 0;
    let mut open = false;
    for &byte in tlv.content {
        value = value
            .checked_mul(128)
            .ok_or(BerError::Value)?
            .checked_add(u64::from(byte & 0x7f))
            .ok_or(BerError::Value)?;
        open = byte & 0x80 != 0;
        if !open {
            if arcs.is_empty() {
                let (first, second) = match value {
                    0..=39 => (0, value),
                    40..=79 => (1, value - 40),
                    _ => (2, value - 80),
                };
                arcs.extend([first, second]);
            } else {
                arcs.push(value);
            }
            value = 0;
        }
    }
    if open {
        return Err(BerError::Value);
    }
    Ok(arcs
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

#[cfg(test)]
#[path = "ber_tests.rs"]
mod tests;
