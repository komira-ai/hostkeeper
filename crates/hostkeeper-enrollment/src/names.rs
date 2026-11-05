//! The names enrollment deals in: host ids, roles, service names, the SHA-256 of a public key
//! (which is also a request's id), and MAC addresses.

use std::fmt;

/// A name that does not match its grammar.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{what} {value:?} does not match its grammar ({grammar})")]
pub struct BadName {
    /// What the name was to be: `host id`, `role` or `service name`.
    pub what: &'static str,
    /// The text that was refused.
    pub value: String,
    /// The grammar, as text.
    pub grammar: &'static str,
}

/// `true` when `text` is 1 to `max` bytes, each byte passes `allowed`, and it neither starts
/// nor ends with `-`.
fn matches(text: &str, max: usize, allowed: fn(u8) -> bool) -> bool {
    (1..=max).contains(&text.len())
        && text.bytes().all(allowed)
        && !text.starts_with('-')
        && !text.ends_with('-')
}

fn lower_digit_dash(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
}

/// A POSIX portable filename character (POSIX.1-2017, 3.282): `A-Z a-z 0-9 . _ -`.
fn portable_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-'
}

/// A host's id: a DNS label, `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$`. A certificate names its
/// host as `host:<id>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(String);

impl HostId {
    /// The grammar, as the refusal states it.
    pub const GRAMMAR: &'static str = "1 to 63 of a-z, 0-9 and -, not starting or ending with -";

    /// `text` as a host id.
    ///
    /// # Errors
    /// [`BadName`] when `text` does not match [`HostId::GRAMMAR`].
    pub fn parse(text: &str) -> Result<HostId, BadName> {
        if matches(text, 63, lower_digit_dash) {
            return Ok(HostId(text.to_owned()));
        }
        Err(BadName {
            what: "host id",
            value: text.to_owned(),
            grammar: Self::GRAMMAR,
        })
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A role: 1 to 32 bytes of `a-z0-9-` (design 11.8), not starting or ending with `-`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Role(String);

impl Role {
    /// The grammar, as the refusal states it.
    pub const GRAMMAR: &'static str = "1 to 32 of a-z, 0-9 and -, not starting or ending with -";

    /// `text` as a role.
    ///
    /// # Errors
    /// [`BadName`] when `text` does not match [`Role::GRAMMAR`].
    pub fn parse(text: &str) -> Result<Role, BadName> {
        if matches(text, 32, lower_digit_dash) {
            return Ok(Role(text.to_owned()));
        }
        Err(BadName {
            what: "role",
            value: text.to_owned(),
            grammar: Self::GRAMMAR,
        })
    }

    /// The role as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A service name (design 11.8): `^[A-Za-z0-9_][A-Za-z0-9._-]{0,254}$`, the POSIX portable
/// filename characters, 1 to 255 bytes, not beginning with `-` or `.`. The same rule as
/// `hostkeeper-config`'s; this crate depends on no workspace crate, so it holds its own copy.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceName(String);

impl ServiceName {
    /// The grammar, as the refusal states it.
    pub const GRAMMAR: &'static str =
        "1 to 255 of A-Z, a-z, 0-9, ., _ and -, not beginning with - or .";

    /// `text` as a service name.
    ///
    /// # Errors
    /// [`BadName`] when `text` does not match [`ServiceName::GRAMMAR`].
    pub fn parse(text: &str) -> Result<ServiceName, BadName> {
        let ok = (1..=255).contains(&text.len())
            && text.bytes().all(portable_byte)
            && !text.starts_with(['-', '.']);
        if ok {
            return Ok(ServiceName(text.to_owned()));
        }
        Err(BadName {
            what: "service name",
            value: text.to_owned(),
            grammar: Self::GRAMMAR,
        })
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The SHA-256 of a DER SubjectPublicKeyInfo. A request's id is the key hash of its CSR's key,
/// computed by the store, never taken from the client.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyHash([u8; 32]);

impl KeyHash {
    /// The SHA-256 of `spki_der`.
    pub fn of_spki(spki_der: &[u8]) -> KeyHash {
        let digest = ring::digest::digest(&ring::digest::SHA256, spki_der);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        KeyHash(out)
    }

    /// The hash whose bytes are `bytes`, which must be exactly 32.
    pub fn from_bytes(bytes: &[u8]) -> Option<KeyHash> {
        <[u8; 32]>::try_from(bytes).ok().map(KeyHash)
    }

    /// The hash spelled as 64 lower-case hex digits.
    pub fn from_hex(text: &str) -> Option<KeyHash> {
        let bytes = hex::decode(text).ok()?;
        // Upper-case digits decode too; only the one spelling `to_hex` gives is a hash.
        KeyHash::from_bytes(&bytes).filter(|hash| hash.to_hex() == text)
    }

    /// The 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The 64 lower-case hex digits: the spelling of file names and of the poll message.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for KeyHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for KeyHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyHash({})", self.to_hex())
    }
}

/// A MAC address: six octets. It reads as six pairs of hex digits separated all by `:` or all by
/// `-`, or as twelve hex digits with no separator, in either case (`AA-BB-CC-DD-EE-FF`,
/// `aa:bb:cc:dd:ee:ff`, `aabbccddeeff`), and is written in one spelling, lower-case and
/// colon-separated, so two spellings of one address compare equal.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    /// The grammar, as the refusal states it.
    pub const GRAMMAR: &'static str =
        "six pairs of hex digits separated all by : or all by -, or twelve hex digits";

    /// `text` as a MAC address.
    ///
    /// # Errors
    /// [`BadName`] when `text` does not match [`MacAddress::GRAMMAR`].
    pub fn parse(text: &str) -> Result<MacAddress, BadName> {
        Self::read(text).ok_or_else(|| BadName {
            what: "MAC address",
            value: text.to_owned(),
            grammar: Self::GRAMMAR,
        })
    }

    fn read(text: &str) -> Option<MacAddress> {
        let bytes = text.as_bytes();
        let digits: Vec<u8> = match bytes.get(2) {
            Some(&separator) if bytes.len() == 17 && matches!(separator, b':' | b'-') => {
                let separators_ok = bytes
                    .iter()
                    .enumerate()
                    .all(|(i, &b)| (i % 3 == 2) == (b == separator));
                if !separators_ok {
                    return None;
                }
                bytes.iter().copied().filter(|&b| b != separator).collect()
            }
            _ => bytes.to_vec(),
        };
        if digits.len() != 12 || !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let nibbles: Vec<u8> = digits
            .iter()
            .filter_map(|&b| char::from(b).to_digit(16))
            .filter_map(|d| u8::try_from(d).ok())
            .collect();
        let mut octets = [0u8; 6];
        for (octet, pair) in octets.iter_mut().zip(nibbles.chunks(2)) {
            *octet = pair.iter().fold(0, |high, low| (high << 4) | low);
        }
        Some(MacAddress(octets))
    }

    /// The six octets.
    pub fn octets(&self) -> [u8; 6] {
        self.0
    }

    /// Whether the address was assigned by the maker of the interface: a unicast address
    /// (the low bit of the first octet clear) that is universally administered (the next bit
    /// clear). A locally administered address (a bridge, a veth, a virtual machine's) is made up
    /// on the host and may repeat on another, so it binds no host.
    pub fn is_universal_unicast(&self) -> bool {
        self.0[0] & 0b11 == 0 && self.0 != [0; 6]
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

impl fmt::Debug for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MacAddress({self})")
    }
}
