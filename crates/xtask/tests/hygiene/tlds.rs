//! The top-level domains a bare dotted name is read as a host for, and the file extensions that
//! make a dotted name a file name in some contexts.

use std::collections::HashSet;
use std::sync::OnceLock;

/// IANA's `tlds-alpha-by-domain.txt`, verbatim, pinned by a test.
pub const IANA_TEXT: &str = include_str!("iana-tlds.txt");

/// The file extensions of the sources, manifests, locks, text, archives and objects of this kind
/// of repository. Several are also top-level domains (`rs` is Serbia, `md` Moldova, `sh` Saint
/// Helena, `py` Paraguay, `so` Somalia, `zip`, `mov` and `pid` generic): a dotted name that ends
/// in one is a file name when it has exactly two labels or sits in a path, and a host otherwise.
#[rustfmt::skip]
pub const FILE_SUFFIXES: &[&str] = &[
    "rs", "md", "sh", "toml", "yml", "yaml", "json", "lock", "txt", "py", "c", "h", "o", "a",
    "so", "cc", "pl", "ml", "mk", "pm", "sv", "ps", "in", "am", "ac", "ar", "la", "zip", "mov",
    "pid",
];

/// Private-use suffixes that are not delegated top-level domains and so are not in the IANA
/// list, but that name internal hosts: a bare dotted name ending in one is read as a host.
pub const PRIVATE_TLDS: &[&str] = &[
    "internal",
    "local",
    "lan",
    "corp",
    "home",
    "intranet",
    "private",
    "localdomain",
];

/// The lowercase top-level domains of `IANA_TEXT`: every line that is not a comment or blank.
pub fn parse_tlds(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// True when `label` (lowercase) is a top-level domain of the pinned IANA list or a private-use
/// suffix of `PRIVATE_TLDS`.
pub fn is_tld(label: &str) -> bool {
    static TLDS: OnceLock<HashSet<String>> = OnceLock::new();
    TLDS.get_or_init(|| parse_tlds(IANA_TEXT)).contains(label) || PRIVATE_TLDS.contains(&label)
}
