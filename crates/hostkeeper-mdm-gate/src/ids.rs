//! The names the gate keys its state by: an MDM enrollment id, a command UUID, and a Mac serial
//! number.
//!
//! Each is text NanoHUB or the Mac gave, so each is checked before the gate keeps it: an
//! enrollment id names a file, and a serial is what the coordinator's allowlist is keyed by.

use std::fmt;

/// The NanoMDM enrollment id of a device channel: on a Mac, its UDID. It names the device's
/// record file, so it is 1 to 64 ASCII letters, digits and `-`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnrollmentId(String);

impl EnrollmentId {
    /// `text` as an enrollment id; `None` when it is not 1 to 64 ASCII letters, digits and `-`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let fits = (1..=64).contains(&text.len())
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        fits.then(|| Self(text.to_owned()))
    }

    /// The id as NanoHUB spells it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnrollmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The UUID of one command the gate enqueued, uppercase, in the version 4 layout. It is how a
/// result on the webhook is matched to the command it answers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandUuid(String);

impl CommandUuid {
    /// The UUID of the 16 random bytes `bytes`, with the version and variant bits set.
    #[must_use]
    pub fn from_random(mut bytes: [u8; 16]) -> Self {
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        Self(format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        ))
    }

    /// A UUID as a record file or a result spells it; `None` when it is not 36 characters of
    /// hexadecimal digits and `-`. Case is kept: NanoHUB gives it back as the gate sent it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let fits = text.len() == 36 && text.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
        fits.then(|| Self(text.to_owned()))
    }

    /// The UUID as the command carries it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `serial` could be a Mac serial number: 1 to 64 ASCII letters and digits, the grammar
/// of the coordinator's allowlist.
#[must_use]
pub fn is_serial(serial: &str) -> bool {
    (1..=64).contains(&serial.len()) && serial.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `text` made safe to log or keep as a reason: at most 200 characters, each one that is not
/// printable ASCII replaced by `?`.
#[must_use]
pub fn shown(text: &str) -> String {
    text.chars()
        .take(200)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}
