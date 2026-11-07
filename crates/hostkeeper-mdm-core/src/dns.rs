//! The host name rule the settings and the URLs share.

/// The longest label: 63 octets (RFC 1035 section 2.3.4).
pub(crate) const MAX_LABEL: usize = 63;
/// The longest name, written without a final dot: 253 octets (RFC 1035 section 2.3.4 allows
/// 255 on the wire, which is the 253 written characters plus the first length octet and the
/// root label).
pub(crate) const MAX_NAME: usize = 253;

/// Which letters a name may hold.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Case {
    /// Lower-case letters only: the settings' names, which the units and profiles carry as
    /// they are written.
    Lower,
    /// Either case: a URL's host, which DNS compares without case.
    Any,
}

/// Whether `name` is a host name of at least `min_labels` labels: labels of ASCII letters (of
/// `case`), digits and `-`, joined by single dots, none empty, none starting or ending with
/// `-`, none longer than [`MAX_LABEL`], and the whole no longer than [`MAX_NAME`] (RFC 1123
/// section 2.1). A final dot is refused, as an empty label.
pub(crate) fn is_host_name(name: &str, case: Case, min_labels: usize) -> bool {
    let letter = |c: char| match case {
        Case::Lower => c.is_ascii_lowercase(),
        Case::Any => c.is_ascii_alphabetic(),
    };
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= MAX_LABEL
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars()
                .all(|c| letter(c) || c.is_ascii_digit() || c == '-')
    };
    name.len() <= MAX_NAME && name.split('.').count() >= min_labels && name.split('.').all(label_ok)
}
