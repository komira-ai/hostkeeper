//! The operator API methods the web app calls: where its sources call them, the list in
//! `ui/calls.json` derived from those calls, and the check against the protobuf descriptors.
//!
//! A page calls a method through the generated client (`ui/gen/client.js`) as
//! `api.<service>.<function>(request)`, `api` the client the page was given (`ctx.api`, or a
//! parameter named `api`). [`call_sites`] reads those calls from a module's text and refuses
//! what would hide one from the list: `api.` followed by anything but `<service>.<function>(`
//! (`api.call(...)`, `api["hosts"]`, `api.hosts.list.call(...)`, a function kept in a variable),
//! the client reached as `api?.` or `ctx["api"]`,
//! the client given another name (`const a = ctx.api`), and a method's full name in a string,
//! which only the generated client spells. [`derived_calls`] turns the calls into the entries of
//! `ui/calls.json`, each function with its method and the pages that call it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use prost_types::FileDescriptorSet;

use crate::client_gen::Function;
use crate::lexis::{is_ident, joined, line_of, skip_blank};
use serde::Deserialize;

/// Where a module calls a function of the client: the module's path under `ui/`, the 1-based
/// line and the function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// The module's path under `ui/`.
    pub path: String,
    /// The line of the call's `api`.
    pub line: usize,
    /// The function, as the page writes it after `api.`: `hosts.list`.
    pub function: String,
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
    /// `api.` followed by something other than `<service>.<function>(`.
    #[error("{path}:{line}: `api.` is not a call of `api.<service>.<function>(...)`")]
    NotACall { path: String, line: usize },
    /// The client given another name, through which its calls would be hidden.
    #[error("{path}:{line}: the client is given another name")]
    Aliased { path: String, line: usize },
    /// A string that opens with a method's full name.
    #[error("{path}:{line}: a method name outside the generated client")]
    MethodName { path: String, line: usize },
}

/// The prefix of every method name, as it opens a string literal.
const NAME_OPENING: &str = "\"hostkeeper.";

/// The name the pages give the client.
const CLIENT: &str = "api";

/// The JavaScript name that opens `rest`, after blanks, and the text after it.
fn name(rest: &str) -> Option<(&str, &str)> {
    let rest = skip_blank(rest);
    let end = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
    (end > 0).then(|| rest.split_at(end))
}

/// The member `.name` that opens `rest`, blanks allowed around the `.`, and the text after it.
fn member(rest: &str) -> Option<(&str, &str)> {
    name(skip_blank(rest).strip_prefix('.')?)
}

/// Whether the client at `at` is assigned to another name: `= api` or `= ctx.api`, not `==`,
/// `!=`, `<=` or `>=`.
fn aliased(text: &str, at: usize) -> bool {
    let mut before = text[..at].trim_end();
    // `ctx.api`: back over the objects it is a member of.
    while let Some(object) = before.strip_suffix('.') {
        before = object.trim_end().trim_end_matches(is_ident).trim_end();
    }
    before
        .strip_suffix('=')
        .is_some_and(|rest| !rest.ends_with(['=', '!', '<', '>']))
}

/// Every call of the client in a module, in the order written. `path` is the module's path
/// under `ui/`, for the sites and the errors. The client is the name `api` (not part of a longer
/// name, and not the module name `api.js` when nothing but punctuation or a blank follows it);
/// where it is followed by `.` or `[` it must be `api.<service>.<function>`, blanks allowed,
/// followed by `(`.
pub fn call_sites(path: &str, text: &str) -> Result<Vec<CallSite>, ScanError> {
    let error =
        |at: usize, make: fn(String, usize) -> ScanError| make(path.to_owned(), line_of(text, at));
    if let Some((at, _)) = text.match_indices(NAME_OPENING).next() {
        return Err(error(at, |path, line| ScanError::MethodName { path, line }));
    }
    let mut sites = Vec::new();
    for (at, _) in text.match_indices(CLIENT) {
        let rest = &text[at + CLIENT.len()..];
        if joined(text, at) || rest.starts_with(is_ident) {
            continue;
        }
        // The module's name, `api.js`, in a comment or an import: not followed by more of a
        // name, a member or a call.
        if rest
            .strip_prefix(".js")
            .is_some_and(|after| !after.starts_with(|c: char| is_ident(c) || c == '.' || c == '('))
        {
            continue;
        }
        // `api?.hosts` and `ctx["api"]`: the client reached in a way no call is read from.
        let quoted = text[..at].ends_with(['"', '\'']) && rest.starts_with(['"', '\'']);
        if quoted || skip_blank(rest).starts_with('?') {
            return Err(error(at, |path, line| ScanError::NotACall { path, line }));
        }
        if skip_blank(rest).starts_with(['.', '[']) {
            let called = member(rest)
                .and_then(|(service, rest)| {
                    member(rest).map(|(function, rest)| (service, function, rest))
                })
                .filter(|(_, _, rest)| skip_blank(rest).starts_with('('));
            let Some((service, function, _)) = called else {
                return Err(error(at, |path, line| ScanError::NotACall { path, line }));
            };
            sites.push(CallSite {
                path: path.to_owned(),
                line: line_of(text, at),
                function: format!("{service}.{function}"),
            });
        } else if aliased(text, at) {
            return Err(error(at, |path, line| ScanError::Aliased { path, line }));
        }
    }
    Ok(sites)
}

/// Why the calls cannot be listed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeriveError {
    /// A call of a function the generated client does not have.
    #[error("{path}:{line}: api.{function} is not a function of the generated client")]
    UnknownFunction {
        path: String,
        line: usize,
        function: String,
    },
    /// A call outside `pages/`, where no call belongs.
    #[error("{path}:{line}: a call outside pages/")]
    OutsidePages { path: String, line: usize },
}

/// The entries of `ui/calls.json` for `sites`: one per function called, by method, with the
/// pages that call it, by name, each once. `functions` is the generated client's.
pub fn derived_calls(
    sites: &[CallSite],
    functions: &[Function],
) -> Result<Vec<Listed>, DeriveError> {
    let mut called: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    for site in sites {
        let Some(function) = functions.iter().find(|f| f.call() == site.function) else {
            return Err(DeriveError::UnknownFunction {
                path: site.path.clone(),
                line: site.line,
                function: site.function.clone(),
            });
        };
        let Some(page) = site.page() else {
            return Err(DeriveError::OutsidePages {
                path: site.path.clone(),
                line: site.line,
            });
        };
        called
            .entry(function.method.clone())
            .or_insert_with(|| (function.call(), BTreeSet::new()))
            .1
            .insert(page.to_owned());
    }
    Ok(called
        .into_iter()
        .map(|(method, (function, pages))| Listed {
            function,
            method,
            pages: pages.into_iter().collect(),
        })
        .collect())
}

/// The text of `ui/calls.json` that lists `entries`, one line each.
pub fn calls_json(entries: &[Listed]) -> String {
    let quoted = |s: &str| serde_json::Value::from(s).to_string();
    let lines: Vec<String> = entries
        .iter()
        .map(|entry| {
            let pages: Vec<String> = entry.pages.iter().map(|p| quoted(p)).collect();
            format!(
                "    {{ \"function\": {}, \"method\": {}, \"pages\": [{}] }}",
                quoted(&entry.function),
                quoted(&entry.method),
                pages.join(", ")
            )
        })
        .collect();
    format!("{{\n  \"calls\": [\n{}\n  ]\n}}\n", lines.join(",\n"))
}

/// One entry of `ui/calls.json`: a function of the client, its method and the pages that call
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listed {
    /// The function, as a page calls it after `api.`: `hosts.list`.
    pub function: String,
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

/// The entries of `ui/calls.json` (`{"calls": [{"function": ..., "method": ..., "pages": [...]},
/// ...]}`). A key the file should not have is an error, never ignored.
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
