//! A tiny DER writer and a civil-time conversion, for the certificates and the timestamp of the
//! fixture signer (`signer.rs`).
//!
//! The fixture signer must hand `gh attestation verify` certificates and an RFC 3161 token whose
//! bytes are exactly what a real Fulcio and a real timestamp authority would produce. No crate of
//! the workspace writes those structures, so they are written here from their ASN.1 definitions.
//!
//! The one rule that keeps hand-written DER correct is that a length is never typed in: every
//! value is built by [`tlv`], which computes the length octets from the content it is given, in
//! the short form below 128 bytes and in the minimal long form above. The helpers on top of it
//! (`sequence`, `integer`, `oid`, ...) only choose the tag and shape the content. Callers that
//! need a SET OF in DER order sort the elements themselves before calling [`set`].
//!
//! Times are computed from unix seconds with [`Civil`] (days to civil date, proleptic Gregorian);
//! no date is written as a literal anywhere in the rig.

/// `BOOLEAN`.
pub const BOOLEAN: u8 = 0x01;
/// `INTEGER`.
pub const INTEGER: u8 = 0x02;
/// `BIT STRING`.
pub const BIT_STRING: u8 = 0x03;
/// `OCTET STRING`.
pub const OCTET_STRING: u8 = 0x04;
/// `NULL`.
pub const NULL: u8 = 0x05;
/// `OBJECT IDENTIFIER`.
pub const OBJECT_IDENTIFIER: u8 = 0x06;
/// `UTF8String`.
pub const UTF8_STRING: u8 = 0x0c;
/// `IA5String`.
pub const IA5_STRING: u8 = 0x16;
/// `UTCTime`.
pub const UTC_TIME: u8 = 0x17;
/// `GeneralizedTime`.
pub const GENERALIZED_TIME: u8 = 0x18;
/// `SEQUENCE` (constructed).
pub const SEQUENCE: u8 = 0x30;
/// `SET` (constructed).
pub const SET: u8 = 0x31;
/// The class and constructed bits of a context-specific tag: `[n]` is `CONTEXT | n`.
pub const CONTEXT: u8 = 0x80;
/// The constructed bit.
pub const CONSTRUCTED: u8 = 0x20;

/// The length octets of a value of `len` content bytes: one octet below 128, else `0x80 | n`
/// followed by the `n` big-endian octets of the length, without leading zeros.
pub fn length(len: usize) -> Vec<u8> {
    if let Ok(short) = u8::try_from(len)
        && short < 0x80
    {
        return vec![short];
    }
    let bytes = len.to_be_bytes();
    let first = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    let digits = &bytes[first..];
    let count = u8::try_from(digits.len()).expect("a usize has at most eight octets");
    let mut out = vec![0x80 | count];
    out.extend_from_slice(digits);
    out
}

/// One element: `tag`, the length of `content` and `content`.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let len = length(content.len());
    let mut out = Vec::with_capacity(1 + len.len() + content.len());
    out.push(tag);
    out.extend_from_slice(&len);
    out.extend_from_slice(content);
    out
}

/// `SEQUENCE { parts... }`: each part is a complete element, in the given order.
pub fn sequence(parts: Vec<Vec<u8>>) -> Vec<u8> {
    tlv(SEQUENCE, &parts.concat())
}

/// `SET { parts... }` with the parts in the given order; for a SET OF the caller passes them
/// already sorted by their encodings, as DER requires.
pub fn set(parts: Vec<Vec<u8>>) -> Vec<u8> {
    tlv(SET, &parts.concat())
}

/// A non-negative `INTEGER` from its big-endian magnitude: leading zero octets are dropped (one
/// is kept for zero) and one is added when the top bit would otherwise make the value negative.
pub fn integer(magnitude: &[u8]) -> Vec<u8> {
    let start = magnitude
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(magnitude.len().saturating_sub(1));
    let digits = magnitude.get(start..).unwrap_or_default();
    let mut content = Vec::with_capacity(digits.len() + 1);
    if digits.first().is_none_or(|&b| b >= 0x80) {
        content.push(0);
    }
    content.extend_from_slice(digits);
    tlv(INTEGER, &content)
}

/// A small non-negative `INTEGER` (a version number).
pub fn small_integer(value: u64) -> Vec<u8> {
    integer(&value.to_be_bytes())
}

/// `OBJECT IDENTIFIER` from its arcs (at least two): the first two arcs share one octet, every
/// arc is base 128 with the continuation bit on all but its last octet.
pub fn oid(arcs: &[u64]) -> Vec<u8> {
    assert!(
        arcs.len() >= 2,
        "an object identifier has at least two arcs"
    );
    let mut content = Vec::new();
    base128(arcs[0] * 40 + arcs[1], &mut content);
    for &arc in &arcs[2..] {
        base128(arc, &mut content);
    }
    tlv(OBJECT_IDENTIFIER, &content)
}

/// Appends `value` in base 128, most significant group first.
fn base128(mut value: u64, out: &mut Vec<u8>) {
    let mut groups = vec![(value & 0x7f) as u8];
    value >>= 7;
    while value > 0 {
        groups.push((value & 0x7f) as u8 | 0x80);
        value >>= 7;
    }
    groups.reverse();
    out.extend_from_slice(&groups);
}

/// `BOOLEAN` (DER: `FF` for true).
pub fn boolean(value: bool) -> Vec<u8> {
    tlv(BOOLEAN, &[if value { 0xff } else { 0x00 }])
}

/// `NULL`.
pub fn null() -> Vec<u8> {
    tlv(NULL, &[])
}

/// `UTF8String`.
pub fn utf8_string(text: &str) -> Vec<u8> {
    tlv(UTF8_STRING, text.as_bytes())
}

/// `IA5String`; the text must be ASCII.
pub fn ia5_string(text: &str) -> Vec<u8> {
    assert!(text.is_ascii(), "an IA5String is ASCII");
    tlv(IA5_STRING, text.as_bytes())
}

/// `OCTET STRING`.
pub fn octet_string(bytes: &[u8]) -> Vec<u8> {
    tlv(OCTET_STRING, bytes)
}

/// `BIT STRING` of whole octets, with `unused` unused bits in the last octet.
pub fn bit_string(unused: u8, bytes: &[u8]) -> Vec<u8> {
    let mut content = Vec::with_capacity(bytes.len() + 1);
    content.push(unused);
    content.extend_from_slice(bytes);
    tlv(BIT_STRING, &content)
}

/// `[number] EXPLICIT`: a constructed context tag around one complete element.
pub fn explicit(number: u8, element: &[u8]) -> Vec<u8> {
    tlv(CONTEXT | CONSTRUCTED | number, element)
}

/// `[number] IMPLICIT` of a primitive type: the context tag replaces the type's own.
pub fn implicit(number: u8, content: &[u8]) -> Vec<u8> {
    tlv(CONTEXT | number, content)
}

/// A certificate `Time`: `UTCTime` for the years 1950 to 2049, else `GeneralizedTime`
/// (the `Validity` rule of RFC 5280).
pub fn time(unix_seconds: i64) -> Vec<u8> {
    let civil = Civil::from_unix(unix_seconds);
    if (1950..2050).contains(&civil.year) {
        tlv(UTC_TIME, civil.utc_time().as_bytes())
    } else {
        generalized_time(unix_seconds)
    }
}

/// `GeneralizedTime` in whole seconds and UTC (`YYYYMMDDHHMMSSZ`), as an RFC 3161 `genTime` is.
pub fn generalized_time(unix_seconds: i64) -> Vec<u8> {
    tlv(
        GENERALIZED_TIME,
        Civil::from_unix(unix_seconds).generalized_time().as_bytes(),
    )
}

/// A UTC calendar date and time of day, from unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Civil {
    /// The civil time of `unix_seconds`, by the days-to-civil algorithm for the proleptic
    /// Gregorian calendar: days are counted from the first of March of year 0 so that the leap day is the last day
    /// of a year of 400-year eras.
    pub fn from_unix(unix_seconds: i64) -> Civil {
        let days = unix_seconds.div_euclid(86_400);
        let of_day = unix_seconds.rem_euclid(86_400);
        // Days since the first of March of year 0.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        // Months counted from March (0) to February (11).
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        let small = |n: i64| u8::try_from(n).expect("a calendar field fits a byte");
        Civil {
            year,
            month: small(month),
            day: small(day),
            hour: small(of_day / 3_600),
            minute: small(of_day % 3_600 / 60),
            second: small(of_day % 60),
        }
    }

    /// `YYMMDDHHMMSSZ`, the text of a `UTCTime` (two-digit year).
    pub fn utc_time(&self) -> String {
        format!(
            "{:02}{:02}{:02}{:02}{:02}{:02}Z",
            self.year.rem_euclid(100),
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        )
    }

    /// `YYYYMMDDHHMMSSZ`, the text of a `GeneralizedTime`.
    pub fn generalized_time(&self) -> String {
        format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `YYYY-MM-DDTHH:MM:SSZ`, RFC 3339 in UTC.
    pub fn rfc3339(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}
