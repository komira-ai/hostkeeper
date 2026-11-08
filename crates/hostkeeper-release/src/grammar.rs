//! The small grammars of a release: a SHA-256 digest, an installer's and a store's name, and a
//! locator (packages.md sections 3.4 and 7.1).

/// The most bytes of an installer's name, and of a store's.
pub const MAX_NAME: usize = 32;
/// The most bytes of a locator.
pub const MAX_LOCATOR: usize = 1024;

/// Whether `text` is a SHA-256 digest as 64 lower-case hex characters.
pub fn is_sha256(text: &str) -> bool {
    (text.len() == 64)
        & text
            .bytes()
            .all(|b| b.is_ascii_digit() | (b'a'..=b'f').contains(&b))
}

/// Whether `text` is 1 to [`MAX_NAME`] bytes of `[a-z0-9-]`.
fn is_name(text: &str) -> bool {
    (1..=MAX_NAME).contains(&text.len())
        & text
            .bytes()
            .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'-'))
}

/// Whether `text` may name an installer: 1 to 32 bytes of `[a-z0-9-]`. Whether a daemon knows
/// it is the daemon's business; a name no daemon lists is never picked.
pub fn is_installer_name(text: &str) -> bool {
    is_name(text)
}

/// Whether `text` may name a store of a host's `stores.toml`: 1 to 32 bytes of `[a-z0-9-]`.
pub fn is_store_name(text: &str) -> bool {
    is_name(text)
}

/// Why a locator is refused, in the order the checks run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocatorError {
    /// No byte at all.
    #[error("is empty")]
    Empty,
    /// Over [`MAX_LOCATOR`] bytes.
    #[error("is {0} bytes, more than 1024")]
    TooLong(usize),
    /// A `%`: no escape is taken, so no two spellings name one path.
    #[error("holds `%`; a locator takes no escape")]
    Percent,
    /// A `?`, the start of a query.
    #[error("holds `?`; a locator has no query")]
    Query,
    /// A `#`, the start of a fragment.
    #[error("holds `#`; a locator has no fragment")]
    Fragment,
    /// A character outside the characters of a URL path.
    #[error("holds {0:?}, which is outside the characters of a URL path")]
    Char(char),
    /// An empty segment: a leading or trailing `/`, or `//`.
    #[error("has an empty segment (a leading, trailing or doubled `/`)")]
    EmptySegment,
    /// A `.` or `..` segment.
    #[error("has a `.` or `..` segment")]
    DotSegment,
    /// A `:` in the first segment, which a URL reads as a scheme.
    #[error("has a `:` in its first segment, which reads as a scheme")]
    Scheme,
}

/// Whether `b` is a character of a URL path segment (RFC 3986 `pchar`) other than `%`: the
/// unreserved characters, the sub-delimiters, `:` and `@`.
fn is_path_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() | b"-._~!$&'()*+,;=:@".contains(&b)
}

/// Checks a locator: 1 to 1024 bytes of URL path characters, segments joined by `/`, with no
/// scheme, no empty segment (so no `//`), no query, no fragment, no `%` and no `.` or `..`
/// segment (section 7.1). It is joined to its store's base as it is, so nothing in it can
/// leave the base or reach another host.
pub fn check_locator(text: &str) -> Result<(), LocatorError> {
    if text.is_empty() {
        return Err(LocatorError::Empty);
    }
    if text.len() > MAX_LOCATOR {
        return Err(LocatorError::TooLong(text.len()));
    }
    let special = [
        ('%', LocatorError::Percent),
        ('?', LocatorError::Query),
        ('#', LocatorError::Fragment),
    ];
    if let Some((_, why)) = special.into_iter().find(|(c, _)| text.contains(*c)) {
        return Err(why);
    }
    if let Some(c) = text
        .chars()
        .find(|&c| !(c.is_ascii() && (is_path_char(c as u8) | (c == '/'))))
    {
        return Err(LocatorError::Char(c));
    }
    if text.split('/').any(str::is_empty) {
        return Err(LocatorError::EmptySegment);
    }
    if text.split('/').any(|s| (s == ".") | (s == "..")) {
        return Err(LocatorError::DotSegment);
    }
    if text
        .split('/')
        .next()
        .is_some_and(|first| first.contains(':'))
    {
        return Err(LocatorError::Scheme);
    }
    Ok(())
}
