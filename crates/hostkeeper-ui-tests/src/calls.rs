//! The operator API methods the web app calls: where its sources name them, the list in
//! `ui/calls.json`, and the check against the protobuf descriptors.
//!
//! A page calls a method as `api.call("hostkeeper.operator.v1.<Service>/<Method>", body)`, the
//! name a double-quoted string literal. [`call_sites`] reads those literals from a module's text
//! and refuses a call whose first argument is anything else, and a method-name literal that is
//! not the first argument of a call (a name kept in a variable or built from pieces), so that no
//! call the app makes is hidden from the list.

use std::collections::BTreeSet;
use std::fmt;

use prost_types::FileDescriptorSet;
use serde::Deserialize;

/// Where a module names a method: the module's path under `ui/`, the 1-based line and the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// The module's path under `ui/`.
    pub path: String,
    /// The line of the method's name.
    pub line: usize,
    /// The method's full name, `<package>.<Service>/<Method>`.
    pub method: String,
}

impl CallSite {
    /// The page whose module calls the method: `fleet` for `pages/fleet.js`; `None` outside
    /// `pages/`, where no call belongs.
    pub fn page(&self) -> Option<&str> {
        self.path.strip_prefix("pages/")?.strip_suffix(".js")
    }
}

/// Why a module's calls cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    /// A call whose first argument is not a double-quoted string literal.
    #[error("{path}:{line}: a call names its method by something other than a string literal")]
    NotALiteral { path: String, line: usize },
    /// A method-name literal that is not the first argument of a call.
    #[error("{path}:{line}: a method name outside a call")]
    NameOutsideCall { path: String, line: usize },
}

/// The prefix of every method name the app may call, as it opens a string literal.
const NAME_OPENING: &str = "\"hostkeeper.";

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// The double-quoted literal that opens `rest`, if it does: its contents, with no escape and no
/// line break in them.
fn literal(rest: &str) -> Option<&str> {
    let body = rest.strip_prefix('"')?;
    let end = body.find('"')?;
    let contents = &body[..end];
    (!contents.contains(['\\', '\n'])).then_some(contents)
}

/// Every method a module calls, in the order written. `path` is the module's path under `ui/`,
/// for the sites and the errors. A call is the word `call` (not part of a longer name) followed
/// by `(`; its first argument must be a string literal, and every literal opening with
/// `"hostkeeper.` must be such an argument.
pub fn call_sites(path: &str, text: &str) -> Result<Vec<CallSite>, ScanError> {
    let mut sites = Vec::new();
    let mut names_at = BTreeSet::new();
    for (at, _) in text.match_indices("call") {
        let before = text[..at].chars().next_back();
        if before.is_some_and(is_ident) {
            continue;
        }
        let Some(args) = text[at + 4..].trim_start().strip_prefix('(') else {
            continue;
        };
        let args = args.trim_start();
        let name_at = text.len() - args.len();
        let line = line_of(text, name_at);
        let Some(method) = literal(args) else {
            return Err(ScanError::NotALiteral {
                path: path.to_owned(),
                line,
            });
        };
        names_at.insert(name_at);
        sites.push(CallSite {
            path: path.to_owned(),
            line,
            method: method.to_owned(),
        });
    }
    if let Some((at, _)) = text
        .match_indices(NAME_OPENING)
        .find(|(at, _)| !names_at.contains(at))
    {
        return Err(ScanError::NameOutsideCall {
            path: path.to_owned(),
            line: line_of(text, at),
        });
    }
    Ok(sites)
}

/// One entry of `ui/calls.json`: a method and the pages that call it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listed {
    /// The method's full name, `<package>.<Service>/<Method>`.
    pub method: String,
    /// The pages that call it, by module name (`fleet`).
    pub pages: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallsFile {
    calls: Vec<Listed>,
}

/// Why `ui/calls.json` cannot be read: it is not JSON, or not the shape [`listed_calls`] reads.
#[derive(Debug, thiserror::Error)]
#[error("ui/calls.json does not read: {0}")]
pub struct ListError(#[from] serde_json::Error);

/// The entries of `ui/calls.json` (`{"calls": [{"method": ..., "pages": [...]}, ...]}`). A key
/// the file should not have is an error, never ignored.
pub fn listed_calls(text: &str) -> Result<Vec<Listed>, ListError> {
    let file: CallsFile = serde_json::from_str(text)?;
    Ok(file.calls)
}

/// Every method of every service in `descriptors`, as `<package>.<Service>/<Method>`.
pub fn descriptor_methods(descriptors: &FileDescriptorSet) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for file in &descriptors.file {
        for service in &file.service {
            for method in &service.method {
                out.insert(format!(
                    "{}.{}/{}",
                    file.package(),
                    service.name(),
                    method.name()
                ));
            }
        }
    }
    out
}

/// A call that names no method of the descriptors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCall {
    /// The call's method name, as the app spells it.
    pub call: String,
}

impl fmt::Display for UnknownCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is not a method of the protobuf descriptors",
            self.call
        )
    }
}

/// Checks that each of `calls` is a method of `descriptors`: the package, the service and the
/// method, all three. The calls that are not, in the order given, each once.
pub fn check_calls(
    calls: &[String],
    descriptors: &FileDescriptorSet,
) -> Result<(), Vec<UnknownCall>> {
    let known = descriptor_methods(descriptors);
    let mut seen = BTreeSet::new();
    let unknown: Vec<UnknownCall> = calls
        .iter()
        .filter(|c| !known.contains(*c) && seen.insert(c.as_str()))
        .map(|c| UnknownCall { call: c.clone() })
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(unknown)
    }
}

#[cfg(test)]
mod tests;
