//! The grammars of the names and paths the two files hold: service names and versions
//! (docs/design/hostkeeper-design.md, section 11.8), absolute and relative paths, and the names of
//! a package's unit files.
//!
//! A path is checked to the grammar systemd reads without surprise: it starts with `/` (so with
//! none of the prefixes `-`, `+` and `!` systemd gives a meaning), and holds no white space, no
//! `%` specifier, no `\` escape and no `.` or `..` component.

use std::fmt;

/// The most bytes of a service name.
pub const MAX_SERVICE_NAME: usize = 64;
/// The most bytes of a version.
pub const MAX_VERSION: usize = 128;
/// The most bytes of a path.
pub const MAX_PATH: usize = 1024;
/// The most bytes of the instance of a unit (`9301` in `tierstore@9301.service`).
pub const MAX_INSTANCE: usize = 64;
/// The kinds of unit a package may carry, by the suffix of the unit's name.
pub const UNIT_SUFFIXES: [&str; 3] = [".service", ".socket", ".timer"];

/// Whether `name` is a service name: 1 to [`MAX_SERVICE_NAME`] bytes of `A-Z`, `a-z`, `0-9`,
/// `_` and `-`.
pub fn is_service_name(name: &str) -> bool {
    (1..=MAX_SERVICE_NAME).contains(&name.len())
        & name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | (b == b'_') | (b == b'-'))
}

/// Whether `version` is a version: 1 to [`MAX_VERSION`] bytes of `A-Z`, `a-z`, `0-9`, `.`, `_`,
/// `+` and `-`, not beginning with `.`.
pub fn is_version(version: &str) -> bool {
    (1..=MAX_VERSION).contains(&version.len())
        & !version.starts_with('.')
        & version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b"._+-".contains(&b))
}

/// Why a name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error(
        "{0:?} is not a service name: 1 to {MAX_SERVICE_NAME} bytes of A-Z, a-z, 0-9, `_` and `-`"
    )]
    Service(String),
    #[error(
        "{0:?} is not a version: 1 to {MAX_VERSION} bytes of A-Z, a-z, 0-9, `.`, `_`, `+` and `-`, not beginning with `.`"
    )]
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
    /// instance (a template) is accepted only with `template`.
    pub fn of(service: &str, unit: &'a str, template: bool) -> Result<UnitName<'a>, UnitError> {
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
