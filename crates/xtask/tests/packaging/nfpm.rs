//! `nfpm.yaml` read as nfpm (v2.48.0) reads it: the files and the links a package carries, its
//! scripts and the names of its files, for one packager (`deb`, `rpm` or `archlinux`).

use std::collections::BTreeMap;

use crate::yaml::{self, Node, Value};

/// The keys nfpm.yaml may hold, at the top and in each table below it. A signing section
/// (`deb.signature`, `rpm.signature`), a repository, an override for one packager, a
/// dependency or a version suffix (`release`, `prerelease`, `epoch`) is none of them. The one
/// table of a packager is `archlinux`, which holds only [`ARCHLINUX_KEYS`].
pub const TOP_KEYS: [&str; 14] = [
    "name",
    "arch",
    "platform",
    "version",
    "section",
    "priority",
    "maintainer",
    "description",
    "vendor",
    "homepage",
    "license",
    "contents",
    "scripts",
    "archlinux",
];
/// The keys of the `archlinux` table: the name of the package's packager, which nfpm writes into
/// `.PKGINFO`.
pub const ARCHLINUX_KEYS: [&str; 1] = ["packager"];
/// The top-level keys whose value nfpm decodes as a string.
pub const TEXT_KEYS: [&str; 11] = [
    "name",
    "arch",
    "platform",
    "version",
    "section",
    "priority",
    "maintainer",
    "description",
    "vendor",
    "homepage",
    "license",
];
pub const SCRIPT_KEYS: [&str; 2] = ["postinstall", "preremove"];
pub const CONTENT_KEYS: [&str; 4] = ["src", "dst", "type", "file_info"];
/// The one `type` a contents entry may have: a link, whose `src` is what it points at. An
/// entry with no `type` is a file.
pub const LINK_TYPE: &str = "symlink";
pub const FILE_INFO_KEYS: [&str; 3] = ["mode", "owner", "group"];

/// One file of a package.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct File {
    pub dst: String,
    pub src: String,
    /// As written (`0755`).
    pub mode: String,
    pub owner: String,
    pub group: String,
}

/// One link of a package: nfpm's `type: symlink` entry.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Link {
    /// Where the link is.
    pub dst: String,
    /// What it points at, as written (`src`).
    pub target: String,
}

/// The entries of a mapping, keyed by their text. A key that is not text, or that comes
/// twice, is refused.
fn map<'a>(node: &'a Node, what: &str) -> Result<BTreeMap<&'a str, &'a Node>, String> {
    let Value::Map(entries) = &node.value else {
        return Err(format!("line {}: {what} is not a mapping", node.line()));
    };
    let mut out = BTreeMap::new();
    for (key, value) in entries {
        let name = key
            .as_str()
            .ok_or_else(|| format!("line {}: a key of {what} is not text", key.line()))?;
        if out.insert(name, value).is_some() {
            return Err(format!("line {}: {what} has {name:?} twice", key.line()));
        }
    }
    Ok(out)
}

fn text(node: &Node, what: &str) -> Result<String, String> {
    node.as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("line {}: {what} is not text", node.line()))
}

fn items<'a>(node: &'a Node, what: &str) -> Result<&'a [Node], String> {
    match &node.value {
        Value::Seq(items) => Ok(items),
        _ => Err(format!("line {}: {what} is not a list", node.line())),
    }
}

/// The keys of `found` that `known` does not list, each as `<at><key>`.
fn unknown(found: &BTreeMap<&str, &Node>, known: &[&str], at: &str) -> Vec<String> {
    found
        .keys()
        .filter(|key| !known.contains(key))
        .map(|key| format!("{at}{key}"))
        .collect()
}

/// The text of `key` in `entry`, or why there is none.
fn required(entry: &BTreeMap<&str, &Node>, key: &str, line: usize) -> Result<String, String> {
    match entry.get(key).copied() {
        Some(node) => text(node, key),
        None => Err(format!("line {line}: a contents entry has no {key}")),
    }
}

/// Whether a contents entry is a link (`type: symlink`); an entry of any other type is a file
/// here, and [`Config::bad_values`] reports its type.
fn is_link(entry: &BTreeMap<&str, &Node>) -> bool {
    entry.get("type").and_then(|node| node.as_str()) == Some(LINK_TYPE)
}

/// The links of a contents list.
fn links_of(node: &Node) -> Result<Vec<Link>, String> {
    let mut out = Vec::new();
    for item in items(node, "contents")? {
        let entry = map(item, "a contents entry")?;
        if is_link(&entry) {
            out.push(Link {
                dst: required(&entry, "dst", item.line())?,
                target: required(&entry, "src", item.line())?,
            });
        }
    }
    Ok(out)
}

/// The files of a contents list: its entries that are not links.
fn files_of(node: &Node) -> Result<Vec<File>, String> {
    let mut out = Vec::new();
    for item in items(node, "contents")? {
        let entry = map(item, "a contents entry")?;
        if is_link(&entry) {
            continue;
        }
        let info = match entry.get("file_info").copied() {
            Some(node) => map(node, "file_info")?,
            None => BTreeMap::new(),
        };
        let line = item.line();
        out.push(File {
            dst: required(&entry, "dst", line)?,
            src: required(&entry, "src", line)?,
            mode: required(&info, "mode", line)?,
            owner: required(&info, "owner", line)?,
            group: required(&info, "group", line)?,
        });
    }
    Ok(out)
}

/// The parsed config.
pub struct Config {
    root: Node,
}

impl Config {
    /// Parses `text`: one YAML document whose root is a mapping with text keys.
    pub fn parse(text: &str) -> Result<Config, String> {
        let root = yaml::load(text)?;
        map(&root, "the config")?;
        Ok(Config { root })
    }

    fn top(&self) -> BTreeMap<&str, &Node> {
        // `parse` checked that the root is a mapping with text keys, so this is never empty
        // for a config that has keys.
        map(&self.root, "the config").unwrap_or_default()
    }

    /// The text of the top-level `key`, if present.
    pub fn field(&self, key: &str) -> Result<Option<String>, String> {
        match self.top().get(key).copied() {
            Some(node) => text(node, key).map(Some),
            None => Ok(None),
        }
    }

    /// Every key nfpm.yaml holds that the lists above do not, as `table.key`.
    pub fn unknown_keys(&self) -> Result<Vec<String>, String> {
        let top = self.top();
        let mut out = unknown(&top, &TOP_KEYS, "");
        if let Some(scripts) = top.get("scripts").copied() {
            out.extend(unknown(&map(scripts, "scripts")?, &SCRIPT_KEYS, "scripts."));
        }
        if let Some(archlinux) = top.get("archlinux").copied() {
            out.extend(unknown(
                &map(archlinux, "archlinux")?,
                &ARCHLINUX_KEYS,
                "archlinux.",
            ));
        }
        if let Some(contents) = top.get("contents").copied() {
            for item in items(contents, "contents")? {
                let entry = map(item, "a contents entry")?;
                out.extend(unknown(&entry, &CONTENT_KEYS, "contents[]."));
                if let Some(info) = entry.get("file_info").copied() {
                    let info = map(info, "file_info")?;
                    out.extend(unknown(&info, &FILE_INFO_KEYS, "contents[].file_info."));
                }
            }
        }
        Ok(out)
    }

    /// The values nfpm would not take, or that are not this package's: a key of [`TEXT_KEYS`]
    /// whose value is a mapping, a list or null, a platform other than `linux`, and a contents
    /// entry whose `type` is not [`LINK_TYPE`] (a `config` file, which a package upgrade would
    /// not replace, among them).
    pub fn bad_values(&self) -> Vec<String> {
        let top = self.top();
        let mut out: Vec<String> = TEXT_KEYS
            .iter()
            .filter(|key| top.get(**key).is_some_and(|node| node.as_str().is_none()))
            .map(|key| format!("{key} is not text"))
            .collect();
        if let Some(archlinux) = top.get("archlinux").copied()
            && let Ok(table) = map(archlinux, "archlinux")
        {
            for key in ARCHLINUX_KEYS {
                if table.get(key).is_some_and(|node| node.as_str().is_none()) {
                    out.push(format!("archlinux.{key} is not text"));
                }
            }
        }
        if let Some(platform) = top.get("platform").copied().and_then(Node::as_str)
            && platform != "linux"
        {
            out.push(format!("platform is {platform:?}, not linux"));
        }
        let entries = top
            .get("contents")
            .and_then(|node| match &node.value {
                Value::Seq(items) => Some(items.as_slice()),
                _ => None,
            })
            .unwrap_or_default();
        for item in entries {
            let kind = map(item, "a contents entry")
                .ok()
                .and_then(|entry| entry.get("type").copied());
            if let Some(kind) = kind
                && kind.as_str() != Some(LINK_TYPE)
            {
                out.push(format!(
                    "line {}: a contents entry's type is not {LINK_TYPE}",
                    kind.line()
                ));
            }
        }
        out
    }

    /// `key` of the `overrides.<packager>` table, if any: nfpm adds an override's `contents`
    /// and `scripts` to the package of that packager only.
    fn overridden(&self, packager: &str, key: &str) -> Result<Option<&Node>, String> {
        let Some(all) = self.top().get("overrides").copied() else {
            return Ok(None);
        };
        let Some(one) = map(all, "overrides")?.get(packager).copied() else {
            return Ok(None);
        };
        Ok(map(one, "an override")?.get(key).copied())
    }

    /// The files the package of `packager` carries, sorted by destination.
    pub fn files(&self, packager: &str) -> Result<Vec<File>, String> {
        let mut out = Vec::new();
        if let Some(contents) = self.top().get("contents").copied() {
            out.extend(files_of(contents)?);
        }
        if let Some(contents) = self.overridden(packager, "contents")? {
            out.extend(files_of(contents)?);
        }
        out.sort();
        Ok(out)
    }

    /// The links the package of `packager` carries, sorted by where they are.
    pub fn links(&self, packager: &str) -> Result<Vec<Link>, String> {
        let mut out = Vec::new();
        if let Some(contents) = self.top().get("contents").copied() {
            out.extend(links_of(contents)?);
        }
        if let Some(contents) = self.overridden(packager, "contents")? {
            out.extend(links_of(contents)?);
        }
        out.sort();
        Ok(out)
    }

    /// The scripts of the package of `packager`: the name nfpm gives each (`postinstall`), to
    /// its source.
    pub fn scripts(&self, packager: &str) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        let tables = [
            self.top().get("scripts").copied(),
            self.overridden(packager, "scripts")?,
        ];
        for table in tables.into_iter().flatten() {
            for (name, node) in map(table, "scripts")? {
                out.insert(name.to_owned(), text(node, name)?);
            }
        }
        Ok(out)
    }

    /// The text of `key` of the `archlinux` table, if present.
    pub fn archlinux_field(&self, key: &str) -> Result<Option<String>, String> {
        let Some(table) = self.top().get("archlinux").copied() else {
            return Ok(None);
        };
        match map(table, "archlinux")?.get(key).copied() {
            Some(node) => text(node, key).map(Some),
            None => Ok(None),
        }
    }

    /// The text of `key` with `${VERSION}` and `${ARCH}` expanded as nfpm expands them from
    /// its environment. Any other `$` is refused: nothing else comes from the environment.
    fn expanded(&self, key: &str, version: &str, arch: &str) -> Result<String, String> {
        let raw = self
            .field(key)?
            .ok_or_else(|| format!("nfpm.yaml has no {key}"))?;
        let done = raw.replace("${VERSION}", version).replace("${ARCH}", arch);
        if done.contains('$') {
            return Err(format!(
                "{key} is {raw:?}: only ${{VERSION}} and ${{ARCH}} are expanded"
            ));
        }
        Ok(done)
    }

    /// The file name nfpm gives the package of `packager` built with `VERSION=version` and
    /// `ARCH=arch` (Go's architecture names, `amd64` and `arm64`): `ConventionalFileName` of
    /// nfpm's `deb/deb.go`, `rpm/rpm.go` and `arch/arch.go`, with no release, prerelease or version metadata
    /// (which [`TOP_KEYS`] does not allow, so the rpm's release is nfpm's default, 1).
    ///
    /// The inputs are the ones nfpm.yaml documents: `version` is `MAJOR.MINOR.PATCH` in digits
    /// (nfpm would drop a `v` prefix and move a prerelease or build suffix into the version in
    /// a form of its own, so those are refused rather than modelled), and `arch` is `amd64` or
    /// `arm64`.
    pub fn file_name(&self, packager: &str, version: &str, arch: &str) -> Result<String, String> {
        let parts: Vec<&str> = version.split('.').collect();
        let plain = parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
        if !plain {
            return Err(format!(
                "VERSION {version:?} is not MAJOR.MINOR.PATCH in digits"
            ));
        }
        if !["amd64", "arm64"].contains(&arch) {
            return Err(format!("ARCH {arch:?} is not amd64 or arm64"));
        }
        let package = self.expanded("name", version, arch)?;
        let package_version = self.expanded("version", version, arch)?;
        let package_arch = self.expanded("arch", version, arch)?;
        match packager {
            "deb" => {
                let deb_arch = match package_arch.as_str() {
                    "x86_64" => "amd64",
                    "aarch64" => "arm64",
                    other => other,
                };
                Ok(format!("{package}_{package_version}_{deb_arch}.deb"))
            }
            "rpm" => {
                let rpm_arch = match package_arch.as_str() {
                    "amd64" => "x86_64",
                    "arm64" => "aarch64",
                    other => other,
                };
                Ok(format!("{package}-{package_version}-1.{rpm_arch}.rpm"))
            }
            "archlinux" => {
                let arch_arch = match package_arch.as_str() {
                    "amd64" => "x86_64",
                    "arm64" => "aarch64",
                    other => other,
                };
                Ok(format!(
                    "{package}-{package_version}-1-{arch_arch}.pkg.tar.zst"
                ))
            }
            other => Err(format!("no packager {other:?}")),
        }
    }
}
