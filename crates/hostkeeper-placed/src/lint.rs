//! The load rules that belong to linking (design section 4.7, "Units, enable and boot"),
//! reported as a refusal of the manifest: class release, reason `manifest_mismatch`, as
//! `hostkeeper_config::manifest::ManifestRefusal` reports its own.
//!
//! - Every `[units] run` entry is a linked unit file, or an instance (`name@instance.service`)
//!   of a linked template (`name@.service`); an instance whose own file is linked is started
//!   from that file.
//! - A unit's command lines (`ExecStart=` and the other keys of [`EXEC_KEYS`]) name files of a
//!   package only through `<packages>/<service>/current/...`: any other path under the packages
//!   directory (a version directory, another service's) would pin the unit to a version or to
//!   another package. The command line is read as systemd reads it: the prefixes before the
//!   command (`-`, `@`, `+`, `!`, `:`) are stripped, words are split with their quotes and
//!   backslash escapes (`\xNN`, `\uNNNN`, `\UNNNNNNNN`, `\NNN`, `\n`, `\t`, `\s` and an escaped
//!   character) decoded, and
//!   each word, and the value of a `--flag=<path>` word, is judged after its `.`, `..` and empty
//!   components are resolved. A launchd plist's first `ProgramArguments` element is held to the
//!   same rule, with XML comments skipped and character references decoded.
//! - Every systemd `run` unit's file has an `[Install] WantedBy=` naming at least one unit (see
//!   [`wanted_by`]), each a plain unit name of a known type ([`is_unit_name`]). The accepted
//!   syntax is narrower than systemd's: no quotes, escapes or `%` specifiers in `WantedBy=`;
//!   such a value is refused, never guessed at. A `run` entry with no systemd suffix is a
//!   launchd label of the service whose `<label>.plist` is a file of `units/`: it has no
//!   `WantedBy=` and gets no `WantedBy=` link, because launchd loads every plist of its
//!   directory at boot, so the placed plist link is all that enables it.
//!
//! The plist reader is shared with the daemon, which finds a launchd job's sealed executable
//! with [`plist_program`] (its `Program`, else its first `ProgramArguments` element); the lint
//! itself judges the first `ProgramArguments` element ([`plist_first_argument`]).

use std::collections::BTreeMap;
use std::iter::Peekable;
use std::path::{Component, Path};
use std::str::Chars;

use hostkeeper_config::manifest::{Class, Manifest};
use hostkeeper_config::names::{PLIST_SUFFIX, UNIT_SUFFIXES, UnitName, launchd_label};

use crate::fs::Fs;
use crate::roots::Roots;

/// The keys of a systemd unit whose value is a command line.
pub const EXEC_KEYS: [&str; 7] = [
    "ExecStart",
    "ExecStartPre",
    "ExecStartPost",
    "ExecStop",
    "ExecStopPost",
    "ExecReload",
    "ExecCondition",
];
/// The characters systemd reads as prefixes of a command, before its path.
pub const COMMAND_PREFIXES: [char; 5] = ['-', '@', '+', '!', ':'];
/// The unit types systemd knows, the suffix of a unit name.
pub const UNIT_TYPES: [&str; 11] = [
    "service",
    "socket",
    "device",
    "mount",
    "automount",
    "swap",
    "target",
    "path",
    "timer",
    "slice",
    "scope",
];
/// The most bytes of a `WantedBy=` unit: its `.wants` directory (six bytes more) must fit in a
/// 255-byte path component.
pub const MAX_UNIT_NAME: usize = 249;

/// Why a manifest's units are refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LintError {
    #[error(
        "the [units] run entry {unit:?} is neither a file of units/ nor an instance of a template there"
    )]
    RunUnit { unit: String },
    #[error(
        "units/{file} {key} names {path:?}, which is under {packages}/ but not {packages}/{service}/current/"
    )]
    PackagePath {
        file: String,
        key: String,
        path: String,
        packages: String,
        service: String,
    },
    #[error(
        "the run unit {unit} has no [Install] WantedBy= in units/{file}, so the daemon cannot enable it"
    )]
    NoWantedBy { unit: String, file: String },
    #[error(
        "units/{file} has WantedBy={target:?}, which is not a unit name of a known type of at most {MAX_UNIT_NAME} bytes"
    )]
    BadTarget { file: String, target: String },
    #[error("units/{file} cannot be read as text: {why}")]
    Unreadable { file: String, why: String },
}

impl LintError {
    /// Always `Release`: the package is wrong.
    pub fn class(&self) -> Class {
        Class::Release
    }

    /// Always `manifest_mismatch`, the reason of every refusal of the package (design 5.5).
    pub fn reason(&self) -> &'static str {
        "manifest_mismatch"
    }
}

/// The lines of a systemd unit file with their continuations (a trailing `\`) joined, and with
/// comment lines (`#`, `;`) and blank lines left out.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut open = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() | line.starts_with(['#', ';']) {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(head) => {
                open.push_str(head);
                open.push(' ');
            }
            None => {
                open.push_str(line);
                lines.push(std::mem::take(&mut open));
            }
        }
    }
    lines.push(open);
    lines.retain(|line| !line.trim().is_empty());
    lines
}

/// The `(section, key, value)` of each assignment of a systemd unit file.
fn assignments(text: &str) -> Vec<(String, String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in logical_lines(text) {
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.push((
                section.clone(),
                key.trim().to_owned(),
                value.trim().to_owned(),
            ));
        }
    }
    out
}

/// The character a backslash escape stands for, the backslash already read: `\xNN` (two hex
/// digits), `\uNNNN` and `\UNNNNNNNN` (four and eight hex digits), `\NNN` (three octal
/// digits), `\n`, `\t`, `\s` (a space), and any other character for itself. A backslash at the
/// end stands for itself.
fn unescape(chars: &mut Peekable<Chars<'_>>) -> char {
    let first = chars.next().unwrap_or('\\');
    let mut digits = |radix: u32, count: usize, start: Option<char>| {
        let mut text: String = start.into_iter().collect();
        while text.len() < count {
            match chars.peek().filter(|c| c.is_digit(radix)) {
                Some(c) => text.push(*c),
                None => break,
            }
            chars.next();
        }
        u32::from_str_radix(&text, radix)
            .ok()
            .and_then(char::from_u32)
    };
    match first {
        'x' => digits(16, 2, None).unwrap_or('x'),
        'u' => digits(16, 4, None).unwrap_or('u'),
        'U' => digits(16, 8, None).unwrap_or('U'),
        '0'..='7' => digits(8, 3, Some(first)).unwrap_or(first),
        'n' => '\n',
        't' => '\t',
        's' => ' ',
        other => other,
    }
}

/// The words of a systemd command line: split at white space outside quotes (`"` or `'`), the
/// quotes removed and the backslash escapes decoded.
fn command_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', _) => {
                word.push(unescape(&mut chars));
                started = true;
            }
            (q, Some(open)) if q == open => quote = None,
            ('"' | '\'', None) => {
                quote = Some(c);
                started = true;
            }
            (space, None) if space.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            (other, _) => {
                word.push(other);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The plain components of the absolute path `path` with `.`, `..` and empty components
/// resolved as text; `None` for a path that is not absolute.
fn resolved(path: &str) -> Option<Vec<&str>> {
    let mut parts: Vec<&str> = Vec::new();
    path.starts_with('/').then_some(())?;
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    Some(parts)
}

/// Whether `path` is outside the packages directory `packages`, the packages directory itself,
/// or under `<packages>/<service>/current/`, once its components are resolved.
pub fn through_current(packages: &Path, service: &str, path: &str) -> bool {
    let Some(parts) = resolved(path) else {
        return true;
    };
    let base: Vec<&str> = packages
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    let Some(rest) = parts.strip_prefix(base.as_slice()) else {
        return true;
    };
    rest.is_empty()
        | ((rest.first() == Some(&service)) & (rest.get(1) == Some(&"current")) & (rest.len() > 2))
}

/// The first path of a command line that names a package file other than through `current`.
fn bad_command_path(packages: &Path, service: &str, command: &str) -> Option<String> {
    command_words(command.trim_start_matches(COMMAND_PREFIXES))
        .into_iter()
        .flat_map(|word| {
            let value = word.split_once('=').map(|(_, value)| value.to_owned());
            [Some(word), value]
        })
        .flatten()
        .find(|path| !through_current(packages, service, path))
}

/// `text` without its XML comments (`<!-- ... -->`); an unclosed comment runs to the end.
fn without_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("<!--") {
        out.push_str(before);
        rest = after.split_once("-->").map_or("", |(_, tail)| tail);
    }
    out.push_str(rest);
    out
}

/// `text` with its XML character and entity references decoded (`&#47;`, `&#x2f;`, `&amp;`,
/// `&lt;`, `&gt;`, `&quot;`, `&apos;`); an unknown reference is kept as it is.
fn decode_xml(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('&') {
        out.push_str(before);
        let decoded = after.split_once(';').and_then(|(name, tail)| {
            let number = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => name.strip_prefix('#').and_then(|dec| dec.parse().ok()),
            };
            let named = [
                ("amp", '&'),
                ("lt", '<'),
                ("gt", '>'),
                ("quot", '"'),
                ("apos", '\''),
            ]
            .into_iter()
            .find_map(|(entity, c)| (entity == name).then_some(c));
            number.and_then(char::from_u32).or(named).map(|c| (c, tail))
        });
        match decoded {
            Some((c, tail)) => {
                out.push(c);
                rest = tail;
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `<string>` that begins `text` (after white space), decoded; `None` when `text` does not
/// begin with one or it is not closed.
fn leading_string(text: &str) -> Option<String> {
    text.trim_start()
        .strip_prefix("<string>")?
        .split_once("</string>")
        .map(|(value, _)| decode_xml(value.trim()))
}

/// The first `<string>` of the `ProgramArguments` array of the plist `text`, whose comments are
/// already removed.
fn first_argument(text: &str) -> Option<String> {
    let (_, after) = text.split_once("<key>ProgramArguments</key>")?;
    leading_string(after.trim_start().strip_prefix("<array>")?)
}

/// The first `<string>` of the `ProgramArguments` array of a launchd plist, decoded, with its
/// XML comments skipped; `None` when it has none.
pub fn plist_first_argument(text: &str) -> Option<String> {
    first_argument(&without_comments(text))
}

/// The executable a launchd plist starts, as launchd chooses it: the `<string>` of its `Program`
/// key when it has that key, else the first `<string>` of its `ProgramArguments` array; decoded,
/// with its XML comments skipped. `None` when it has neither, or its `Program` key is not
/// followed by a string (launchd does not load such a job, so nothing is guessed in its place).
/// The keys are found as text, as the lint finds them: the first `<key>Program</key>` anywhere
/// counts, a nested dictionary's included.
pub fn plist_program(text: &str) -> Option<String> {
    let text = without_comments(text);
    match text.split_once("<key>Program</key>") {
        Some((_, after)) => leading_string(after),
        None => first_argument(&text),
    }
}

/// Checks the command lines of the unit file `file` of `service`, whose text is `text`, against
/// the packages directory `packages`: a launchd plist (`.plist`) by its first
/// `ProgramArguments` element, a systemd unit by every key of [`EXEC_KEYS`].
pub fn check_unit_text(
    packages: &Path,
    service: &str,
    file: &str,
    text: &str,
) -> Result<(), LintError> {
    let refuse = |key: &str, path: String| LintError::PackagePath {
        file: file.to_owned(),
        key: key.to_owned(),
        path,
        packages: packages.display().to_string(),
        service: service.to_owned(),
    };
    if file.ends_with(".plist") {
        let program = plist_first_argument(text).unwrap_or_default();
        return match through_current(packages, service, &program) {
            true => Ok(()),
            false => Err(refuse("ProgramArguments", program)),
        };
    }
    for (_, key, value) in assignments(text) {
        let bad = EXEC_KEYS
            .contains(&key.as_str())
            .then(|| bad_command_path(packages, service, &value))
            .flatten();
        if let Some(path) = bad {
            return Err(refuse(&format!("{key}="), path));
        }
    }
    Ok(())
}

/// The units named by the `WantedBy=` of the `[Install]` section of a systemd unit, in order;
/// an empty `WantedBy=` clears the list, as systemd reads it.
pub fn wanted_by(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for (section, key, value) in assignments(text) {
        if (section == "[Install]") & (key == "WantedBy") {
            if value.is_empty() {
                targets.clear();
            }
            targets.extend(value.split_whitespace().map(str::to_owned));
        }
    }
    targets
}

/// Whether `name` is a plain unit name: at most [`MAX_UNIT_NAME`] bytes of `A-Za-z0-9` and
/// `_ . @ -` (characters of the path grammar, which every recorded path keeps), a stem not
/// starting with `.` and a suffix of [`UNIT_TYPES`], with at most one `@`, which has a prefix
/// before it and an instance after it. Templates, specifiers (`%`), escapes and quotes are not.
pub fn is_unit_name(name: &str) -> bool {
    let Some((stem, kind)) = name.rsplit_once('.') else {
        return false;
    };
    let instance_ok = stem.split_once('@').is_none_or(|(prefix, instance)| {
        !prefix.is_empty() & !instance.is_empty() & !instance.contains('@')
    });
    (name.len() <= MAX_UNIT_NAME)
        & UNIT_TYPES.contains(&kind)
        & !stem.is_empty()
        & !stem.starts_with('.')
        & instance_ok
        & name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b"_.@-".contains(&b))
}

/// The unit file a `run` entry is started from: the entry itself when it is a file of `units/`,
/// else the template it is an instance of; either must be a file of `units/`. A template is
/// not a unit that can run, and is refused.
pub(crate) fn unit_file_of(manifest: &Manifest, unit: &str) -> Result<String, LintError> {
    let refuse = || LintError::RunUnit {
        unit: unit.to_owned(),
    };
    let name = UnitName::of(manifest.name.as_str(), unit, false).map_err(|_| refuse())?;
    let file = match (manifest.unit_files.contains(unit), name.template()) {
        (false, Some(template)) => template,
        _ => unit.to_owned(),
    };
    manifest
        .unit_files
        .contains(&file)
        .then_some(file)
        .ok_or_else(refuse)
}

/// The text of the unit file `file` in the directory `units`.
pub(crate) fn read_unit(fs: &dyn Fs, units: &Path, file: &str) -> Result<String, LintError> {
    let unreadable = |why: String| LintError::Unreadable {
        file: file.to_owned(),
        why,
    };
    let bytes = fs
        .read(&units.join(file))
        .map_err(|e| unreadable(e.to_string()))?;
    String::from_utf8(bytes).map_err(|_| unreadable("not UTF-8".to_owned()))
}

/// The text of each file of `units/` of the version directory of `manifest`.
pub(crate) fn unit_texts(
    fs: &dyn Fs,
    roots: &Roots,
    manifest: &Manifest,
) -> Result<BTreeMap<String, String>, LintError> {
    let units = roots
        .packages()
        .version(manifest.name.as_str(), manifest.version.as_str())
        .join("units");
    manifest
        .unit_files
        .iter()
        .map(|file| -> Result<(String, String), LintError> {
            Ok((file.clone(), read_unit(fs, &units, file)?))
        })
        .collect()
}

/// The plist a launchd `run` entry is loaded from: `None` for an entry with a systemd suffix;
/// for any other, `<label>.plist`, which must be a file of `units/` and whose label is the
/// service's, else [`LintError::RunUnit`].
fn plist_of(manifest: &Manifest, unit: &str) -> Result<Option<String>, LintError> {
    if UNIT_SUFFIXES.iter().any(|suffix| unit.ends_with(suffix)) {
        return Ok(None);
    }
    let refuse = || LintError::RunUnit {
        unit: unit.to_owned(),
    };
    let label = launchd_label(manifest.name.as_str(), unit).map_err(|_| refuse())?;
    let file = format!("{label}{PLIST_SUFFIX}");
    match manifest.unit_files.contains(&file) {
        true => Ok(Some(file)),
        false => Err(refuse()),
    }
}

/// The unit file of the `run` unit `unit` and its `WantedBy=` units, read from `texts` (by
/// [`unit_texts`]): for a systemd unit at least one, each a unit name; for a launchd label its
/// plist and none.
pub(crate) fn run_targets(
    manifest: &Manifest,
    texts: &BTreeMap<String, String>,
    unit: &str,
) -> Result<(String, Vec<String>), LintError> {
    if let Some(plist) = plist_of(manifest, unit)? {
        return Ok((plist, Vec::new()));
    }
    let file = unit_file_of(manifest, unit)?;
    let targets = texts
        .get(&file)
        .map(String::as_str)
        .map(wanted_by)
        .unwrap_or_default();
    if targets.is_empty() {
        return Err(LintError::NoWantedBy {
            unit: unit.to_owned(),
            file,
        });
    }
    match targets.iter().find(|target| !is_unit_name(target)) {
        Some(target) => Err(LintError::BadTarget {
            file,
            target: target.clone(),
        }),
        None => Ok((file, targets)),
    }
}

/// Applies every rule of this module to `manifest`, whose version directory is
/// `<packages>/<service>/<version>`, read through `fs`.
pub fn lint(fs: &dyn Fs, roots: &Roots, manifest: &Manifest) -> Result<(), LintError> {
    let texts = unit_texts(fs, roots, manifest)?;
    for (file, text) in &texts {
        check_unit_text(roots.packages().dir(), manifest.name.as_str(), file, text)?;
    }
    for unit in &manifest.run {
        run_targets(manifest, &texts, unit)?;
    }
    Ok(())
}
