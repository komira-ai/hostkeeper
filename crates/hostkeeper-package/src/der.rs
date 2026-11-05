//! Just enough DER to read one extension of an X.509 certificate: the certificate's
//! `tbsCertificate`, its `[3]` extensions, and an extension's value by its object identifier.
//! Nothing here checks a signature; the verifier does that. What cannot be read is `None`.

/// The tags read here.
const BOOLEAN: u8 = 0x01;
const OCTET_STRING: u8 = 0x04;
const OBJECT_IDENTIFIER: u8 = 0x06;
const UTF8_STRING: u8 = 0x0c;
const SEQUENCE: u8 = 0x30;
/// `[3]`, constructed: the extensions of a `tbsCertificate`.
const EXTENSIONS: u8 = 0xa3;

/// The DER content of the object identifier `1.3.6.1.4.1.57264.1`, under which Sigstore's
/// certificate authority (Fulcio) puts what it read from the build's identity token.
const FULCIO: [u8; 9] = [0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xbf, 0x30, 0x01];
/// `1.3.6.1.4.1.57264.1.14`: the git ref the build ran on (`refs/tags/v1.2.3`).
pub(crate) const SOURCE_REPOSITORY_REF: u8 = 14;

/// The DER content of the object identifier `1.3.6.1.4.1.57264.1.<arc>`, for an arc below 128.
pub(crate) fn fulcio(arc: u8) -> Vec<u8> {
    let mut oid = FULCIO.to_vec();
    oid.push(arc & 0x7f);
    oid
}

/// The first element of `input`: its tag, its content and what follows it. Only the low tag
/// numbers and the definite lengths of at most four bytes are read.
pub(crate) fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if !(1..=4).contains(&count) {
            return None;
        }
        let (digits, rest) = rest.split_at_checked(count)?;
        let length = digits
            .iter()
            .fold(0_usize, |length, &digit| (length << 8) | usize::from(digit));
        (length, rest)
    };
    let (content, rest) = rest.split_at_checked(length)?;
    Some((tag, content, rest))
}

/// The content of the first element of `input` and what follows it, when its tag is `tag`.
fn take(tag: u8, input: &[u8]) -> Option<(&[u8], &[u8])> {
    tlv(input)
        .filter(|&(found, _, _)| found == tag)
        .map(|(_, content, rest)| (content, rest))
}

/// The elements of a constructed value's content, until the end or the first that cannot be
/// read.
struct Elements<'a>(&'a [u8]);

impl<'a> Iterator for Elements<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<(u8, &'a [u8])> {
        let (tag, content, rest) = tlv(self.0)?;
        self.0 = rest;
        Some((tag, content))
    }
}

/// The value (the content of `extnValue`) of the extension `oid` in the certificate `cert`.
fn extension<'a>(cert: &'a [u8], oid: &[u8]) -> Option<&'a [u8]> {
    let (certificate, _) = take(SEQUENCE, cert)?;
    let (tbs, _) = take(SEQUENCE, certificate)?;
    let (_, wrapped) = Elements(tbs).find(|&(tag, _)| tag == EXTENSIONS)?;
    let (list, _) = take(SEQUENCE, wrapped)?;
    Elements(list)
        .filter(|&(tag, _)| tag == SEQUENCE)
        .find_map(|(_, one)| value_of(one, oid))
}

/// The value of one `Extension` (`SEQUENCE { extnID, critical BOOLEAN OPTIONAL, extnValue }`)
/// when its identifier is `oid`.
fn value_of<'a>(extension: &'a [u8], oid: &[u8]) -> Option<&'a [u8]> {
    let (id, rest) = take(OBJECT_IDENTIFIER, extension)?;
    let rest = take(BOOLEAN, rest).map_or(rest, |(_, after)| after);
    let (value, _) = take(OCTET_STRING, rest)?;
    (id == oid).then_some(value)
}

/// The extension `oid` of the certificate `cert`, when its value is a DER `UTF8String`, as
/// Sigstore's own extensions are.
pub(crate) fn utf8_extension<'a>(cert: &'a [u8], oid: &[u8]) -> Option<&'a str> {
    let (text, _) = take(UTF8_STRING, extension(cert, oid)?)?;
    std::str::from_utf8(text).ok()
}

#[cfg(test)]
#[path = "der_tests.rs"]
mod tests;
