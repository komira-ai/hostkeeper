//! The path rule of `pr-device.yml`: its job [`CHANGES_JOB`] answers whether a pull request
//! changes a path the macOS jobs build, test or read, and the macOS jobs run unless it answers
//! `needed=false` ([`super::pr_device`]).
//!
//! The answer is one `git diff` ([`DIFF_PREFIX`]): the paths changed between the merge base of the
//! pull request's base and its head, less those its exclude pathspecs (the skip list) match. It
//! exits 0 when nothing is left (`needed=false`), 1 when a path is, and 128 when it cannot compute
//! the diff; anything but 0 answers `needed=true` ([`NEEDED`]).
//!
//! [`changes_problems`] holds the job to its shape: [`CHANGES_TEMPLATE`] key for key (its checkout
//! of the whole history with no token kept, its step `diff` that may fail, the SHAs of the event,
//! its output), and the step's command to [`DIFF_PREFIX`] followed by exclude pathspecs of the
//! grammar [`Pattern`] reads, which [`Pattern::matches`] evaluates as git does
//! (`pr_paths_tests.rs` runs git on the same paths). [`entry_problems`] holds each entry to a
//! reason it cannot change a macOS job:
//! - under `docs/`, or a file at the top of the repository named `*.md`: text no macOS job
//!   compiles; [`read_problems`] refuses one that a source a macOS job may compile (any file but
//!   another crate's tests) names in an `include_str!` or `include_bytes!`, or that any file of a
//!   crate whose tests run on macOS names in a string (`README.md` and `docs/macos.md`, for example, are read by such tests);
//! - a workflow ([`WORKFLOWS_DIR`]) with no job on a macOS runner that is not one of
//!   [`KEPT_WORKFLOWS`] (this workflow, the one its jobs copy, and the release workflows);
//! - a test target of `crates/hostkeeper-privileged` ([`PRIVILEGED_TESTS`]) directly in its
//!   `tests/` that the macOS job does not name with `--test` and that no other file of the crate
//!   includes (`mod <name>;` in another file of `tests/`, a `#[path]` or any string naming
//!   `<name>.rs`, a target of its `Cargo.toml`): cargo then never builds it on macOS.
//!
//! Every entry also matches a file of the repository, so the list holds no stale entry.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::lint::{MACOS_DEVICE_JOB, entries, entry};
use super::pr_device::{CHANGES_JOB, FILE, MACOS_TEST_JOB};
use super::rust_text::rust_text_mask;
use super::yaml::{self, Node, Value};

/// The command of the step `diff`, before its pathspecs: `--quiet` exits 1 when a path is left,
/// `--merge-base` diffs the merge base of the two SHAs against the head (and fails on an empty
/// one), `--no-renames` lists a rename as its old and its new path, and `.` is every path.
pub const DIFF_PREFIX: &str =
    "git diff --quiet --no-renames --merge-base \"$BASE_SHA\" \"$HEAD_SHA\" -- .";

/// What every pathspec of the skip list starts with: git's exclude, with `*` and `**` read as
/// globs that do not cross a `/` (and a pattern anchored at the top of the repository).
pub const EXCLUDE: &str = ":(exclude,glob)";

/// The output of [`CHANGES_JOB`]: `false` only on a pull request whose diff exited 0.
pub const NEEDED: &str =
    "${{ github.event_name != 'pull_request' || steps.diff.outcome != 'success' }}";

/// [`CHANGES_JOB`], key for key, with `<USES>` for its checkout's pinned action (held by the
/// lint's pin rule) and `<RUN>` for its diff command (held to [`DIFF_PREFIX`] and the grammar).
pub const CHANGES_TEMPLATE: &str = "\
runs-on: ubuntu-24.04
permissions:
  contents: read
timeout-minutes: 10
outputs:
  needed: ${{ github.event_name != 'pull_request' || steps.diff.outcome != 'success' }}
steps:
  - uses: <USES>
    with:
      persist-credentials: false
      fetch-depth: 0
  - name: Changed paths outside the skip list (exit 1), or no diff (exit 128)
    id: diff
    continue-on-error: true
    env:
      BASE_SHA: ${{ github.event.pull_request.base.sha }}
      HEAD_SHA: ${{ github.event.pull_request.head.sha }}
    run: <RUN>
";

/// The directory of the workflows.
pub const WORKFLOWS_DIR: &str = ".github/workflows/";

/// The workflows the skip list may not hold, whatever their jobs: this one, privileged.yml (whose
/// jobs it copies), and the workflows that build and publish a release.
pub const KEPT_WORKFLOWS: [&str; 4] = [FILE, "privileged.yml", "artifacts.yml", "release.yml"];

/// The test targets of the crate whose macOS tests [`MACOS_DEVICE_JOB`] runs.
pub const PRIVILEGED_TESTS: &str = "crates/hostkeeper-privileged/tests/";

/// `(file, string)`: a string of a file a macOS job compiles that names a skipped path but reads
/// nothing. hostkeeper-package's test refuses `.github/workflows` in the crate's sources; it is
/// the text it looks for.
pub const NOT_READS: [(&str, &str); 1] = [(
    "crates/hostkeeper-package/src/crate_tests.rs",
    ".github/workflows",
)];

/// One segment of a [`Pattern`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// The segment itself.
    Literal(String),
    /// `*`: any one segment.
    One,
    /// `**`, last: one or more segments.
    Rest,
}

/// The pattern of an exclude pathspec: segments joined by `/`, each a name with no glob
/// character, `*` (any one segment), or, last only, `**` (any one or more). Nothing else is read:
/// git's other globs (`*.md`, `?`, `[...]`) are refused, so the lint never evaluates a pattern
/// otherwise than git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub text: String,
    pub segments: Vec<Segment>,
}

impl Pattern {
    /// The pattern of `text`, or why it is not one of the grammar.
    pub fn parse(text: &str) -> Result<Pattern, String> {
        let parts: Vec<&str> = text.split('/').collect();
        let mut segments = Vec::new();
        for (n, part) in parts.iter().enumerate() {
            let segment = match *part {
                "**" if n + 1 == parts.len() && n > 0 => Segment::Rest,
                "*" => Segment::One,
                "" | "." | ".." => return Err(format!("{text:?}: an empty, `.` or `..` segment")),
                _ if part.contains(['*', '?', '[', ']', '\\', '!', ':', '\'', '"']) => {
                    return Err(format!(
                        "{text:?}: {part:?} is neither a name, `*` nor a last `**`"
                    ));
                }
                _ => Segment::Literal((*part).to_owned()),
            };
            segments.push(segment);
        }
        if segments.iter().all(|s| !matches!(s, Segment::Literal(_))) {
            return Err(format!("{text:?}: no segment names a path"));
        }
        Ok(Pattern {
            text: text.to_owned(),
            segments,
        })
    }

    /// Whether git's exclude pathspec of this pattern matches the repository path `path`.
    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        let mut at = 0;
        for segment in &self.segments {
            match segment {
                Segment::Rest => {
                    return parts.len() > at && parts[at..].iter().all(|p| !p.is_empty());
                }
                Segment::One => {
                    if parts.get(at).is_none_or(|p| p.is_empty()) {
                        return false;
                    }
                }
                Segment::Literal(name) => {
                    if parts.get(at) != Some(&name.as_str()) {
                        return false;
                    }
                }
            }
            at += 1;
        }
        at == parts.len()
    }

    /// Whether a path under the directory `dir` may match: `dir`'s segments agree with the
    /// pattern's first ones and the pattern goes on after them.
    pub fn may_hold(&self, dir: &str) -> bool {
        let parts: Vec<&str> = dir.split('/').collect();
        if parts.len() >= self.segments.len()
            && !matches!(self.segments.last(), Some(Segment::Rest))
        {
            return false;
        }
        parts
            .iter()
            .zip(&self.segments)
            .all(|(part, segment)| match segment {
                Segment::Rest | Segment::One => !part.is_empty(),
                Segment::Literal(name) => name == part,
            })
    }
}

/// Whether the macOS jobs are needed for a pull request that changes `changed` (repository
/// paths, a rename as its old and its new path) under `skip`: whether a path is matched by no
/// pattern. A pull request that changes nothing needs nothing.
pub fn needed(skip: &[Pattern], changed: &[&str]) -> bool {
    changed
        .iter()
        .any(|path| !skip.iter().any(|p| p.matches(path)))
}

/// The pathspec patterns of the diff command `run`, or why it is not [`DIFF_PREFIX`] followed by
/// single-quoted exclude pathspecs of the grammar, one at least.
pub fn skip_list_of(run: &str) -> Result<Vec<Pattern>, String> {
    let rest = run
        .strip_prefix(DIFF_PREFIX)
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
        .ok_or_else(|| format!("the diff step must run `{DIFF_PREFIX}` and its pathspecs"))?;
    let mut patterns = Vec::new();
    for word in rest.split(' ').filter(|w| !w.is_empty()) {
        let spec = word
            .strip_prefix('\'')
            .and_then(|w| w.strip_suffix('\''))
            .filter(|w| !w.contains('\''))
            .ok_or_else(|| format!("the pathspec {word} is not one word in single quotes"))?;
        let pattern = spec
            .strip_prefix(EXCLUDE)
            .ok_or_else(|| format!("the pathspec {word} does not start with {EXCLUDE}"))?;
        patterns.push(Pattern::parse(pattern)?);
    }
    if patterns.is_empty() {
        return Err("the diff step names no pathspec to skip".to_owned());
    }
    Ok(patterns)
}

/// The job [`CHANGES_JOB`] of the document `doc`.
fn changes_job(doc: &Node) -> Option<&Node> {
    entry(doc, "jobs").and_then(|jobs| entry(jobs, CHANGES_JOB))
}

/// The steps of `job`.
fn steps(job: &Node) -> &[Node] {
    match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps,
        _ => &[],
    }
}

/// The diff command of [`CHANGES_JOB`] of `doc`: the `run` of its step `diff`.
pub fn diff_run(doc: &Node) -> Option<&str> {
    let job = changes_job(doc)?;
    steps(job)
        .iter()
        .find(|step| entry(step, "id").and_then(Node::as_str) == Some("diff"))
        .and_then(|step| entry(step, "run"))
        .and_then(Node::as_str)
}

/// The skip list of [`FILE`] (`doc`).
pub fn skip_list(doc: &Node) -> Result<Vec<Pattern>, String> {
    skip_list_of(diff_run(doc).ok_or("no step `diff` with a `run` in the job changes")?)
}

/// `node` with the scalar at `path` (keys, or `#n` for the n-th item) set to `text`, plain.
fn with_scalar(node: &Node, path: &[&str], text: &str) -> Node {
    let mut copy = node.clone();
    let mut at = &mut copy;
    for key in path {
        let next = match &mut at.value {
            Value::Map(entries) => entries
                .iter_mut()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v),
            Value::Seq(items) => key
                .strip_prefix('#')
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| items.get_mut(n)),
            Value::Scalar { .. } => None,
        };
        match next {
            Some(next) => at = next,
            None => return copy,
        }
    }
    at.value = Value::Scalar {
        text: text.to_owned(),
        plain: true,
    };
    copy
}

/// What is wrong with [`CHANGES_JOB`] of [`FILE`] (`doc`): a job other than
/// [`CHANGES_TEMPLATE`] (with its pinned checkout and its diff command), a checkout of another
/// action, or a diff command off [`DIFF_PREFIX`] or the grammar.
pub fn changes_problems(doc: &Node) -> Vec<String> {
    let Some(job) = changes_job(doc) else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let checkout = steps(job)
        .first()
        .and_then(|s| entry(s, "uses"))
        .and_then(Node::as_str);
    if !checkout.is_some_and(|u| u.starts_with("actions/checkout@")) {
        problems.push(format!(
            "job {CHANGES_JOB}: its first step must be actions/checkout"
        ));
    }
    let job = with_scalar(job, &["steps", "#0", "uses"], "<USES>");
    let job = with_scalar(&job, &["steps", "#1", "run"], "<RUN>");
    let template = yaml::load(CHANGES_TEMPLATE).expect("the template parses");
    if !super::pr_device::same(&job, &template) {
        problems.push(format!(
            "job {CHANGES_JOB}: not pr_paths::CHANGES_TEMPLATE, key for key"
        ));
    }
    if let Err(why) = skip_list(doc) {
        problems.push(format!("job {CHANGES_JOB}: {why}"));
    }
    problems
}

/// The bodies of the string literals of the Rust source `text` (escapes as written).
pub fn strings(text: &str) -> Vec<(usize, String)> {
    let mask = rust_text_mask(text);
    let mut out = Vec::new();
    let mut i = 0;
    while i < mask.len() {
        if !mask[i] {
            i += 1;
            continue;
        }
        let end = mask[i..]
            .iter()
            .position(|m| !m)
            .map_or(mask.len(), |p| i + p);
        let run = &text[i..end];
        if !run.starts_with("//") && !run.starts_with("/*") {
            out.push((i, run.to_owned()));
        }
        i = end;
    }
    out
}

/// The string literals inside the `include_str!(...)` and `include_bytes!(...)` calls of the
/// Rust source `text`.
pub fn included(text: &str) -> Vec<String> {
    let mask = rust_text_mask(text);
    let all = strings(text);
    let mut out = Vec::new();
    for macro_name in ["include_str!", "include_bytes!"] {
        for (at, _) in text.match_indices(macro_name) {
            if mask[at] {
                continue;
            }
            let mut depth = 0usize;
            let mut end = text.len();
            for (offset, c) in text[at..].char_indices() {
                let i = at + offset;
                if mask[i] {
                    continue;
                }
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.extend(
                all.iter()
                    .filter(|(start, _)| *start > at && *start < end)
                    .map(|(_, s)| s.clone()),
            );
        }
    }
    out
}

/// The repository path a string names, read from any directory of it: leading `/`, `./` and
/// `../` and a trailing `/` dropped. None for a string that names no path (empty after that, or
/// with a space, a newline or an escape in it).
pub fn path_of(string: &str) -> Option<String> {
    let mut rest = string;
    loop {
        let next = rest
            .strip_prefix('/')
            .or_else(|| rest.strip_prefix("./"))
            .or_else(|| rest.strip_prefix("../"));
        match next {
            Some(next) => rest = next,
            None => break,
        }
    }
    let rest = rest.trim_end_matches('/');
    let named = !rest.is_empty() && rest != "." && rest != "..";
    (named && !rest.contains([' ', '\n', '\t', '\\'])).then(|| rest.to_owned())
}

/// Every file under `dir`, as paths from `root` with `/`, but those in `.git` and in a directory
/// named `target`.
pub fn files_under(root: &Path, dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if name != ".git" && name != "target" {
                    pending.push(path);
                }
            } else if let Ok(relative) = path.strip_prefix(root) {
                let parts: Vec<String> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                found.push(parts.join("/"));
            }
        }
    }
    found.sort();
    found
}

/// Whether `source` is a file of tests by the workspace's convention (the one the coverage report
/// ignores): under a `tests/` directory, or named `tests.rs` or `*_tests.rs` (a `#[cfg(test)]`
/// module). Such a file of a crate is compiled only for that crate's own tests.
pub fn test_only(source: &str) -> bool {
    let name = source.rsplit('/').next().unwrap_or_default();
    source.contains("/tests/") || name == "tests.rs" || name.ends_with("_tests.rs")
}

/// What the skip list `skip` lets through that a macOS job reads: for each Rust source of the
/// repository (`sources`, paths from `root`) that no entry skips, a path the source names that an
/// entry matches or may hold, in any string of a file under one of `mac_dirs` (the crates whose
/// tests run on macOS), and in an `include_str!` or `include_bytes!` of any other file but a file
/// of tests ([`test_only`]: macOS runs no other crate's tests), but the strings of [`NOT_READS`].
pub fn read_problems(
    root: &Path,
    skip: &[Pattern],
    sources: &[String],
    mac_dirs: &[String],
) -> Vec<String> {
    let mut problems = Vec::new();
    for source in sources {
        if skip.iter().any(|p| p.matches(source)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(source)) else {
            continue;
        };
        let on_mac = mac_dirs
            .iter()
            .any(|dir| source.starts_with(&format!("{dir}/")));
        let named: Vec<String> = if on_mac {
            strings(&text).into_iter().map(|(_, s)| s).collect()
        } else if test_only(source) {
            Vec::new()
        } else {
            included(&text)
        };
        let named: BTreeSet<String> = named.into_iter().collect();
        for string in named {
            if NOT_READS.contains(&(source.as_str(), string.as_str())) {
                continue;
            }
            let Some(path) = path_of(&string) else {
                continue;
            };
            let is_file = root.join(&path).is_file();
            for pattern in skip {
                if pattern.matches(&path) || (!is_file && pattern.may_hold(&path)) {
                    problems.push(format!(
                        "{source} names {string:?}, which the skip entry {} holds",
                        pattern.text
                    ));
                }
            }
        }
    }
    problems
}

/// The test targets [`MACOS_DEVICE_JOB`] of [`FILE`] (`doc`) names with `--test`.
pub fn device_tests(doc: &Node) -> BTreeSet<String> {
    let job = entry(doc, "jobs").and_then(|jobs| entry(jobs, MACOS_DEVICE_JOB));
    let mut found = BTreeSet::new();
    for step in job.map_or(&[][..], steps) {
        let run = entry(step, "run")
            .and_then(Node::as_str)
            .unwrap_or_default();
        let words: Vec<&str> = run.split_whitespace().collect();
        for pair in words.windows(2).filter(|pair| pair[0] == "--test") {
            found.insert(pair[1].to_owned());
        }
    }
    found
}

/// The packages [`MACOS_TEST_JOB`] of [`FILE`] (`doc`) tests: the `-p` of its steps.
pub fn macos_test_packages(doc: &Node) -> BTreeSet<String> {
    let job = entry(doc, "jobs").and_then(|jobs| entry(jobs, MACOS_TEST_JOB));
    let mut found = BTreeSet::new();
    for step in job.map_or(&[][..], steps) {
        let run = entry(step, "run")
            .and_then(Node::as_str)
            .unwrap_or_default();
        let words: Vec<&str> = run.split_whitespace().collect();
        for pair in words.windows(2).filter(|pair| pair[0] == "-p") {
            found.insert(pair[1].to_owned());
        }
    }
    found
}

/// Whether some job of the workflow `text` runs on a macOS runner (a value of its `runs-on`
/// names `macos`, in any case), or the text does not parse.
fn has_macos_job(text: &str) -> bool {
    let Ok(doc) = yaml::load(text) else {
        return true;
    };
    entry(&doc, "jobs")
        .map_or(&[][..], entries)
        .iter()
        .any(|(_, job)| {
            let mut values = Vec::new();
            if let Some(runner) = entry(job, "runs-on") {
                super::lint::scalars(runner, &mut values);
            }
            values.iter().any(|v| v.to_lowercase().contains("macos"))
        })
}

/// What is wrong with the entries of the skip list `skip` of the repository at `root`, whose
/// files are `files` (paths from `root`), given the test targets the macOS device job names
/// (`device_tests`): an entry that matches no file, or that is none of the kinds the module
/// documentation lists.
pub fn entry_problems(
    root: &Path,
    skip: &[Pattern],
    files: &[String],
    device_tests: &BTreeSet<String>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let privileged: Vec<&String> = files
        .iter()
        .filter(|f| f.starts_with("crates/hostkeeper-privileged/"))
        .collect();
    for pattern in skip {
        let text = pattern.text.as_str();
        if !files.iter().any(|f| pattern.matches(f)) {
            problems.push(format!("the skip entry {text} matches no file"));
            continue;
        }
        let literal = pattern
            .segments
            .iter()
            .all(|s| matches!(s, Segment::Literal(_)));
        if let Some(workflow) = text.strip_prefix(WORKFLOWS_DIR) {
            let text_of = std::fs::read_to_string(root.join(text)).unwrap_or_default();
            if !literal || workflow.contains('/') {
                problems.push(format!("the skip entry {text} is not one workflow file"));
            } else if KEPT_WORKFLOWS.contains(&workflow) {
                problems.push(format!(
                    "the skip entry {text} is a workflow the macOS jobs depend on"
                ));
            } else if has_macos_job(&text_of) {
                problems.push(format!("the skip entry {text} has a job on macOS"));
            }
        } else if let Some(target) = text.strip_prefix(PRIVILEGED_TESTS) {
            let stem = target.strip_suffix(".rs").unwrap_or_default();
            let one_target = literal
                && !target.contains('/')
                && !stem.is_empty()
                && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !one_target {
                problems.push(format!(
                    "the skip entry {text} is not one test target of {PRIVILEGED_TESTS}"
                ));
                continue;
            }
            if device_tests.contains(stem) {
                problems.push(format!(
                    "the skip entry {text} is a test target {MACOS_DEVICE_JOB} builds"
                ));
            }
            let module = format!("mod {stem};");
            let file_name = format!("/{stem}.rs\"");
            let target_path = format!("\"tests/{stem}.rs\"");
            for other in privileged.iter().filter(|f| f.as_str() != text) {
                let content = std::fs::read_to_string(root.join(other)).unwrap_or_default();
                // `mod <stem>;` is the target's file only in a file of `tests/` itself (a
                // target's root); elsewhere it is a file of that file's own directory.
                let in_tests = other
                    .strip_prefix(PRIVILEGED_TESTS)
                    .is_some_and(|rest| !rest.contains('/'));
                let names = if other.ends_with(".rs") {
                    (in_tests && content.contains(&module))
                        || content.contains(&file_name)
                        || content.contains(&format!("\"{stem}.rs\""))
                } else {
                    content.contains(&target_path)
                };
                if names {
                    problems.push(format!("the skip entry {text} is included by {other}"));
                }
            }
        } else {
            let doc = text.starts_with("docs/");
            let top_md = pattern.segments.len() == 1 && literal && text.ends_with(".md");
            if !doc && !top_md {
                problems.push(format!(
                    "the skip entry {text} is neither under docs/, a top-level *.md, a workflow \
                     nor a test target of {PRIVILEGED_TESTS}"
                ));
            }
        }
    }
    problems
}

/// The directories of the crates whose tests run on macOS: those [`MACOS_TEST_JOB`] tests and
/// `crates/hostkeeper-privileged`, found by the package names of the manifests under `crates/`
/// and `examples/` of `root`. A package with no manifest found is an error.
pub fn mac_dirs(root: &Path, doc: &Node) -> Result<Vec<String>, String> {
    let mut by_name = std::collections::BTreeMap::new();
    for top in ["crates", "examples"] {
        let Ok(read) = std::fs::read_dir(root.join(top)) else {
            continue;
        };
        for entry in read.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let table: toml::Table = text.parse().map_err(|e| format!("{manifest:?}: {e}"))?;
            if let Some(name) = table
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str)
            {
                let dir = format!("{top}/{}", entry.file_name().to_string_lossy());
                by_name.insert(name.to_owned(), dir);
            }
        }
    }
    let mut wanted = macos_test_packages(doc);
    wanted.insert("hostkeeper-privileged".to_owned());
    wanted
        .iter()
        .map(|name| {
            by_name
                .get(name)
                .cloned()
                .ok_or_else(|| format!("no crate named {name} under crates/ or examples/"))
        })
        .collect()
}

/// The root of the repository.
pub fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}
