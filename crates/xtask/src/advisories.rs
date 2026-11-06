//! `cargo xtask advisories --repo <owner>/<repo>`: the security advisories against the
//! dependencies in `Cargo.lock`, kept in one GitHub issue instead of failing a pull request.
//!
//! `ci.yml` checks bans, licences and sources only: a new advisory against a crate we already
//! use would fail every open pull request for something none of them did (AGENTS.md B9, R6).
//! `.github/workflows/advisories.yml` runs this verb daily, on every push to `main` and by hand
//! instead, and never as a check of a pull request.
//!
//! The verb runs [`DENY_ARGS`] (`cargo deny ... check advisories`, with `--format json`, an
//! option of cargo-deny itself and so before `check`) in the current directory, through the
//! [`AdvisoryCheck`] port, and reads the records cargo-deny 0.20 writes to stderr, one JSON
//! object a line ([`parse`]):
//!
//! - `{"type":"diagnostic","fields":{"code":..,"severity":..,"message":..,"graphs":[{"Krate":
//!   {"name":..,"version":..}, ..}],"advisory":{"id":..,"package":..,"title":.., ..}}}`: a
//!   diagnostic with an `advisory` is a finding: a `vulnerability`, `unmaintained`, `unsound`
//!   or `notice` advisory against the crate and version of its first graph node, the one in
//!   `Cargo.lock`. A `yanked` diagnostic (deny.toml sets `yanked = "deny"`) has no advisory
//!   and is a finding too, under the id [`YANKED`]. Any other diagnostic of severity `error`
//!   (a `deny.toml` that does not parse, an index that cannot be read) is a tool failure; one
//!   of a lower severity is passed over;
//! - `{"type":"log","fields":{"level":..,"message":..}}`: cargo-deny's own log; an `ERROR` one
//!   is a tool failure (an advisory database that could not be fetched);
//! - `{"type":"summary", ..}`: the last record of a check that ran to its end. A run without
//!   one did not finish, and is a tool failure.
//!
//! cargo-deny exits non-zero when it reports an error-level diagnostic, which every finding is
//! by default, so its exit status is not the verdict: a run that failed and reported no finding
//! is a tool failure. A tool failure leaves the issue as it is: the issue is never closed on a
//! run nobody understood.
//!
//! Then the issue titled [`TITLE`], read through the [`GitHubRuns`] port (`gh api`, the
//! adapter the release verbs use), is brought to match ([`decide`]):
//!
//! | findings | open issue | does |
//! |---|---|---|
//! | none | none | nothing |
//! | none | yes | comments [`CLOSE_COMMENT`] and closes it |
//! | some | none | opens it with the body [`render`] gives |
//! | some | yes | replaces its body, when the body differs |
//!
//! When more than one open issue has the title, the lowest number is the one. The body is a
//! pure function of the findings, sorted, so a run that finds what the last one found writes
//! nothing. The verb exits 0 in every row: the issue is the signal, the run informational. It
//! exits 1 on a tool failure or a GitHub call that failed.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::release::contract::Repo;
use crate::release::git::command_line;
use crate::release::ports::{AdvisoryCheck, Exec, GitHubRuns, Issue, PortError, Said};

/// The title of the one issue the findings are kept in.
pub(crate) const TITLE: &str = "Security advisories in hostkeeper's dependencies";

/// What follows `cargo` for the check: the arguments `ci.yml` gives `cargo deny`, narrowed to
/// the advisories and in JSON.
pub(crate) const DENY_ARGS: [&str; 10] = [
    "deny",
    "--log-level",
    "warn",
    "--format",
    "json",
    "--manifest-path",
    "./Cargo.toml",
    "--all-features",
    "check",
    "advisories",
];

/// The id a yanked crate is listed under: it has no advisory.
pub(crate) const YANKED: &str = "yanked";

/// The comment the issue is closed with.
pub(crate) const CLOSE_COMMENT: &str = "`cargo deny check advisories` reports no advisory and no \
     yanked crate against `Cargo.lock` on `main` any more. Closing; the advisories workflow \
     opens a new issue when one appears.";

/// Why the verb failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// cargo-deny or GitHub could not be asked.
    #[error(transparent)]
    Port(#[from] PortError),
    /// A record cargo-deny printed is not one this verb reads.
    #[error("cargo deny output, line {line}: {why}")]
    Output { line: usize, why: String },
    /// cargo-deny said it failed.
    #[error("cargo deny failed: {0}")]
    Tool(String),
    /// cargo-deny printed no summary: the check did not run to its end.
    #[error("cargo deny printed no summary: the check did not finish")]
    Unfinished,
    /// cargo-deny exited non-zero and reported no finding.
    #[error("cargo deny exited non-zero and reported no advisory")]
    FailedWithoutFinding,
}

/// One finding against a crate in `Cargo.lock`. The order of the fields is the order of the
/// body's rows.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Finding {
    /// `RUSTSEC-YYYY-NNNN`, or [`YANKED`].
    pub(crate) id: String,
    pub(crate) krate: String,
    pub(crate) version: String,
    /// cargo-deny's code: `vulnerability`, `unmaintained`, `unsound`, `notice` or `yanked`.
    pub(crate) kind: String,
    pub(crate) title: String,
}

/// What to do with the issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// No finding and no open issue.
    Nothing,
    /// No finding: comment and close the open issue.
    Close(u64),
    /// Findings and no open issue: open one.
    Create,
    /// Findings and an open issue with another body: replace it.
    Update(u64),
    /// Findings and an open issue that already holds the body.
    Unchanged(u64),
}

/// `cargo deny`, run through an [`Exec`].
pub(crate) struct DenyCli<E> {
    exec: E,
}

impl<E: Exec> DenyCli<E> {
    pub(crate) fn new(exec: E) -> DenyCli<E> {
        DenyCli { exec }
    }
}

impl<E: Exec> AdvisoryCheck for DenyCli<E> {
    /// Runs `cargo` with [`DENY_ARGS`]. A non-zero exit is an answer, not an error: [`parse`]
    /// judges it. Only a `cargo` that cannot start is an error.
    fn check_advisories(&self) -> Result<Said, PortError> {
        let args: Vec<String> = DENY_ARGS.iter().map(|a| (*a).to_owned()).collect();
        let ran = self
            .exec
            .spawn("cargo", &args)
            .map_err(|e| PortError::new(command_line("cargo", &args), e.to_string()))?;
        Ok(Said {
            success: ran.success,
            stdout: String::from_utf8_lossy(&ran.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&ran.stderr).into_owned(),
        })
    }
}

/// Runs the check and brings the issue of `repo` to match it; the line to print.
pub(crate) fn sync(
    check: &dyn AdvisoryCheck,
    github: &dyn GitHubRuns,
    repo: &Repo,
) -> Result<Vec<String>, Error> {
    let found = parse(&check.check_advisories()?)?;
    let issues = github.open_issues(repo)?;
    let open = issues
        .iter()
        .filter(|issue| issue.title == TITLE)
        .min_by_key(|issue| issue.number);
    let body = render(&found);
    let n = found.len();
    let line = match decide(&found, &body, open) {
        Action::Nothing => "no advisories, and no open issue".to_owned(),
        Action::Close(number) => {
            github.close_issue(repo, number, CLOSE_COMMENT)?;
            format!("no advisories: closed issue #{number}")
        }
        Action::Create => {
            let number = github.open_issue(repo, TITLE, &body)?;
            format!("{n} found: opened issue #{number}")
        }
        Action::Update(number) => {
            github.edit_issue(repo, number, &body)?;
            format!("{n} found: updated issue #{number}")
        }
        Action::Unchanged(number) => format!("{n} found: issue #{number} is up to date"),
    };
    Ok(vec![line])
}

/// What to do, given the findings, the body they render to and the open issue.
pub(crate) fn decide(found: &[Finding], body: &str, open: Option<&Issue>) -> Action {
    match (found.is_empty(), open) {
        (true, None) => Action::Nothing,
        (true, Some(issue)) => Action::Close(issue.number),
        (false, None) => Action::Create,
        (false, Some(issue)) if issue.body == body => Action::Unchanged(issue.number),
        (false, Some(issue)) => Action::Update(issue.number),
    }
}

/// The findings in what `cargo deny` printed, sorted and without repeats.
pub(crate) fn parse(said: &Said) -> Result<Vec<Finding>, Error> {
    let mut found = BTreeSet::new();
    let mut summary = false;
    for (n, text) in said.stderr.lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let bad = |why: String| Error::Output { line: n + 1, why };
        let record: Value = serde_json::from_str(text)
            .map_err(|e| bad(format!("not a JSON record ({e}): {}", clip(text))))?;
        let fields = &record["fields"];
        match record["type"].as_str() {
            Some("diagnostic") => {
                if let Some(finding) = finding_of(fields).map_err(bad)? {
                    found.insert(finding);
                }
            }
            Some("log") if fields["level"] == "ERROR" => {
                return Err(Error::Tool(text_of(fields, "message")));
            }
            Some("summary") => summary = true,
            Some(_) => {}
            None => return Err(bad("no `type`".to_owned())),
        }
    }
    if !summary {
        return Err(Error::Unfinished);
    }
    if found.is_empty() && !said.success {
        return Err(Error::FailedWithoutFinding);
    }
    Ok(found.into_iter().collect())
}

/// At most 200 characters of `text`, for an error.
fn clip(text: &str) -> String {
    text.chars().take(200).collect()
}

/// The string at `key` of `object`, or an empty one.
fn text_of(object: &Value, key: &str) -> String {
    object[key].as_str().unwrap_or_default().to_owned()
}

/// The string at `key` of `object`, or why it is not there.
fn string(object: &Value, key: &str) -> Result<String, String> {
    object[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("no string `{key}`"))
}

/// The finding a diagnostic reports: none for one that is no finding and below `error`.
fn finding_of(fields: &Value) -> Result<Option<Finding>, String> {
    let code = string(fields, "code")?;
    let advisory = fields.get("advisory").filter(|a| !a.is_null());
    if advisory.is_none() && code != YANKED {
        if fields["severity"] == "error" {
            return Err(format!(
                "an error that is no advisory ({code}): {}",
                text_of(fields, "message")
            ));
        }
        return Ok(None);
    }
    let krate = &fields["graphs"][0]["Krate"];
    let name = string(krate, "name").map_err(|why| format!("{code}: graph crate: {why}"))?;
    let version = string(krate, "version").map_err(|why| format!("{code}: graph crate: {why}"))?;
    let (id, title) = match advisory {
        Some(entry) => (string(entry, "id")?, string(entry, "title")?),
        None => (YANKED.to_owned(), string(fields, "message")?),
    };
    Ok(Some(Finding {
        id,
        krate: name,
        version,
        kind: code,
        title,
    }))
}

/// A Markdown table cell: one line, no `|`.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// Whether `id` is a RustSec id, `RUSTSEC-` and four digits, `-` and four digits, so that it
/// can stand in a link.
fn is_rustsec(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("RUSTSEC-") else {
        return false;
    };
    let digits = |s: &str| s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit());
    matches!(rest.split_once('-'), Some((year, number)) if digits(year) && digits(number))
}

/// The advisory cell: a RustSec id linked to its page, anything else as text.
fn advisory_cell(id: &str) -> String {
    if is_rustsec(id) {
        format!("[{id}](https://rustsec.org/advisories/{id})")
    } else {
        cell(id)
    }
}

/// The body of the issue: a pure function of the set of findings, one row each, sorted by
/// advisory id, crate, version, kind and title, whatever the order of `found`.
pub(crate) fn render(found: &[Finding]) -> String {
    let rows: BTreeSet<&Finding> = found.iter().collect();
    let mut body = format!(
        "`cargo deny check advisories` reports {} against the dependencies in `Cargo.lock` on \
         `main`.\n\n| Advisory | Kind | Crate | Version | Title |\n|---|---|---|---|---|\n",
        match rows.len() {
            1 => "1 finding".to_owned(),
            n => format!("{n} findings"),
        }
    );
    for f in rows {
        body.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            advisory_cell(&f.id),
            cell(&f.kind),
            cell(&f.krate),
            cell(&f.version),
            cell(&f.title)
        ));
    }
    body.push_str(
        "\nThe advisories workflow (`.github/workflows/advisories.yml`) rewrites this issue \
         daily and on every push to `main`, and closes it when none remain. Advisories never \
         block a pull request.\n",
    );
    body
}

#[cfg(test)]
mod tests;
