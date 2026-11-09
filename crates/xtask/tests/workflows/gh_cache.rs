//! The rule of every download of the pinned gh (a step whose `run:` names
//! [`GH_RELEASES`]): the job takes the archive from the actions cache when a run before it
//! cached that version and SHA-256, downloads it from GitHub only when it did not, checks it
//! against the pin whichever way it came, and caches it once the check passed. So a job whose
//! pin is cached runs through an outage of GitHub's release downloads, and one whose pin is not
//! fails and says why in one line.
//!
//! The download is the third of five steps in a row, each held to its shape:
//!
//! 1. `actions/cache/restore` with `id: gh-cache`, `path: ${{ runner.temp }}/<file>` and
//!    `key: gh-${{ env.GH_VERSION }}-<pin>`, where `<file>` is the download's
//!    `--output "$RUNNER_TEMP/<file>"` and `<pin>` the expression of the SHA-256 the check
//!    takes (`${{ env.GH_SHA256 }}` for `"$GH_SHA256"`, or the value of the check step's own
//!    `env:` entry of that name).
//! 2. The download itself, one `curl` with `id: gh-download` and `if:` [`ON_MISS`].
//! 3. The note: `if:` [`ON_DOWNLOAD_FAILURE`] and a `run:` of one line,
//!    `echo "::error::…"`. It runs only when the download ran and failed.
//! 4. The check: `cargo xtask install-tool --download "$RUNNER_TEMP/<file>" --sha256 "$<NAME>"`
//!    with no `if:`, so it runs on a cache hit as on a download; every use of the archive comes
//!    after it.
//! 5. `actions/cache/save` with `if:` [`ON_MISS`], the same `path`, and `key:` [`SAVED_KEY`].
//!    It follows a passed check (its `if:` names no status function, so GitHub runs it only
//!    when every step before it succeeded).
//!
//! `gh_cache_tests.rs` shows each defect refused, and runs the five conditions of the
//! repository's own steps against every outcome of the cache and the download.

use super::lint::entry;
use super::xtask_calls::{commands, xtask_args};
use super::yaml::{Node, Value};

/// Where gh's release archives are downloaded from.
pub const GH_RELEASES: &str = "https://github.com/cli/cli/releases/download/";

/// The `if:` of the download and of the save: the cache held no archive under the key.
pub const ON_MISS: &str = "${{ steps.gh-cache.outputs.cache-hit != 'true' }}";

/// The `if:` of the note: the download ran and failed.
pub const ON_DOWNLOAD_FAILURE: &str =
    "${{ !cancelled() && steps.gh-download.outcome == 'failure' }}";

/// The `key:` of the save: the key the restore looked up.
pub const SAVED_KEY: &str = "${{ steps.gh-cache.outputs.cache-primary-key }}";

const RESTORE: &str = "actions/cache/restore@";
const SAVE: &str = "actions/cache/save@";
const NOTE: &str = "echo \"::error::";

/// One line per problem of the gh downloads of the workflow `doc`.
pub fn problems(doc: &Node) -> Vec<String> {
    let jobs = entry(doc, "jobs").map_or(&[][..], super::lint::entries);
    let mut problems = Vec::new();
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or_default();
        let steps = match entry(job, "steps").map(|s| &s.value) {
            Some(Value::Seq(steps)) => &steps[..],
            _ => &[],
        };
        for (at, step) in steps.iter().enumerate() {
            if let Some(run) = text(step, "run").filter(|r| r.contains(GH_RELEASES)) {
                let line = entry(step, "run").map_or(0, Node::line);
                problems.extend(download_problems(name, line, run, steps, at));
            }
        }
    }
    problems
}

/// The value of `key` in the mapping `node`, if it is text.
fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    entry(node, key).and_then(Node::as_str)
}

/// The value of `key` under the step's `with:`.
fn with<'a>(step: &'a Node, key: &str) -> Option<&'a str> {
    entry(step, "with").and_then(|w| text(w, key))
}

/// The word after `flag` in `words`.
fn after<'a>(words: &'a [String], flag: &str) -> Option<&'a str> {
    let at = words.iter().position(|w| w == flag)?;
    words.get(at + 1).map(String::as_str)
}

/// The problems of the gh download of job `name` at `steps[at]`, whose `run:` is `run`.
fn download_problems(name: &str, line: usize, run: &str, steps: &[Node], at: usize) -> Vec<String> {
    let who = format!("job {name}: the gh download (line {line})");
    let commands = commands(run);
    let file = match &commands[..] {
        [curl] if curl.first().is_some_and(|w| w == "curl") => after(curl, "--output")
            .and_then(|o| o.strip_prefix("$RUNNER_TEMP/"))
            .filter(|f| !f.is_empty() && !f.contains(['/', '$'])),
        _ => None,
    };
    let Some(file) = file else {
        return vec![format!(
            "{who} is not one curl with `--output \"$RUNNER_TEMP/<file>\"`"
        )];
    };
    let path = format!("${{{{ runner.temp }}}}/{file}");
    let mut problems = Vec::new();
    let step = &steps[at];
    if text(step, "id") != Some("gh-download") || text(step, "if") != Some(ON_MISS) {
        problems.push(format!("{who} lacks `id: gh-download` and `if: {ON_MISS}`"));
    }
    let pin = steps.get(at + 2).and_then(|check| check_pin(check, file));
    if pin.is_none() {
        problems.push(format!(
            "{who}: the second step after it is not the check `cargo xtask install-tool --download \"$RUNNER_TEMP/{file}\" --sha256 \"$<NAME>\"` with no `if:`"
        ));
    }
    let key = format!(
        "gh-${{{{ env.GH_VERSION }}}}-{}",
        pin.as_deref().unwrap_or("<the check's SHA-256>")
    );
    let restored = at
        .checked_sub(1)
        .map(|before| &steps[before])
        .is_some_and(|r| {
            text(r, "uses").is_some_and(|u| u.starts_with(RESTORE))
                && text(r, "id") == Some("gh-cache")
                && with(r, "path") == Some(&path)
                && (pin.is_none() || with(r, "key") == Some(&key))
        });
    if !restored {
        problems.push(format!(
            "{who}: the step before it is not `{RESTORE}` with `id: gh-cache`, `path: {path}` and `key: {key}`"
        ));
    }
    let noted = steps.get(at + 1).is_some_and(|note| {
        text(note, "if") == Some(ON_DOWNLOAD_FAILURE)
            && text(note, "run")
                .is_some_and(|r| r.starts_with(NOTE) && !r.trim_end().contains('\n'))
    });
    if !noted {
        problems.push(format!(
            "{who}: the step after it is not the note `if: {ON_DOWNLOAD_FAILURE}` with a one-line `run: {NOTE}…\"`"
        ));
    }
    let saved = steps.get(at + 3).is_some_and(|s| {
        text(s, "uses").is_some_and(|u| u.starts_with(SAVE))
            && text(s, "if") == Some(ON_MISS)
            && with(s, "path") == Some(&path)
            && with(s, "key") == Some(SAVED_KEY)
    });
    if !saved {
        problems.push(format!(
            "{who}: the third step after it is not `{SAVE}` with `if: {ON_MISS}`, `path: {path}` and `key: {SAVED_KEY}`"
        ));
    }
    problems
}

/// The expression of the SHA-256 the step `check` holds the archive `$RUNNER_TEMP/<file>` to, if
/// it is the check: one `cargo xtask install-tool` of that file with no `if:`, whose `--sha256`
/// is a variable: the step's own `env:` value of it, or else `${{ env.<NAME> }}`.
fn check_pin(check: &Node, file: &str) -> Option<String> {
    if entry(check, "if").is_some() {
        return None;
    }
    let commands = commands(text(check, "run")?);
    let [command] = &commands[..] else {
        return None;
    };
    let args = xtask_args(command)?;
    if args.first().map(String::as_str) != Some("install-tool")
        || after(args, "--download") != Some(&format!("$RUNNER_TEMP/{file}"))
    {
        return None;
    }
    let sha = after(args, "--sha256")?;
    let name = sha
        .strip_prefix("${")
        .and_then(|n| n.strip_suffix('}'))
        .or_else(|| sha.strip_prefix('$'))
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))?;
    let own = entry(check, "env").and_then(|env| text(env, name));
    Some(own.map_or_else(|| format!("${{{{ env.{name} }}}}"), str::to_owned))
}
