//! The grammars of the names and paths the two files hold: service names and versions, over one
//! character set ([`is_service_name`], [`is_version`], docs/design/hostkeeper-design.md, section
//! 11.8), absolute and relative paths, and the names of a package's unit files.
//!
//! A path is checked to the grammar systemd reads without surprise: it starts with `/` (so with
//! none of the prefixes `-`, `+` and `!` systemd gives a meaning), and holds no white space, no
//! `%` specifier, no `\` escape and no `.` or `..` component.

use std::fmt;

use crate::manifest::HostkeeperVersion;

/// The most bytes of a service name or a version: 255, Linux's `NAME_MAX`, the longest single
/// file name, since each becomes one path component (`packages/<service>/<version>/`).
pub const MAX_NAME: usize = 255;
/// The most bytes of a systemd unit name, its suffix included (systemd's `UNIT_NAME_MAX`).
pub const MAX_UNIT_NAME: usize = 255;
/// The most bytes of a path.
pub const MAX_PATH: usize = 1024;
/// The most bytes of the instance of a unit (`9301` in `tierstore@9301.service`).
pub const MAX_INSTANCE: usize = 64;
/// The kinds of unit a package may carry, by the suffix of the unit's name.
pub const UNIT_SUFFIXES: [&str; 3] = [".service", ".socket", ".timer"];

/// Whether `text` is 1 to [`MAX_NAME`] bytes of the POSIX portable filename character set
/// (POSIX.1-2017, 3.282: `A-Z a-z 0-9 . _ -`), with `+` added when `plus`, and does not begin
/// with `-` or `.`.
///
/// - It is one path component (`packages/<service>/<version>/`): no `/`, at most `NAME_MAX`
///   bytes, and no leading `.`, so never `.`, `..` or a hidden file.
/// - It is literal in bash without quoting: no white space, quote, `$`, `~`, `*`, `?` or other
///   character the shell gives a meaning, and no leading `-`, which a command reads as an option.
///
/// A name that keeps the rule can still be too long for a name built from it
/// (`.<version>.seal.tmp` beside the version, `<service>.service`): each such name is checked where it is built, and a
/// name too long for it is refused there with an error of its own ([`UnitError::TooLong`] here).
fn is_portable(text: &str, plus: bool) -> bool {
    let allowed = |b: u8| b.is_ascii_alphanumeric() | b"_.-".contains(&b) | (plus & (b == b'+'));
    let first = text
        .bytes()
        .next()
        .is_some_and(|b| allowed(b) & !b"-.".contains(&b));
    first & (text.len() <= MAX_NAME) & text.bytes().all(allowed)
}

/// Whether `name` is a service name (design section 11.8): `^[A-Za-z0-9_][A-Za-z0-9._-]{0,254}$`,
/// the POSIX portable filename characters, 1 to [`MAX_NAME`] bytes, not beginning with `-` or `.`.
///
/// The one difference from [`is_version`]: a service name has no `+`, because it also names the
/// service's systemd units (`<service>.service`, `<service>@<instance>.service`) and systemd
/// refuses `+` in a unit name. A version never appears in a unit name.
pub fn is_service_name(name: &str) -> bool {
    is_portable(name, false)
}

/// Whether `version` is a version (design section 11.8): `^[A-Za-z0-9_+][A-Za-z0-9._+-]{0,254}$`,
/// the characters of [`is_service_name`] plus `+`, for semver build metadata (`1.4.0+g1a2b3c`).
pub fn is_version(version: &str) -> bool {
    is_portable(version, true)
}

/// What a string that is not a service name breaks, as [`NameError::Service`] says it.
pub const SERVICE_NAME_RULE: &str = "is not a service name: 1 to 255 bytes of A-Z, a-z, 0-9, `.`, `_` and `-`, not beginning with `-` or `.`";
/// What a string that is not a version breaks, as [`NameError::Version`] says it.
pub const VERSION_RULE: &str = "is not a version: 1 to 255 bytes of A-Z, a-z, 0-9, `.`, `_`, `+` and `-`, not beginning with `-` or `.`";

/// Why a name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("{0:?} {SERVICE_NAME_RULE}")]
    Service(String),
    #[error("{0:?} {VERSION_RULE}")]
    Version(String),
}

/// A service name that keeps [`is_service_name`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceName(String);

impl ServiceName {
    pub fn new(name: &str) -> Result<ServiceName, NameError> {
        is_service_name(name)
            .then(|| ServiceName(name.to_owned()))
            .ok_or_else(|| NameError::Service(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A version that keeps [`is_version`]. Versions are compared only for equality here; their order
/// is the coordinator's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version(String);

impl Version {
    pub fn new(version: &str) -> Result<Version, NameError> {
        is_version(version)
            .then(|| Version(version.to_owned()))
            .ok_or_else(|| NameError::Version(version.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<HostkeeperVersion> for Version {
    /// A version of hostkeeper as a version: `major.minor.patch` is digits and dots, at most 62
    /// bytes, which [`is_version`] keeps.
    fn from(version: HostkeeperVersion) -> Version {
        Version(version.to_string())
    }
}

/// Why a path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("starts with {0:?}, a systemd prefix")]
    SystemdPrefix(char),
    #[error("is not absolute")]
    NotAbsolute,
    #[error("is longer than {MAX_PATH} bytes")]
    TooLong,
    #[error("holds {0:?}, which is outside the characters a path may use")]
    BadChar(char),
    #[error("has an empty component (a doubled or a trailing slash)")]
    EmptyComponent,
    #[error("has a `.` or `..` component")]
    DotComponent,
    #[error("is empty")]
    Empty,
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@')
}

/// Checks the grammar of an absolute path: it starts with `/` (so with none of the systemd
/// prefixes `-`, `+` and `!`), is at most [`MAX_PATH`] bytes, holds only letters, digits and
/// `/ . _ - + @`, and has no empty, `.` or `..` component. `/` alone is a path.
pub fn check_path(path: &str) -> Result<(), PathError> {
    if let Some(prefix) = path.chars().next().filter(|c| matches!(c, '-' | '+' | '!')) {
        return Err(PathError::SystemdPrefix(prefix));
    }
    if !path.starts_with('/') {
        return Err(PathError::NotAbsolute);
    }
    if path.len() > MAX_PATH {
        return Err(PathError::TooLong);
    }
    if let Some(c) = path.chars().find(|c| !is_path_char(*c)) {
        return Err(PathError::BadChar(c));
    }
    let components = path.strip_prefix('/').unwrap_or_default();
    // `/` alone has one empty component, which is the root and not a mistake.
    if components.is_empty() {
        return Ok(());
    }
    for component in components.split('/') {
        match component {
            "" => return Err(PathError::EmptyComponent),
            "." | ".." => return Err(PathError::DotComponent),
            _ => {}
        }
    }
    Ok(())
}

/// Checks the grammar of a path within a package: the same characters and components as an
/// absolute path, written without the first `/`.
pub fn check_relative(path: &str) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    check_path(&format!("/{path}"))
}

/// Whether the absolute path `path` is `dir` or lies below it, compared on whole components:
/// `/usr/bin` is below `/usr`, `/usr2/x` is not. Both are in the path grammar.
pub fn is_within(path: &str, dir: &str) -> bool {
    (path == dir)
        | path
            .strip_prefix(dir.trim_end_matches('/'))
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Why a unit name was refused for a service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnitError {
    #[error("is not a unit of a kind a package may carry (`.service`, `.socket`, `.timer`)")]
    NotAUnit,
    #[error("does not begin with the service name, alone or followed by `-`")]
    NotTheServices,
    #[error("has an instance outside `A-Za-z0-9_.-`, 1 to {MAX_INSTANCE} bytes")]
    BadInstance,
    #[error("is longer than {MAX_UNIT_NAME} bytes, the most systemd takes for a unit name")]
    TooLong,
}

/// A unit name taken apart: `tierstore@9301.service` is the base `tierstore`, the instance
/// `9301` and the suffix `.service`; a template (`tierstore@.service`) has an empty instance and
/// a plain unit none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitName<'a> {
    pub base: &'a str,
    pub instance: Option<&'a str>,
    pub suffix: &'static str,
}

impl<'a> UnitName<'a> {
    /// Takes `unit` apart and checks that it is `service`'s: the base is the service name, or the
    /// service name followed by `-` and more (`tierstore-cache.service` for `tierstore`), and an
    /// instance, when there is one, is 1 to [`MAX_INSTANCE`] bytes of `A-Za-z0-9_.-`. An empty
    /// instance (a template) is accepted only with `template`. A unit name of more than
    /// [`MAX_UNIT_NAME`] bytes is [`UnitError::TooLong`].
    pub fn of(service: &str, unit: &'a str, template: bool) -> Result<UnitName<'a>, UnitError> {
        if unit.len() > MAX_UNIT_NAME {
            return Err(UnitError::TooLong);
        }
        let (stem, suffix) = UNIT_SUFFIXES
            .iter()
            .find_map(|suffix| unit.strip_suffix(suffix).map(|stem| (stem, *suffix)))
            .ok_or(UnitError::NotAUnit)?;
        let (base, instance) = match stem.split_once('@') {
            Some((base, instance)) => (base, Some(instance)),
            None => (stem, None),
        };
        let own = (base == service)
            | base
                .strip_prefix(service)
                .is_some_and(|rest| rest.starts_with('-') & is_service_name(base));
        if !own {
            return Err(UnitError::NotTheServices);
        }
        let instance_ok = instance.is_none_or(|i| {
            (i.is_empty() & template)
                | ((1..=MAX_INSTANCE).contains(&i.len())
                    & i.bytes()
                        .all(|b| b.is_ascii_alphanumeric() | b"_.-".contains(&b)))
        });
        if !instance_ok {
            return Err(UnitError::BadInstance);
        }
        Ok(UnitName {
            base,
            instance,
            suffix,
        })
    }

    /// Whether this is a template: an `@` with nothing after it.
    pub fn is_template(&self) -> bool {
        self.instance == Some("")
    }

    /// The template this is an instance of (`tierstore@.service` for `tierstore@9301.service`),
    /// or `None` for a plain unit and for a template.
    pub fn template(&self) -> Option<String> {
        self.instance
            .filter(|instance| !instance.is_empty())
            .map(|_| format!("{}@{}", self.base, self.suffix))
    }

    /// The instance as a port: digits only, 1 to 65535.
    pub fn port(&self) -> Option<u16> {
        self.instance
            .filter(|instance| instance.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|instance| instance.parse::<u16>().ok())
            .filter(|port| *port != 0)
    }
}
