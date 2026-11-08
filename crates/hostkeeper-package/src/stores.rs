//! The stores of a host: `/etc/hostkeeper/stores.toml` (docs/design/packages.md, sections 7.2 and
//! 7.3), read and checked once, at the daemon's start.
//!
//! ```toml
//! [[store]]
//! name = "github"          # 1 to 32 bytes of [a-z0-9-]; the name a location uses
//! kind = "https"           # https or oci
//! base = "https://github.com/"
//! redirects = 3            # https only, 0 to 5, default 0
//!
//! [[store]]
//! name = "lan"
//! kind = "oci"
//! base = "https://mirror.example/"
//! mirror = true            # oci only: tried first for every digest
//! repository = "releases"  # an oci mirror's repository
//! basic_auth_file = "/etc/hostkeeper/stores/lan.basic"   # one line, user:secret
//! # or bearer_file = "/etc/hostkeeper/stores/lan.token"  # one line, the token
//! ```
//!
//! A host with no file has the one store [`CHANNEL_STORE`]: kind `https`, the daemon config's
//! release channel as its base, no redirect, anonymous ([`Stores::channel`]). A file that is
//! there defines every store of the host, and any defect refuses it ([`StoresError`] names the
//! store and the rule): the daemon does not start on a half-read file.
//!
//! | what the file holds | refused as |
//! |---|---|
//! | the file is a link or not a regular file, is not owned by the expected user, is writable by group or others, is over 64 KiB, or cannot be read | [`StoresError::File`] |
//! | text that is not TOML, an unknown key, a wrong type | [`StoresError::Syntax`] |
//! | a name outside 1 to 32 bytes of `[a-z0-9-]`, or a name used twice | [`Rule::Name`], [`Rule::DuplicateName`] |
//! | a kind other than `https` and `oci` | [`Rule::Kind`] |
//! | a base that is not `https://`, has a user name, a query or a fragment, or does not end in `/` | [`Rule::Base`] |
//! | `redirects` on an `oci` store (it follows a fixed 3), or over 5 | [`Rule::RedirectsOnOci`], [`Rule::RedirectsOver`] |
//! | `mirror` or `repository` on an `https` store (only an `oci` store addresses a blob by digest alone) | [`Rule::MirrorOnHttps`], [`Rule::RepositoryOnHttps`] |
//! | an `oci` mirror with no repository, or a repository that is not a locator | [`Rule::MirrorWithoutRepository`], [`Rule::Repository`] |
//! | both credential keys | [`Rule::TwoCredentials`] |
//! | a credential file that is not an absolute path, is missing, is a link or not a regular file, is not owned by the expected user, is not mode 0600, is empty or is not one line | [`Rule::Credential`] |
//!
//! A credential is sent to its store's base origin only ([`Store::origin`]); the fetcher drops it
//! on a redirect to another origin. It is held in a value whose `Debug` shows nothing of it, and
//! no error here holds it.
//!
//! The URL of a blob: an `https` store gives `<base><locator>`; an `oci` store gives
//! `<base>v2/<locator>/blobs/sha256:<hex>`, the locator being a repository name; an `oci`
//! mirror gives `<base>v2/<repository>/blobs/sha256:<hex>`.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::daemon::HttpsUrl;
use hostkeeper_release::grammar::{check_locator, is_store_name};
use serde::Deserialize;

use crate::ports::{Url, origin_of};

/// The name of the store a host without a `stores.toml` has.
pub const CHANNEL_STORE: &str = "channel";
/// The most bytes of `stores.toml`.
pub const MAX_STORES_BYTES: u64 = 64 * 1024;
/// The most bytes of a credential file.
pub const MAX_CREDENTIAL_BYTES: u64 = 4 * 1024;
/// The most redirects an `https` store may be configured to follow.
pub const MAX_REDIRECTS: u32 = 5;
/// The redirects an `oci` store follows: a registry answers a blob GET with a redirect to its
/// blob storage.
pub const OCI_REDIRECTS: u32 = 3;

/// How a store addresses a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `<base><locator>`.
    Https,
    /// `<base>v2/<repository>/blobs/sha256:<hex>`.
    Oci,
}

/// A credential's `Authorization` header value. Its `Debug` shows nothing of it.
#[derive(Clone, PartialEq, Eq)]
struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// One store of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    name: String,
    kind: Kind,
    base: String,
    origin: String,
    redirects: u32,
    mirror: bool,
    repository: Option<String>,
    authorization: Option<Secret>,
}

impl Store {
    /// The name a location uses.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The base URL, which ends in `/`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The base's origin (`https://<host>:<port>`, see [`origin_of`]): the only one the
    /// credential is sent to and a token challenge is answered for.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// How many redirects a GET follows: the configured count of an `https` store, a fixed
    /// [`OCI_REDIRECTS`] for an `oci` store.
    pub fn redirects(&self) -> u32 {
        self.redirects
    }

    /// Whether the store is tried first for every digest, listed in an order or not.
    pub fn is_mirror(&self) -> bool {
        self.mirror
    }

    /// An `oci` mirror's repository.
    pub fn repository(&self) -> Option<&str> {
        self.repository.as_deref()
    }

    /// The value of the `Authorization` header this store's requests carry, if it has a
    /// credential: `Basic <base64>` or `Bearer <token>`. It is the secret itself: give it to
    /// the fetcher and to nothing that logs.
    pub fn authorization(&self) -> Option<&str> {
        self.authorization.as_ref().map(|secret| secret.0.as_str())
    }

    /// The URL of the blob whose SHA-256 is `hex` at `locator` in this store. The locator has
    /// passed `check_locator`; for an `oci` store it is a repository name.
    pub fn blob_url(&self, locator: &str, hex: &str) -> Url {
        Url::new(match self.kind {
            Kind::Https => format!("{}{locator}", self.base),
            Kind::Oci => format!("{}v2/{locator}/blobs/sha256:{hex}", self.base),
        })
    }

    /// The URL of the blob whose SHA-256 is `hex` when this store is asked for every digest:
    /// `None` unless it is a mirror (which is `oci` and has a repository).
    pub fn mirror_url(&self, hex: &str) -> Option<Url> {
        let repository = self.repository.as_deref().filter(|_| self.mirror)?;
        Some(self.blob_url(repository, hex))
    }
}

/// The stores of a host, in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stores {
    stores: Vec<Store>,
}

impl Stores {
    /// The stores of a host with no `stores.toml`: [`CHANNEL_STORE`], of kind `https`, whose
    /// base is `release_channel` (with a `/` added if it has none), with no redirect, no mirror
    /// and no credential.
    pub fn channel(release_channel: &HttpsUrl) -> Stores {
        let mut base = release_channel.as_str().to_owned();
        if !base.ends_with('/') {
            base.push('/');
        }
        let origin = origin_of(&base).unwrap_or_default();
        Stores {
            stores: vec![Store {
                name: CHANNEL_STORE.to_owned(),
                kind: Kind::Https,
                base,
                origin,
                redirects: 0,
                mirror: false,
                repository: None,
                authorization: None,
            }],
        }
    }

    /// Every store, in file order.
    pub fn all(&self) -> &[Store] {
        &self.stores
    }

    /// The store called `name`.
    pub fn get(&self, name: &str) -> Option<&Store> {
        self.stores.iter().find(|store| store.name == name)
    }

    /// The mirrors, in file order.
    pub fn mirrors(&self) -> impl Iterator<Item = &Store> {
        self.stores.iter().filter(|store| store.mirror)
    }
}

/// Why a file is not accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FileFault {
    #[error("is not an absolute path")]
    NotAbsolute,
    #[error("does not exist")]
    Missing,
    #[error("is a link")]
    Link,
    #[error("is not a regular file")]
    NotRegular,
    #[error("is owned by user {found}, not {expected}")]
    WrongOwner { found: u32, expected: u32 },
    #[error("has mode {found:04o}, not 0600")]
    NotPrivate { found: u32 },
    #[error("has mode {found:04o}, which group or others can write")]
    WritableByOthers { found: u32 },
    #[error("is over {0} bytes")]
    TooLarge(u64),
    #[error("is empty")]
    Empty,
    #[error("is not one line of text")]
    NotOneLine,
    #[error("cannot be read: {0}")]
    Unreadable(String),
}

/// The rule a store breaks.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rule {
    #[error("its name is not 1 to 32 bytes of a-z, 0-9 and -")]
    Name,
    #[error("another store has this name")]
    DuplicateName,
    #[error("its kind {0:?} is not https or oci")]
    Kind(String),
    #[error(
        "its base is not an https:// URL without a user name, a query or a fragment, ending in /"
    )]
    Base,
    #[error("`redirects` is for an https store; an oci store follows {OCI_REDIRECTS}")]
    RedirectsOnOci,
    #[error("`redirects` {0} is over {MAX_REDIRECTS}")]
    RedirectsOver(u32),
    #[error("`mirror` is for an oci store: an https store has no blob address of its own")]
    MirrorOnHttps,
    #[error("`repository` is for an oci mirror")]
    RepositoryOnHttps,
    #[error("an oci mirror needs a `repository`")]
    MirrorWithoutRepository,
    #[error("its `repository` is not a locator")]
    Repository,
    #[error("it has both `basic_auth_file` and `bearer_file`")]
    TwoCredentials,
    #[error("its credential file {}: {fault}", path.display())]
    Credential { path: PathBuf, fault: FileFault },
}

/// Why `stores.toml` is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoresError {
    #[error("{}: the file {fault}", path.display())]
    File { path: PathBuf, fault: FileFault },
    #[error("stores.toml is not a valid stores file: {0}")]
    Syntax(String),
    #[error("store {store:?}: {rule}")]
    Store { store: String, rule: Rule },
}

/// What a store of the file may say.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileText {
    #[serde(default)]
    store: Vec<StoreText>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreText {
    name: String,
    kind: String,
    base: String,
    redirects: Option<u32>,
    mirror: Option<bool>,
    repository: Option<String>,
    basic_auth_file: Option<PathBuf>,
    bearer_file: Option<PathBuf>,
}

/// The mode a file must have.
#[derive(Clone, Copy)]
enum Mode {
    /// Exactly 0600, a credential.
    Private,
    /// Not writable by group or others, the stores file.
    Guarded,
}

/// The bytes of the regular file at `path`, which is not a link, is owned by `owner`, has the
/// `mode`, and is at most `cap` bytes. It is opened without following a link or waiting on a
/// pipe, and checked on the descriptor it was opened as.
fn read_checked(path: &Path, owner: u32, mode: Mode, cap: u64) -> Result<Vec<u8>, FileFault> {
    let unreadable = |e: io::Error| FileFault::Unreadable(e.to_string());
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match (e.kind(), e.raw_os_error()) {
            (io::ErrorKind::NotFound, _) => FileFault::Missing,
            (_, Some(libc::ELOOP)) => FileFault::Link,
            _ => unreadable(e),
        })?;
    let meta = file.metadata().map_err(unreadable)?;
    let found = meta.mode() & 0o7777;
    if !meta.is_file() {
        return Err(FileFault::NotRegular);
    }
    if meta.uid() != owner {
        return Err(FileFault::WrongOwner {
            found: meta.uid(),
            expected: owner,
        });
    }
    match mode {
        Mode::Private if found != 0o600 => return Err(FileFault::NotPrivate { found }),
        Mode::Guarded if found & 0o022 != 0 => {
            return Err(FileFault::WritableByOthers { found });
        }
        Mode::Private | Mode::Guarded => {}
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if u64::try_from(bytes.len()).is_ok_and(|n| n > cap) {
        return Err(FileFault::TooLarge(cap));
    }
    Ok(bytes)
}

/// The one line of a credential file, without its final newline.
fn credential_line(path: &Path, owner: u32) -> Result<String, FileFault> {
    if !path.is_absolute() {
        return Err(FileFault::NotAbsolute);
    }
    let bytes = read_checked(path, owner, Mode::Private, MAX_CREDENTIAL_BYTES)?;
    let text = String::from_utf8(bytes).map_err(|_| FileFault::NotOneLine)?;
    let line = text.strip_suffix('\n').unwrap_or(&text);
    if line.is_empty() {
        return Err(FileFault::Empty);
    }
    if line.chars().any(char::is_control) {
        return Err(FileFault::NotOneLine);
    }
    Ok(line.to_owned())
}

/// Whether `base` is an `https://` URL with no user name, query or fragment, ending in `/`.
fn base_is_valid(base: &str) -> bool {
    let plain = !base.contains(['?', '#', '@']);
    plain & base.ends_with('/') & origin_of(base).is_some()
}

/// The credential of `text`, as the `Authorization` header value.
fn credential(text: &StoreText, owner: u32) -> Result<Option<Secret>, Rule> {
    let read = |path: &PathBuf| {
        credential_line(path, owner).map_err(|fault| Rule::Credential {
            path: path.clone(),
            fault,
        })
    };
    match (&text.basic_auth_file, &text.bearer_file) {
        (Some(_), Some(_)) => Err(Rule::TwoCredentials),
        (Some(path), None) => Ok(Some(Secret(format!(
            "Basic {}",
            STANDARD.encode(read(path)?)
        )))),
        (None, Some(path)) => Ok(Some(Secret(format!("Bearer {}", read(path)?)))),
        (None, None) => Ok(None),
    }
}

/// The store `text` says, or the rule it breaks. Rules are checked in the order of the table of
/// the module.
fn store_of(text: StoreText, owner: u32) -> Result<Store, Rule> {
    let kind = match text.kind.as_str() {
        "https" => Kind::Https,
        "oci" => Kind::Oci,
        other => return Err(Rule::Kind(other.to_owned())),
    };
    if !base_is_valid(&text.base) {
        return Err(Rule::Base);
    }
    let mirror = text.mirror.unwrap_or(false);
    let redirects = match (kind, text.redirects) {
        (Kind::Oci, Some(_)) => return Err(Rule::RedirectsOnOci),
        (Kind::Oci, None) => OCI_REDIRECTS,
        (Kind::Https, Some(count)) if count > MAX_REDIRECTS => {
            return Err(Rule::RedirectsOver(count));
        }
        (Kind::Https, count) => count.unwrap_or(0),
    };
    match (kind, mirror, &text.repository) {
        (Kind::Https, true, _) => return Err(Rule::MirrorOnHttps),
        (Kind::Https, false, Some(_)) => return Err(Rule::RepositoryOnHttps),
        (Kind::Oci, true, None) => return Err(Rule::MirrorWithoutRepository),
        (Kind::Oci, _, Some(repository)) if check_locator(repository).is_err() => {
            return Err(Rule::Repository);
        }
        _ => {}
    }
    let authorization = credential(&text, owner)?;
    let origin = origin_of(&text.base).unwrap_or_default();
    Ok(Store {
        name: text.name,
        kind,
        base: text.base,
        origin,
        redirects,
        mirror,
        repository: text.repository,
        authorization,
    })
}

/// The stores `text` defines.
fn parse(text: &[u8], owner: u32) -> Result<Stores, StoresError> {
    let text =
        std::str::from_utf8(text).map_err(|_| StoresError::Syntax("not UTF-8 text".to_owned()))?;
    let file: FileText =
        toml::from_str(text).map_err(|e| StoresError::Syntax(e.message().to_owned()))?;
    let mut stores: Vec<Store> = Vec::new();
    for text in file.store {
        let name = text.name.clone();
        let refuse = |rule| StoresError::Store {
            store: name.clone(),
            rule,
        };
        if !is_store_name(&name) {
            return Err(refuse(Rule::Name));
        }
        if stores.iter().any(|store| store.name == name) {
            return Err(refuse(Rule::DuplicateName));
        }
        stores.push(store_of(text, owner).map_err(refuse)?);
    }
    Ok(Stores { stores })
}

/// The stores of the host from the file at `path`: [`Stores::channel`] when there is no file,
/// and otherwise the stores the file defines, or the first defect in it. The file must be a
/// regular file (not a link) owned by `owner_uid`, not writable by group or others, and at most
/// [`MAX_STORES_BYTES`]; each credential file must be a regular file (not a link) owned by
/// `owner_uid` with mode 0600. The daemon passes `0`.
pub fn load(
    path: &Path,
    release_channel: &HttpsUrl,
    owner_uid: u32,
) -> Result<Stores, StoresError> {
    match read_checked(path, owner_uid, Mode::Guarded, MAX_STORES_BYTES) {
        Ok(bytes) => parse(&bytes, owner_uid),
        Err(FileFault::Missing) => Ok(Stores::channel(release_channel)),
        Err(fault) => Err(StoresError::File {
            path: path.to_owned(),
            fault,
        }),
    }
}

#[cfg(test)]
#[path = "stores_tests.rs"]
mod tests;
