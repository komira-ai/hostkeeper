//! The host's repository configuration (packages-repositories.md section 3.1):
//! `/etc/hostkeeper/repositories/`, one `<name>.sources` file per repository entry, a deb822
//! stanza, and the keyrings those stanzas name. The daemon reads it at start and refuses to
//! start on any defect (rule S2: configuration is files), as for a bad `stores.toml`; a missing
//! or empty directory is no repository, and such a host never announces a repository installer.
//!
//! A stanza is exactly `Types: deb`, one `URIs:` of `https://` or `file:/`, one `Suites:`, at
//! least one `Components:`, and `Signed-By:` the absolute path of a keyring in the same
//! directory. Every key that would turn off apt's checks (`Trusted`, `Allow-Insecure`,
//! `Allow-Weak`, `Allow-Downgrade-To-Insecure`, `Check-Valid-Until: no`) is refused, and so is any
//! key not named here: an entry is the trust root of every package installed from it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_release::grammar::is_repository_name;

/// Where a host keeps its repository entries.
pub const REPOSITORIES_DIR: &str = "/etc/hostkeeper/repositories";

/// The suffix of an entry's file; the stem is the entry's name.
pub const SOURCES_SUFFIX: &str = ".sources";

/// The most bytes of a `.sources` file read; a longer file is refused.
pub const MAX_SOURCES_BYTES: u64 = 16 * 1024;

/// The keys a stanza may hold, lower-cased (deb822 keys are read without regard to case).
pub const KEYS: [&str; 6] = [
    "types",
    "uris",
    "suites",
    "components",
    "signed-by",
    "check-valid-until",
];

/// The keys that turn off a check of apt's, refused whatever their value.
pub const WEAKENING: [&str; 4] = [
    "trusted",
    "allow-insecure",
    "allow-weak",
    "allow-downgrade-to-insecure",
];

/// One repository entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    /// The entry's name: its file's stem.
    pub name: String,
    /// The one URI.
    pub uri: String,
    /// The one suite.
    pub suite: String,
    /// The components, in their order.
    pub components: Vec<String>,
    /// The keyring `Signed-By` names, absolute.
    pub keyring: PathBuf,
    /// The file's text, which a run copies into its work directory.
    pub text: String,
}

/// A host's repository entries, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repositories(BTreeMap<String, Repository>);

impl Repositories {
    /// The entries of `entries`, by their names.
    pub fn new(entries: Vec<Repository>) -> Repositories {
        Repositories(entries.into_iter().map(|r| (r.name.clone(), r)).collect())
    }

    /// The entry named `name`.
    pub fn get(&self, name: &str) -> Option<&Repository> {
        self.0.get(name)
    }

    /// Whether the host has no entry.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The entries' names, in order.
    pub fn names(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

/// What is wrong with a file or the directory.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FileFault {
    #[error("is a symbolic link")]
    Link,
    #[error("is not a regular file")]
    NotRegular,
    #[error("is not a directory")]
    NotDirectory,
    #[error("is owned by uid {found}, not {expected}")]
    WrongOwner { found: u32, expected: u32 },
    #[error("has mode {found:04o}: group or others can write it")]
    Writable { found: u32 },
    #[error("is over {MAX_SOURCES_BYTES} bytes")]
    TooLarge,
    #[error("is not UTF-8")]
    NotUtf8,
    #[error("cannot be read: {0}")]
    Unreadable(String),
}

/// What is wrong with a stanza.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rule {
    #[error("holds no stanza")]
    NoStanza,
    #[error("holds {0} stanzas, not one")]
    Stanzas(usize),
    #[error("line {0} is neither a field, a continuation nor a comment")]
    Syntax(usize),
    #[error("has the key {0:?}, which turns off a check of apt's")]
    Weakening(String),
    #[error("has `Check-Valid-Until: {0}`; only `yes` is accepted")]
    ValidUntil(String),
    #[error("has the unknown key {0:?}")]
    UnknownKey(String),
    #[error("has the key {0:?} twice")]
    Duplicate(String),
    #[error("has no {0:?}")]
    Missing(&'static str),
    #[error("has Types {0:?}, not exactly \"deb\"")]
    Types(String),
    #[error("has {0} URIs, not one")]
    Uris(usize),
    #[error("has the URI {0:?}, neither https:// nor file:/")]
    Scheme(String),
    #[error("has {0} suites, not one")]
    Suites(usize),
    #[error("has no component")]
    NoComponent,
    #[error("has a Signed-By that is an inline key, not a path")]
    InlineKey,
    #[error("has the Signed-By {0:?}, not an absolute path of a keyring in its directory")]
    SignedBy(String),
}

/// Why the repository configuration was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RepositoriesError {
    #[error("{}: {fault}", path.display())]
    File { path: PathBuf, fault: FileFault },
    #[error("{}: the name {name:?} is not 1 to 32 bytes of [a-z0-9-]", path.display())]
    Name { path: PathBuf, name: String },
    #[error("{}: neither a .sources file nor a keyring one names", path.display())]
    Stray { path: PathBuf },
    #[error("{}: the keyring it names {fault}", path.display())]
    Keyring { path: PathBuf, fault: FileFault },
    #[error("{}: {rule}", path.display())]
    Stanza { path: PathBuf, rule: Rule },
}

/// The entries of `dir`, every file owned by `owner_uid`. A missing directory is no entry.
pub fn load(dir: &Path, owner_uid: u32) -> Result<Repositories, RepositoriesError> {
    let file = |path: &Path, fault| RepositoriesError::File {
        path: path.to_path_buf(),
        fault,
    };
    let meta = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Repositories::default()),
        Err(e) => return Err(file(dir, FileFault::Unreadable(e.to_string()))),
        Ok(meta) => meta,
    };
    judge(&meta, owner_uid, true).map_err(|fault| file(dir, fault))?;
    let listing = fs::read_dir(dir).map_err(|e| file(dir, FileFault::Unreadable(e.to_string())))?;
    let mut names = BTreeSet::new();
    for entry in listing {
        let entry = entry.map_err(|e| file(dir, FileFault::Unreadable(e.to_string())))?;
        names.insert(entry.file_name());
    }
    let mut entries = Vec::new();
    let mut others = BTreeSet::new();
    for name in names {
        let path = dir.join(&name);
        match name.to_str().and_then(|n| n.strip_suffix(SOURCES_SUFFIX)) {
            Some(stem) => entries.push(load_entry(dir, &path, stem, owner_uid)?),
            None => {
                others.insert(path);
            }
        }
    }
    let named: BTreeSet<&PathBuf> = entries.iter().map(|e| &e.keyring).collect();
    if let Some(stray) = others.iter().find(|path| !named.contains(path)) {
        return Err(RepositoriesError::Stray {
            path: stray.clone(),
        });
    }
    for entry in &entries {
        keyring_fault(&entry.keyring, owner_uid).map_err(|fault| RepositoriesError::Keyring {
            path: dir.join(format!("{}{SOURCES_SUFFIX}", entry.name)),
            fault,
        })?;
    }
    Ok(Repositories::new(entries))
}

/// The entry of the file `path`, named `stem`.
fn load_entry(
    dir: &Path,
    path: &Path,
    stem: &str,
    owner_uid: u32,
) -> Result<Repository, RepositoriesError> {
    if !is_repository_name(stem) {
        return Err(RepositoriesError::Name {
            path: path.to_path_buf(),
            name: stem.to_owned(),
        });
    }
    let text = read_checked(path, owner_uid).map_err(|fault| RepositoriesError::File {
        path: path.to_path_buf(),
        fault,
    })?;
    parse(stem, &text, dir).map_err(|rule| RepositoriesError::Stanza {
        path: path.to_path_buf(),
        rule,
    })
}

/// The rule of every file and the directory: what it is, its owner, and that group and others
/// cannot write it.
fn judge(meta: &fs::Metadata, owner_uid: u32, dir: bool) -> Result<(), FileFault> {
    let kind = meta.file_type();
    if kind.is_symlink() {
        return Err(FileFault::Link);
    }
    match (dir, kind.is_dir(), kind.is_file()) {
        (true, false, _) => return Err(FileFault::NotDirectory),
        (false, _, false) => return Err(FileFault::NotRegular),
        _ => {}
    }
    if meta.uid() != owner_uid {
        return Err(FileFault::WrongOwner {
            found: meta.uid(),
            expected: owner_uid,
        });
    }
    let mode = meta.mode() & 0o7777;
    match mode & 0o022 {
        0 => Ok(()),
        _ => Err(FileFault::Writable { found: mode }),
    }
}

/// The keyring at `path`, judged without reading it: apt reads it at every update.
fn keyring_fault(path: &Path, owner_uid: u32) -> Result<(), FileFault> {
    let meta = fs::symlink_metadata(path).map_err(|e| FileFault::Unreadable(e.to_string()))?;
    judge(&meta, owner_uid, false)
}

/// The text of `path`, opened without following a link and judged on the open file.
fn read_checked(path: &Path, owner_uid: u32) -> Result<String, FileFault> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => FileFault::Link,
            _ => FileFault::Unreadable(e.to_string()),
        })?;
    let meta = file
        .metadata()
        .map_err(|e| FileFault::Unreadable(e.to_string()))?;
    judge(&meta, owner_uid, false)?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCES_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| FileFault::Unreadable(e.to_string()))?;
    if bytes.len() as u64 > MAX_SOURCES_BYTES {
        return Err(FileFault::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| FileFault::NotUtf8)
}

/// The fields of each stanza of `text`, keys lower-cased, a continuation line joined to its
/// field's value with a newline.
fn stanzas(text: &str) -> Result<Vec<Vec<(String, String)>>, Rule> {
    let mut all: Vec<Vec<(String, String)>> = Vec::new();
    let mut open = false;
    for (index, line) in text.lines().enumerate() {
        if line.starts_with('#') {
            continue;
        }
        if line.trim().is_empty() {
            open = false;
            continue;
        }
        if line.starts_with([' ', '\t']) {
            let field = all
                .last_mut()
                .filter(|_| open)
                .and_then(|stanza| stanza.last_mut())
                .ok_or(Rule::Syntax(index + 1))?;
            field.1.push('\n');
            field.1.push_str(line.trim());
            continue;
        }
        let (key, value) = line.split_once(':').ok_or(Rule::Syntax(index + 1))?;
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(Rule::Syntax(index + 1));
        }
        if !open {
            all.push(Vec::new());
            open = true;
        }
        if let Some(stanza) = all.last_mut() {
            stanza.push((key.to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    Ok(all)
}

/// The entry `name` that `text` holds, its keyring in `dir`.
pub fn parse(name: &str, text: &str, dir: &Path) -> Result<Repository, Rule> {
    let mut all = stanzas(text)?;
    let fields = match all.len() {
        0 => return Err(Rule::NoStanza),
        1 => all.remove(0),
        n => return Err(Rule::Stanzas(n)),
    };
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in fields {
        if WEAKENING.contains(&key.as_str()) {
            return Err(Rule::Weakening(key));
        }
        if !KEYS.contains(&key.as_str()) {
            return Err(Rule::UnknownKey(key));
        }
        if map.contains_key(&key) {
            return Err(Rule::Duplicate(key));
        }
        map.insert(key, value);
    }
    if let Some(value) = map.get("check-valid-until").filter(|v| *v != "yes") {
        return Err(Rule::ValidUntil(value.clone()));
    }
    let field = |key: &'static str| map.get(key).ok_or(Rule::Missing(key));
    let words = |key: &'static str| field(key).map(|v| v.split_whitespace().collect::<Vec<_>>());
    let types = field("types")?;
    if types != "deb" {
        return Err(Rule::Types(types.clone()));
    }
    let uri = match words("uris")?.as_slice() {
        [one] => one.to_string(),
        many => return Err(Rule::Uris(many.len())),
    };
    let https = uri
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty());
    let file = uri
        .strip_prefix("file:/")
        .is_some_and(|rest| !rest.is_empty());
    if !(https | file) {
        return Err(Rule::Scheme(uri));
    }
    let suite = match words("suites")?.as_slice() {
        [one] => one.to_string(),
        many => return Err(Rule::Suites(many.len())),
    };
    let components: Vec<String> = words("components")?
        .into_iter()
        .map(str::to_owned)
        .collect();
    if components.is_empty() {
        return Err(Rule::NoComponent);
    }
    let signed_by = field("signed-by")?;
    if signed_by.contains('\n') || signed_by.starts_with("-----") {
        return Err(Rule::InlineKey);
    }
    let keyring = PathBuf::from(signed_by);
    let in_dir = keyring.is_absolute()
        && keyring.parent() == Some(dir)
        && keyring
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| !f.ends_with(SOURCES_SUFFIX) && !f.starts_with('.'));
    if !in_dir {
        return Err(Rule::SignedBy(signed_by.clone()));
    }
    Ok(Repository {
        name: name.to_owned(),
        uri,
        suite,
        components,
        keyring,
        text: text.to_owned(),
    })
}
