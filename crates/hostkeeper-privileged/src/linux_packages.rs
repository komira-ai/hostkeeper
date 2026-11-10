//! The parts of the Linux packages test (`tests/linux_packages.rs`) that need no root and no
//! docker: the list of releases the packages are installed on, the finding of the packages the
//! job built, the script a container runs, and the arguments of `docker run`.
//!
//! **The list.** `linux-releases.toml` beside this crate's manifest ([`RELEASES_TEXT`]) holds one
//! `[[release]]` table per release, with exactly the keys `name`, `format` (`deb` or `rpm`)
//! and `image`, which must be pinned by digest (`<repository>@sha256:<64 hex digits>`).
//! [`parse_releases`] refuses anything else: another key, a missing key, a key that is not text,
//! an unknown format, an image that is not pinned, a name listed twice, an empty list.
//!
//! **The packages.** The job builds the deb and the rpm into one directory
//! ([`PACKAGES_DIR`] of the target directory). [`find_packages`] gives the one file of each format
//! and fails by name when a format has none or more than one.
//!
//! **The container.** [`script`] is what `/bin/sh` runs in a container: where the image lacks
//! `systemctl` or `systemd-tmpfiles` (the images of Debian, Ubuntu and openSUSE carry no systemd,
//! and no container runs a service manager) it puts a stand-in that does nothing, so the
//! packages' scripts run unchanged; it installs the package with the format's own tool (its output
//! to stderr, so that stdout holds the versions alone), then runs each of [`PROGRAMS`] with
//! `--version`. [`docker_args`] is the `docker run` that runs it with no
//! network and no pull.

use std::fs;
use std::path::{Path, PathBuf};

/// The list, as checked in beside this crate's manifest.
pub const RELEASES_TEXT: &str = include_str!("../linux-releases.toml");
/// The directory of the target directory the job builds the packages into.
pub const PACKAGES_DIR: &str = "linux-packages";
/// The directory of the target directory the job stages the programs into.
pub const STAGED_DIR: &str = "nfpm";
/// Where the packages directory is mounted, read only, in a container.
pub const MOUNT: &str = "/packages";
/// Each program the packages install and the file it is staged as: run with `--version` in a
/// container and, as the staged file, on the runner, the two outputs are equal.
pub const PROGRAMS: [(&str, &str); 2] = [
    ("/usr/bin/hostkeeper", "hostkeeper"),
    ("/usr/lib/hostkeeper/gh", "gh"),
];

/// The keys of a `[[release]]` table.
const KEYS: [&str; 3] = ["name", "format", "image"];

/// Why the list cannot be read, or the packages cannot be found.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LinuxPackagesError {
    /// The list is not TOML.
    #[error("the list of releases is not TOML: {0}")]
    Unparseable(String),
    /// The list has a key it does not define.
    #[error("the list of releases has the key {0:?}, which it does not define")]
    UnknownKey(String),
    /// A release lacks a key.
    #[error("release {0} has no {1}")]
    MissingKey(usize, &'static str),
    /// A key holds something other than text, or the releases are not an array of tables.
    #[error("the list's {0} is not {1}")]
    WrongType(String, &'static str),
    /// A release names a format that is neither `deb` nor `rpm`.
    #[error("release {name:?} has the format {format:?}, not deb or rpm")]
    BadFormat { name: String, format: String },
    /// A release's image is not `<repository>@sha256:<64 lower-case hex digits>`.
    #[error("release {name:?} has the image {image:?}, which is not pinned by digest")]
    ImageNotPinned { name: String, image: String },
    /// Two releases have one name.
    #[error("the release {0:?} is listed twice")]
    Duplicate(String),
    /// The list has no release.
    #[error("the list of releases is empty")]
    Empty,
    /// The packages directory cannot be listed.
    #[error("{path}: cannot list the packages: {why}")]
    Unreadable { path: PathBuf, why: String },
    /// A format has no package in the directory, or more than one.
    #[error(
        "{dir} holds {found} files ending {suffix}, not one: build the packages as the job's steps do"
    )]
    PackageCount {
        dir: PathBuf,
        suffix: &'static str,
        found: usize,
    },
    /// A package's file name has a character a script cannot take as it is.
    #[error("the package file name {0:?} holds a character other than letters, digits and `._+-`")]
    BadFileName(String),
}

/// The format of a package: how a release installs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Format {
    Deb,
    Rpm,
}

impl Format {
    /// Every format.
    pub const ALL: [Format; 2] = [Format::Deb, Format::Rpm];

    /// The name of the format in the list.
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Deb => "deb",
            Format::Rpm => "rpm",
        }
    }

    /// The end of the package file's name.
    pub fn suffix(self) -> &'static str {
        match self {
            Format::Deb => ".deb",
            Format::Rpm => ".rpm",
        }
    }

    /// The command that installs the package at `path` with the format's own tool.
    fn install(self, path: &str) -> String {
        match self {
            Format::Deb => format!("dpkg -i {path}"),
            Format::Rpm => format!("rpm -i {path}"),
        }
    }

    fn parse(text: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.as_str() == text)
    }
}

/// One release the packages are installed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The release as people name it.
    pub name: String,
    /// The package it installs.
    pub format: Format,
    /// The container image, pinned by digest.
    pub image: String,
}

/// Whether `image` is `<repository>@sha256:<64 lower-case hex digits>`.
fn is_pinned(image: &str) -> bool {
    image
        .split_once("@sha256:")
        .is_some_and(|(repository, digest)| {
            !repository.is_empty()
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// The text of `key` in the release `table` (number `at`, from 1).
fn text_of<'a>(
    table: &'a toml::Table,
    at: usize,
    key: &'static str,
) -> Result<&'a str, LinuxPackagesError> {
    match table.get(key) {
        None => Err(LinuxPackagesError::MissingKey(at, key)),
        Some(value) => value
            .as_str()
            .ok_or_else(|| LinuxPackagesError::WrongType(format!("release {at} {key}"), "text")),
    }
}

/// Reads the list's text (the module says what it holds).
///
/// # Errors
/// The text is not TOML or is not a list of releases, as [`LinuxPackagesError`] names.
pub fn parse_releases(text: &str) -> Result<Vec<Release>, LinuxPackagesError> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| LinuxPackagesError::Unparseable(e.message().to_owned()))?;
    if let Some(key) = table.keys().find(|key| key.as_str() != "release") {
        return Err(LinuxPackagesError::UnknownKey(key.clone()));
    }
    let tables = table
        .get("release")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| LinuxPackagesError::WrongType("release".to_owned(), "an array of tables"))?;
    let mut releases: Vec<Release> = Vec::new();
    for (index, value) in tables.iter().enumerate() {
        let at = index + 1;
        let entry = value
            .as_table()
            .ok_or_else(|| LinuxPackagesError::WrongType(format!("release {at}"), "a table"))?;
        if let Some(key) = entry.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(LinuxPackagesError::UnknownKey(format!("release.{key}")));
        }
        let name = text_of(entry, at, "name")?;
        let format = text_of(entry, at, "format")?;
        let image = text_of(entry, at, "image")?;
        let format = Format::parse(format).ok_or_else(|| LinuxPackagesError::BadFormat {
            name: name.to_owned(),
            format: format.to_owned(),
        })?;
        if !is_pinned(image) {
            return Err(LinuxPackagesError::ImageNotPinned {
                name: name.to_owned(),
                image: image.to_owned(),
            });
        }
        if releases.iter().any(|r| r.name == name) {
            return Err(LinuxPackagesError::Duplicate(name.to_owned()));
        }
        releases.push(Release {
            name: name.to_owned(),
            format,
            image: image.to_owned(),
        });
    }
    if releases.is_empty() {
        return Err(LinuxPackagesError::Empty);
    }
    Ok(releases)
}

/// The packages the job built: the file name of each format, in the packages directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packages {
    /// The directory they are in.
    pub dir: PathBuf,
    deb: String,
    rpm: String,
}

impl Packages {
    /// The file name of the package of `format`.
    pub fn file_name(&self, format: Format) -> &str {
        match format {
            Format::Deb => &self.deb,
            Format::Rpm => &self.rpm,
        }
    }
}

/// The one package of each format in `dir`.
///
/// # Errors
/// `dir` cannot be listed, a format has no package in it or more than one, or a package's name
/// holds a character a script cannot take as it is.
pub fn find_packages(dir: &Path) -> Result<Packages, LinuxPackagesError> {
    let unreadable = |e: std::io::Error| LinuxPackagesError::Unreadable {
        path: dir.to_path_buf(),
        why: e.to_string(),
    };
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let one = |format: Format| -> Result<String, LinuxPackagesError> {
        let found: Vec<&String> = names
            .iter()
            .filter(|name| name.ends_with(format.suffix()))
            .collect();
        let [name] = found[..] else {
            return Err(LinuxPackagesError::PackageCount {
                dir: dir.to_path_buf(),
                suffix: format.suffix(),
                found: found.len(),
            });
        };
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        {
            return Err(LinuxPackagesError::BadFileName(name.clone()));
        }
        Ok(name.clone())
    };
    Ok(Packages {
        dir: dir.to_path_buf(),
        deb: one(Format::Deb)?,
        rpm: one(Format::Rpm)?,
    })
}

/// The script a container runs to install the package `file` of `format` (mounted at [`MOUNT`])
/// and print the version of each of [`PROGRAMS`]. It stops at the first command that fails.
pub fn script(format: Format, file: &str) -> String {
    let mut lines = vec![
        "set -eu".to_owned(),
        "for tool in systemctl systemd-tmpfiles; do command -v \"$tool\" >/dev/null 2>&1 || { printf '#!/bin/sh\\nexit 0\\n' > \"/usr/bin/$tool\"; chmod 755 \"/usr/bin/$tool\"; }; done".to_owned(),
        format!("{} >&2", format.install(&format!("{MOUNT}/{file}"))),
    ];
    lines.extend(PROGRAMS.map(|(installed, _)| format!("{installed} --version")));
    lines.join("\n")
}

/// The name of the container that runs release number `index` in the test process `pid`: unique
/// to the run, so that no two runs share one and a container left behind names its test.
pub fn container_name(pid: u32, index: usize) -> String {
    format!("hostkeeper-ci-linux-packages-{pid}-{index}")
}

/// The arguments of the `docker` that runs `release`'s container `name`: removed when it ends,
/// never pulled, no network, the packages mounted read only at [`MOUNT`], and [`script`] run by
/// `/bin/sh`.
pub fn docker_args(name: &str, release: &Release, packages: &Packages) -> Vec<String> {
    let script = script(release.format, packages.file_name(release.format));
    [
        "run",
        "--rm",
        "--pull=never",
        "--network=none",
        "--name",
        name,
        "-v",
        &format!("{}:{MOUNT}:ro", packages.dir.display()),
        &release.image,
        "/bin/sh",
        "-c",
        &script,
    ]
    .map(str::to_owned)
    .to_vec()
}

#[cfg(test)]
mod tests;
