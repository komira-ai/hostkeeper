//! A walker over DER as received. The decoders of `cms` re-encode (and sort) what they read, which
//! changes the bytes a signature was made over when the sender's encoding differs; this reads the
//! top-level layout from the bytes themselves and hands the owned decoders only the parts that are
//! not signed.
use der::{Any, Decode, Encode, Reader, SliceReader, Tag, Tagged};

/// What is wrong, as text for the reply's log line and the audit; never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Bad(pub &'static str);

pub(crate) type R<T> = Result<T, Bad>;

/// The TLVs that make up `value`, in order.
pub(crate) fn children(value: &[u8], what: &'static str) -> R<Vec<Any>> {
    let mut reader = SliceReader::new(value).ok().ok_or(Bad(what))?;
    let mut out = Vec::new();
    while !reader.is_finished() {
        out.push(Any::decode(&mut reader).map_err(|_| Bad(what))?);
    }
    Ok(out)
}

/// The children of `any`, which must carry `tag`.
pub(crate) fn children_of(any: &Any, tag: Tag, what: &'static str) -> R<Vec<Any>> {
    if any.tag() != tag {
        return Err(Bad(what));
    }
    children(any.value(), what)
}

/// `any` decoded as an owned `T`.
pub(crate) fn decode<T: for<'a> Decode<'a>>(any: &Any, what: &'static str) -> R<T> {
    let bytes = any.to_der().ok().ok_or(Bad(what))?;
    T::from_der(&bytes).map_err(|_| Bad(what))
}

/// The DER of a `SET` whose content is `value`: the bytes a signature over `[0] IMPLICIT SET OF
/// Attribute` is made over (RFC 5652 section 5.4).
pub(crate) fn as_set(value: &[u8]) -> R<Vec<u8>> {
    Any::new(Tag::Set, value)
        .ok()
        .and_then(|any| any.to_der().ok())
        .ok_or(Bad("signed attributes"))
}
