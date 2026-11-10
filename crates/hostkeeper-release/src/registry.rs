//! The rules of the package repositories the coordinator keeps and pushes to every host
//! (packages-repositories.md sections 3.1 and 3.2): what makes a [`Repository`] valid, what a
//! host's directory holds for a set of them ([`render`]), the digest the coordinator and the
//! host agree on ([`digest`]), and the fingerprints of an OpenPGP keyring ([`keys`]).
//!
//! The coordinator runs [`check`] on a repository an operator puts and [`conflict`] against the
//! others, and sends a host each entry as [`for_host`] makes it; the daemon runs [`check_set`] on
//! a pushed set and recomputes [`digest`], so a set the coordinator sends is judged by the same
//! rules on the host. Nothing here reads a file, a clock or a socket, and no collection is
//! hashed.
//!
//! A private repository (`access` [`ACCESS_HOSTS`], docs/design/hostgroups.md section 6.6) is
//! read by the coordinator alone, from its `uri` with the credential of its `credential_file`.
//! Each of its suites is a host group's. A host is sent it with its own group as the one suite
//! and no `credential_file`, and holds it as an entry whose `URIs` is its local mirror,
//! `file:<mirror>/<name>` ([`mirror_uri`]), which the daemon fills from the coordinator.

use hostkeeper_proto::hostkeeper::release::v1::Repository;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::grammar::is_repository_name;

/// The one kind of repository: an apt repository.
pub const KIND_APT: &str = "apt";
/// Where a host keeps its repository entries.
pub const REPOSITORIES_DIR: &str = "/etc/hostkeeper/repositories";
/// Where a host keeps the local mirror of each private repository, one directory per name.
pub const MIRROR_DIR: &str = "/var/lib/hostkeeper/mirror";
/// The `access` of a repository hosts read from its `uri` themselves (the empty `access` too).
pub const ACCESS_PUBLIC: &str = "public";
/// The `access` of a private repository: read by the coordinator, and by hosts through it.
pub const ACCESS_HOSTS: &str = "hosts";
/// The most bytes of a private repository's `credential_file`.
pub const MAX_CREDENTIAL_PATH: usize = 4096;
/// The most repositories of the coordinator's registry.
pub const MAX_REPOSITORIES: usize = 16;
/// The most repositories of the set one host is sent.
pub const MAX_SET: usize = 8;
/// The most suites of a repository.
pub const MAX_SUITES: usize = 8;
/// The most components of a repository.
pub const MAX_COMPONENTS: usize = 16;
/// The most fingerprints of a repository.
pub const MAX_FINGERPRINTS: usize = 8;
/// The most bytes of a repository's keyring.
pub const MAX_KEYRING: usize = 64 * 1024;
/// The most bytes of a repository's URI.
pub const MAX_URI: usize = 512;
/// The suffix of a keyring's file; the stem is the repository's name.
pub const KEYRING_SUFFIX: &str = ".gpg";
/// The suffix of an entry's file; the stem is the channel's name.
pub const SOURCES_SUFFIX: &str = ".sources";

/// The most bytes of a suite.
const MAX_SUITE: usize = 31;
/// The most bytes of a component.
const MAX_COMPONENT: usize = 64;
/// The most characters of a URI's port.
const MAX_PORT: usize = 5;

/// The name of the host entry for `name`'s `suite`: `<name>-<suite>`.
pub fn channel(name: &str, suite: &str) -> String {
    format!("{name}-{suite}")
}

/// The channels of `entry`, one per suite, in the suites' order.
pub fn channels(entry: &Repository) -> Vec<String> {
    entry
        .suites
        .iter()
        .map(|suite| channel(&entry.name, suite))
        .collect()
}

/// Whether `entry` is a private repository: its `access` is [`ACCESS_HOSTS`].
pub fn is_private(entry: &Repository) -> bool {
    entry.access == ACCESS_HOSTS
}

/// The `URIs` of a private repository `name`'s host entry: its local mirror under `mirror`.
pub fn mirror_uri(mirror: &str, name: &str) -> String {
    format!("file:{mirror}/{name}")
}

/// The name of `name`'s keyring file: `<name>.gpg`.
pub fn keyring_file(name: &str) -> String {
    format!("{name}{KEYRING_SUFFIX}")
}

/// Why a repository is not valid: the first rule it breaks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryError {
    /// The name is not 1 to 32 bytes of `[a-z0-9-]`.
    #[error("the name {0:?} is not 1 to 32 bytes of [a-z0-9-]")]
    Name(String),
    /// The kind is not `apt`.
    #[error("the kind {0:?} is not \"apt\"")]
    Kind(String),
    /// The URI is longer than [`MAX_URI`] bytes.
    #[error("the uri is {0} bytes, at most {MAX_URI}")]
    UriLength(usize),
    /// The URI is not `https://`, a host, an optional port and an optional path.
    #[error("the uri {0:?} is not https://, a host, an optional :port and an optional path")]
    Uri(String),
    /// The number of suites is not 1 to [`MAX_SUITES`].
    #[error("{0} suites, not 1 to {MAX_SUITES}")]
    SuiteCount(usize),
    /// A suite is not 1 to 31 bytes of `[a-z0-9-]`.
    #[error("the suite {0:?} is not 1 to 31 bytes of [a-z0-9-]")]
    Suite(String),
    /// A suite is listed twice.
    #[error("the suite {0:?} is listed twice")]
    SuiteTwice(String),
    /// `<name>-<suite>` is not a valid name: more than 32 bytes.
    #[error("the channel {0:?} is not 1 to 32 bytes of [a-z0-9-]")]
    Channel(String),
    /// The number of components is not 1 to [`MAX_COMPONENTS`].
    #[error("{0} components, not 1 to {MAX_COMPONENTS}")]
    ComponentCount(usize),
    /// A component is not 1 to 64 bytes of `[A-Za-z0-9._-]`.
    #[error("the component {0:?} is not 1 to 64 bytes of [A-Za-z0-9._-]")]
    Component(String),
    /// A component is listed twice.
    #[error("the component {0:?} is listed twice")]
    ComponentTwice(String),
    /// The number of fingerprints is not 1 to [`MAX_FINGERPRINTS`].
    #[error("{0} fingerprints, not 1 to {MAX_FINGERPRINTS}")]
    FingerprintCount(usize),
    /// A fingerprint is not 40 or 64 upper-case hex characters.
    #[error("the fingerprint {0:?} is not 40 or 64 upper-case hex characters")]
    Fingerprint(String),
    /// A fingerprint is listed twice.
    #[error("the fingerprint {0} is listed twice")]
    FingerprintTwice(String),
    /// The keyring is empty or longer than [`MAX_KEYRING`] bytes.
    #[error("the keyring is {0} bytes, not 1 to {MAX_KEYRING}")]
    KeyringSize(usize),
    /// The keyring is not a keyring of public keys.
    #[error("the keyring is not valid: {0}")]
    Keyring(#[from] KeyringError),
    /// A listed fingerprint is the fingerprint of no key of the keyring.
    #[error("the fingerprint {0} is no key of the keyring")]
    FingerprintAbsent(String),
    /// A key of the keyring has neither its own fingerprint nor a subkey's listed.
    #[error("the key {0} of the keyring has neither its own fingerprint nor a subkey's listed")]
    KeyUnlisted(String),
    /// The access is not `public`, `hosts` or empty.
    #[error("the access {0:?} is not \"public\" or \"hosts\"")]
    Access(String),
    /// A private repository names no credential file.
    #[error("a private repository (access \"hosts\") names no credential_file")]
    CredentialAbsent,
    /// A public repository names a credential file.
    #[error("a public repository names a credential_file: only access \"hosts\" takes one")]
    CredentialPublic,
    /// The credential file is not an absolute path of at most [`MAX_CREDENTIAL_PATH`] bytes with
    /// no control character and no empty, `.` or `..` component.
    #[error(
        "the credential_file {0:?} is not an absolute path of at most {MAX_CREDENTIAL_PATH} bytes \
         with no control character and no empty, . or .. component"
    )]
    CredentialPath(String),
    /// An entry a host was sent names a credential file: the coordinator never sends one.
    #[error("an entry a host is sent names a credential_file")]
    CredentialSent,
    /// A private entry a host was sent has other than one suite: its host group's.
    #[error("a private entry a host is sent has {0} suites, not 1")]
    PrivateSuites(usize),
}

impl EntryError {
    /// Whether the keyring is at fault: it does not parse, or it and the fingerprints disagree.
    /// The coordinator answers these `keyring_invalid` and every other `invalid_request`.
    pub fn is_keyring(&self) -> bool {
        matches!(
            self,
            EntryError::Keyring(_) | EntryError::FingerprintAbsent(_) | EntryError::KeyUnlisted(_)
        )
    }
}

/// Why a set of repositories is not valid: the first rule it breaks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetError {
    /// More than [`MAX_SET`] repositories.
    #[error("{0} repositories, at most {MAX_SET}")]
    TooMany(usize),
    /// A repository is not valid.
    #[error("repository {name:?}: {source}")]
    Entry {
        /// The repository's name.
        name: String,
        /// The rule it breaks.
        source: EntryError,
    },
    /// Two repositories have the same name.
    #[error("the name {0:?} is used by two repositories")]
    NameTwice(String),
    /// Two repositories have a channel of the same name.
    #[error("the channel {0:?} is used by two repositories")]
    ChannelTwice(String),
}

/// Why a keyring does not parse as OpenPGP public keys.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyringError {
    /// A byte that should start a packet has bit 7 clear.
    #[error("byte {0} does not start an OpenPGP packet")]
    Header(usize),
    /// A packet's length is indeterminate (old format) or partial (new format).
    #[error("the packet at byte {0} has an indeterminate or partial length")]
    Length(usize),
    /// The keyring ends inside a packet.
    #[error("the keyring ends inside the packet at byte {0}")]
    Truncated(usize),
    /// A secret key or subkey.
    #[error("the keyring holds a secret key at byte {0}")]
    Secret(usize),
    /// The first packet is not a public key.
    #[error("the first packet is not a public key")]
    NotKey,
    /// A public key packet with no body.
    #[error("the key packet at byte {0} is empty")]
    Empty(usize),
    /// A key of a version other than 4 and 6.
    #[error("the key at byte {at} has version {version}, not 4 or 6")]
    Version {
        /// Where its packet starts.
        at: usize,
        /// Its version.
        version: u8,
    },
    /// A version 4 key longer than a 16 bit length counts.
    #[error("the key at byte {0} is too long for a version 4 fingerprint")]
    Oversize(usize),
}

/// A primary key of a keyring and its subkeys, by fingerprint (upper-case hex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// The primary key's fingerprint.
    pub fingerprint: String,
    /// The fingerprints of its subkeys, in the keyring's order.
    pub subkeys: Vec<String>,
}

/// The primary keys of `keyring` (the binary OpenPGP packets `gpg --export` writes, RFC 9580
/// section 4.2) and their subkeys. Every byte must belong to a packet; the first packet must be a
/// public key; secret keys are refused; signatures, user ids and every other packet are skipped.
pub fn keys(keyring: &[u8]) -> Result<Vec<Key>, KeyringError> {
    let mut found: Vec<Key> = Vec::new();
    let mut at = 0;
    while at < keyring.len() {
        let (tag, body, next) = packet(keyring, at)?;
        match tag {
            5 | 7 => return Err(KeyringError::Secret(at)),
            6 => found.push(Key {
                fingerprint: fingerprint(body, at)?,
                subkeys: Vec::new(),
            }),
            14 => {
                let print = fingerprint(body, at)?;
                found
                    .last_mut()
                    .ok_or(KeyringError::NotKey)?
                    .subkeys
                    .push(print);
            }
            _ if found.is_empty() => return Err(KeyringError::NotKey),
            _ => {}
        }
        at = next;
    }
    if found.is_empty() {
        return Err(KeyringError::NotKey);
    }
    Ok(found)
}

/// The packet at `at`: its tag, its body and where the next starts.
fn packet(data: &[u8], at: usize) -> Result<(u8, &[u8], usize), KeyringError> {
    let head = data[at];
    if head & 0x80 == 0 {
        return Err(KeyringError::Header(at));
    }
    let take = |from: usize, n: usize| data.get(from..from + n).ok_or(KeyringError::Truncated(at));
    let be = |bytes: &[u8]| {
        bytes
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
    };
    let (tag, start, len) = if head & 0x40 == 0 {
        let width = match head & 3 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return Err(KeyringError::Length(at)),
        };
        ((head >> 2) & 0x0f, at + 1 + width, be(take(at + 1, width)?))
    } else {
        let first = take(at + 1, 1)?[0];
        let (width, len) = match first {
            0..=191 => (1, usize::from(first)),
            192..=223 => {
                let second = take(at + 2, 1)?[0];
                (
                    2,
                    ((usize::from(first) - 192) << 8) + usize::from(second) + 192,
                )
            }
            255 => (5, be(take(at + 2, 4)?)),
            _ => return Err(KeyringError::Length(at)),
        };
        (head & 0x3f, at + 1 + width, len)
    };
    let body = take(start, len)?;
    Ok((tag, body, start + len))
}

/// The fingerprint of the key packet `body` that starts at `at`, upper-case hex.
fn fingerprint(body: &[u8], at: usize) -> Result<String, KeyringError> {
    let (version, len) = match body.first() {
        None => return Err(KeyringError::Empty(at)),
        Some(version) => (*version, body.len()),
    };
    match version {
        4 => {
            let len = u16::try_from(len).map_err(|_| KeyringError::Oversize(at))?;
            let mut hash = Sha1::new();
            hash.update([0x99]);
            hash.update(len.to_be_bytes());
            hash.update(body);
            Ok(hex(&hash.finalize(), true))
        }
        6 => {
            // A packet's length is at most 32 bits (`packet`), so it fits.
            let len = u32::try_from(len).unwrap_or(u32::MAX);
            let mut hash = Sha256::new();
            hash.update([0x9b]);
            hash.update(len.to_be_bytes());
            hash.update(body);
            Ok(hex(&hash.finalize(), true))
        }
        version => Err(KeyringError::Version { at, version }),
    }
}

fn hex(bytes: &[u8], upper: bool) -> String {
    bytes
        .iter()
        .map(|b| {
            if upper {
                format!("{b:02X}")
            } else {
                format!("{b:02x}")
            }
        })
        .collect()
}

/// The SHA-256 of `keyring`, as 64 lower-case hex characters.
pub fn keyring_sha256(keyring: &[u8]) -> String {
    hex(&Sha256::digest(keyring), false)
}

/// Whether `text` is 1 to `max` bytes, every one of which `ok` accepts.
fn word(text: &str, max: usize, ok: impl Fn(u8) -> bool) -> bool {
    (1..=max).contains(&text.len()) && text.bytes().all(ok)
}

/// Whether `uri` is `https://`, a host of `[A-Za-z0-9.-]` (not starting with `.` or `-`), an
/// optional `:` and 1 to 5 digits, and an optional path of `[A-Za-z0-9._~/-]` after a `/`.
fn is_uri(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("https://") else {
        return false;
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let host_ok = !host.starts_with(['.', '-'])
        && word(host, usize::MAX, |b| {
            b.is_ascii_alphanumeric() | (b == b'.') | (b == b'-')
        });
    let port_ok = port.is_none_or(|p| word(p, MAX_PORT, |b| b.is_ascii_digit()));
    let path_ok = path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'~' | b'/' | b'-'));
    host_ok & port_ok & path_ok
}

/// Which rules an entry is judged by: the registry's, or those of an entry a host is sent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// An entry of the coordinator's registry: a private one names its credential file.
    Registry,
    /// An entry of a host's set: none names a credential file, and a private one has one suite.
    Host,
}

/// Whether `path` is an absolute path of at most [`MAX_CREDENTIAL_PATH`] bytes, with no control
/// character and no empty, `.` or `..` component.
fn is_credential_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    path.len() <= MAX_CREDENTIAL_PATH
        && !path.chars().any(char::is_control)
        && rest.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Whether `entry`'s access and credential file keep the rules of `form`.
fn check_access(entry: &Repository, form: Form) -> Result<(), EntryError> {
    let private = match entry.access.as_str() {
        "" | ACCESS_PUBLIC => false,
        ACCESS_HOSTS => true,
        other => return Err(EntryError::Access(other.to_owned())),
    };
    let named = !entry.credential_file.is_empty();
    match (form, private, named) {
        (Form::Host, _, true) => Err(EntryError::CredentialSent),
        (Form::Host, true, false) if entry.suites.len() != 1 => {
            Err(EntryError::PrivateSuites(entry.suites.len()))
        }
        (Form::Registry, true, false) => Err(EntryError::CredentialAbsent),
        (Form::Registry, false, true) => Err(EntryError::CredentialPublic),
        (Form::Registry, true, true) if !is_credential_path(&entry.credential_file) => {
            Err(EntryError::CredentialPath(entry.credential_file.clone()))
        }
        _ => Ok(()),
    }
}

/// Whether `entry` is valid as an entry of the coordinator's registry: the first rule it breaks,
/// in the order of the fields. A private repository names its credential file.
pub fn check(entry: &Repository) -> Result<(), EntryError> {
    check_form(entry, Form::Registry)
}

fn check_form(entry: &Repository, form: Form) -> Result<(), EntryError> {
    if !is_repository_name(&entry.name) {
        return Err(EntryError::Name(entry.name.clone()));
    }
    if entry.kind != KIND_APT {
        return Err(EntryError::Kind(entry.kind.clone()));
    }
    if entry.uri.len() > MAX_URI {
        return Err(EntryError::UriLength(entry.uri.len()));
    }
    if !is_uri(&entry.uri) {
        return Err(EntryError::Uri(entry.uri.clone()));
    }
    check_suites(entry)?;
    check_components(&entry.components)?;
    check_fingerprints(&entry.fingerprints)?;
    check_keyring(entry)?;
    check_access(entry, form)
}

fn check_suites(entry: &Repository) -> Result<(), EntryError> {
    if !(1..=MAX_SUITES).contains(&entry.suites.len()) {
        return Err(EntryError::SuiteCount(entry.suites.len()));
    }
    let mut seen = BTreeSet::new();
    for suite in &entry.suites {
        let ok = word(suite, MAX_SUITE, |b| {
            b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'-')
        });
        if !ok {
            return Err(EntryError::Suite(suite.clone()));
        }
        if !seen.insert(suite) {
            return Err(EntryError::SuiteTwice(suite.clone()));
        }
        let channel = channel(&entry.name, suite);
        if !is_repository_name(&channel) {
            return Err(EntryError::Channel(channel));
        }
    }
    Ok(())
}

fn check_components(components: &[String]) -> Result<(), EntryError> {
    if !(1..=MAX_COMPONENTS).contains(&components.len()) {
        return Err(EntryError::ComponentCount(components.len()));
    }
    let mut seen = BTreeSet::new();
    for component in components {
        let ok = word(component, MAX_COMPONENT, |b| {
            b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'-')
        });
        if !ok {
            return Err(EntryError::Component(component.clone()));
        }
        if !seen.insert(component) {
            return Err(EntryError::ComponentTwice(component.clone()));
        }
    }
    Ok(())
}

fn check_fingerprints(fingerprints: &[String]) -> Result<(), EntryError> {
    if !(1..=MAX_FINGERPRINTS).contains(&fingerprints.len()) {
        return Err(EntryError::FingerprintCount(fingerprints.len()));
    }
    let mut seen = BTreeSet::new();
    for print in fingerprints {
        let ok = matches!(print.len(), 40 | 64)
            && print
                .bytes()
                .all(|b| b.is_ascii_digit() | (b'A'..=b'F').contains(&b));
        if !ok {
            return Err(EntryError::Fingerprint(print.clone()));
        }
        if !seen.insert(print) {
            return Err(EntryError::FingerprintTwice(print.clone()));
        }
    }
    Ok(())
}

fn check_keyring(entry: &Repository) -> Result<(), EntryError> {
    if !(1..=MAX_KEYRING).contains(&entry.keyring.len()) {
        return Err(EntryError::KeyringSize(entry.keyring.len()));
    }
    let found = keys(&entry.keyring)?;
    let held: BTreeSet<&String> = found
        .iter()
        .flat_map(|key| std::iter::once(&key.fingerprint).chain(&key.subkeys))
        .collect();
    if let Some(absent) = entry
        .fingerprints
        .iter()
        .find(|print| !held.contains(print))
    {
        return Err(EntryError::FingerprintAbsent(absent.clone()));
    }
    let listed = |print: &String| entry.fingerprints.contains(print);
    match found
        .iter()
        .find(|key| !listed(&key.fingerprint) && !key.subkeys.iter().any(listed))
    {
        Some(key) => Err(EntryError::KeyUnlisted(key.fingerprint.clone())),
        None => Ok(()),
    }
}

/// Whether `entries` are valid as one host's set: at most [`MAX_SET`], each valid by [`check`]
/// but that none names a credential file and a private one has one suite (as [`for_host`] makes
/// it), no name twice and no channel twice.
pub fn check_set(entries: &[Repository]) -> Result<(), SetError> {
    if entries.len() > MAX_SET {
        return Err(SetError::TooMany(entries.len()));
    }
    let mut names = BTreeSet::new();
    let mut taken = BTreeSet::new();
    for entry in entries {
        check_form(entry, Form::Host).map_err(|source| SetError::Entry {
            name: entry.name.clone(),
            source,
        })?;
        if !names.insert(&entry.name) {
            return Err(SetError::NameTwice(entry.name.clone()));
        }
        for channel in channels(entry) {
            if taken.contains(&channel) {
                return Err(SetError::ChannelTwice(channel));
            }
            taken.insert(channel);
        }
    }
    Ok(())
}

/// The first channel of `entry` that is a channel of another entry of `others` (one of another
/// name), if any.
pub fn conflict(entry: &Repository, others: &[Repository]) -> Option<String> {
    let taken: BTreeSet<String> = others
        .iter()
        .filter(|other| other.name != entry.name)
        .flat_map(channels)
        .collect();
    channels(entry)
        .into_iter()
        .find(|channel| taken.contains(channel))
}

/// The entry `entry` of the registry as a host of the host group `group` is sent it: a public
/// one as it is; a private one with `group` as its one suite, and only when its suites list
/// `group` (else `None`: the host is sent nothing of it). Neither names a credential file.
pub fn for_host(entry: &Repository, group: &str) -> Option<Repository> {
    let mut sent = entry.clone();
    sent.credential_file.clear();
    if is_private(entry) {
        if !entry.suites.iter().any(|suite| suite == group) {
            return None;
        }
        sent.suites = vec![group.to_owned()];
    }
    Some(sent)
}

/// A file of a host's repositories directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFile {
    /// The file's name.
    pub name: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// The files a host holds for `entries`, sorted by name: per entry its keyring as
/// `<name>.gpg`, and per suite `<name>-<suite>.sources` naming `dir/<name>.gpg` as its
/// `Signed-By`; its `URIs` is the entry's `uri`, or for a private entry its local mirror under
/// `mirror` ([`mirror_uri`]). `entries` are taken as valid ([`check_set`]).
pub fn render(entries: &[Repository], dir: &str, mirror: &str) -> Vec<HostFile> {
    let mut files = Vec::new();
    for entry in entries {
        files.push(HostFile {
            name: keyring_file(&entry.name),
            bytes: entry.keyring.to_vec(),
        });
        let uri = if is_private(entry) {
            mirror_uri(mirror, &entry.name)
        } else {
            entry.uri.clone()
        };
        for suite in &entry.suites {
            let text = format!(
                "Types: deb\nURIs: {uri}\nSuites: {suite}\nComponents: {}\nSigned-By: {dir}/{}\n",
                entry.components.join(" "),
                keyring_file(&entry.name),
            );
            files.push(HostFile {
                name: format!("{}{SOURCES_SUFFIX}", channel(&entry.name, suite)),
                bytes: text.into_bytes(),
            });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

/// The digest of `entries` as a host holds them: the SHA-256, in lower-case hex, over the files
/// of [`render`] at [`REPOSITORIES_DIR`] and [`MIRROR_DIR`] in the order of their names, each as its name, a zero
/// byte, its length as eight big-endian bytes and its bytes.
pub fn digest(entries: &[Repository]) -> String {
    let mut hash = Sha256::new();
    for file in render(entries, REPOSITORIES_DIR, MIRROR_DIR) {
        hash.update(file.name.as_bytes());
        hash.update([0]);
        hash.update(
            u64::try_from(file.bytes.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hash.update(&file.bytes);
    }
    hex(&hash.finalize(), false)
}
