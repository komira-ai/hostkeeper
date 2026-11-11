//! dpkg's grammars of a package's name and version, and its order of versions
//! (`deb-version(7)`): what `apt` adds to the union grammars of `hostkeeper-release`
//! (packages-repositories.md section 2.2, "A7 owns apt's grammars").

use std::cmp::Ordering;

/// Whether `name` is a dpkg package name: at least two bytes of `[a-z0-9+.-]`, a letter or
/// digit first.
pub fn is_package(name: &str) -> bool {
    let bytes = name.as_bytes();
    (2..=128).contains(&bytes.len())
        & bytes
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() | b.is_ascii_digit())
        & bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | b"+.-".contains(b))
}

/// A version's three parts.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Parts<'a> {
    epoch: u64,
    upstream: &'a str,
    revision: &'a str,
}

/// The parts of `version`, `None` when it is not a dpkg version: `[epoch:]upstream[-revision]`,
/// the epoch digits, the upstream a digit first and of `[A-Za-z0-9.+~-]`, the revision of
/// `[A-Za-z0-9.+~]`.
fn parts(version: &str) -> Option<Parts<'_>> {
    let (epoch, rest) = match version.split_once(':') {
        Some((epoch, rest)) => {
            let digits = !epoch.is_empty() & epoch.bytes().all(|b| b.is_ascii_digit());
            (digits.then(|| epoch.parse::<u64>().ok()).flatten()?, rest)
        }
        None => (0, version),
    };
    let (upstream, revision) = match rest.rsplit_once('-') {
        Some((upstream, revision)) if !revision.is_empty() => (upstream, revision),
        Some(_) => return None,
        None => (rest, ""),
    };
    let first = upstream.as_bytes().first().is_some_and(u8::is_ascii_digit);
    let upstream_ok = upstream
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() | b".+~-".contains(&b));
    let revision_ok = revision
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() | b".+~".contains(&b));
    (first & upstream_ok & revision_ok).then_some(Parts {
        epoch,
        upstream,
        revision,
    })
}

/// Whether `version` is a dpkg version of at most 128 bytes.
pub fn is_version(version: &str) -> bool {
    (version.len() <= 128) & parts(version).is_some()
}

/// The weight of one byte of a non-digit run: `~` before everything, the end next, then
/// letters, then every other byte.
fn weight(byte: Option<u8>) -> i32 {
    match byte {
        None => 0,
        Some(b'~') => -1,
        Some(b) if b.is_ascii_digit() => 0,
        Some(b) if b.is_ascii_alphabetic() => i32::from(b),
        Some(b) => i32::from(b) + 256,
    }
}

/// dpkg's comparison of two upstream versions or two revisions.
fn compare_part(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while a.get(i).is_some_and(|c| !c.is_ascii_digit())
            || b.get(j).is_some_and(|c| !c.is_ascii_digit())
        {
            let (x, y) = (weight(a.get(i).copied()), weight(b.get(j).copied()));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first = Ordering::Equal;
        while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
            if !(x.is_ascii_digit() && y.is_ascii_digit()) {
                break;
            }
            if first == Ordering::Equal {
                first = x.cmp(y);
            }
            i += 1;
            j += 1;
        }
        if a.get(i).is_some_and(u8::is_ascii_digit) {
            return Ordering::Greater;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            return Ordering::Less;
        }
        if first != Ordering::Equal {
            return first;
        }
    }
    Ordering::Equal
}

/// The order of two dpkg versions; `None` when either is not one.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (parts(a)?, parts(b)?);
    Some(
        a.epoch
            .cmp(&b.epoch)
            .then_with(|| compare_part(a.upstream, b.upstream))
            .then_with(|| compare_part(a.revision, b.revision)),
    )
}
