//! The rule of every pinned download of a workflow: the job takes the file from the actions
//! cache when a run before it cached that version and SHA-256, downloads it from GitHub only when
//! none did, checks it against the pin whichever way it came, and caches it once the check
//! passed. So a job whose pins are cached runs through an outage of GitHub's release downloads,
//! and one whose pin is not cached fails and says why in one line.
//!
//! A pinned download is a `curl` to `"$RUNNER_TEMP/<file>"` whose file a pin check of the same
//! job reads: a step whose `run:` is one
//! `cargo xtask install-tool --download "$RUNNER_TEMP/<file>" --sha256 "$<PIN>"` (a check of a
//! file no `curl` of the job writes, such as a trusted root gh prints, is not one). Every `curl`
//! that reaches a release asset ([`RELEASES`] in one of its words, or in the value a word's whole
//! `$NAME` takes from the step's, the job's or the workflow's `env:`), however it names its
//! output, is the `curl` of a pinned download, but one of this repository's own releases
//! ([`OWN`]: a release a workflow publishes or picks by its tag, checked against that release's
//! own `SHA256SUMS` and attestations; its pinned previous release is held by its check).
//! The check is the fourth of five steps in a row, each held to its shape, and none of them sets
//! a key that would undo it ([`UNDOING`], or `RUNNER_TEMP` in its `env:`); `<stem>` is `<file>`
//! up to its first `.` (`gh` for `gh.tar.gz`), naming the steps' ids:
//!
//! 1. `actions/cache/restore` with `id: <stem>-cache`, the check's own `if:`,
//!    `path: ${{ runner.temp }}/<file>` and `key: pinned-<stem>-<version>-<pin>`. `<version>` is
//!    the expression of the first variable of the download's URL, its last word
//!    (`${{ env.GH_VERSION }}` for `$GH_VERSION`, or the value of the download step's own `env:`
//!    entry of that name), and
//!    `<pin>` that of the check's `--sha256` the same way, from the check step's `env:`.
//! 2. The download: one `curl` with `--output "$RUNNER_TEMP/<file>"`, `id: <stem>-download` and
//!    `if:` [`on_miss`].
//! 3. The note: `if:` [`on_download_failure`] and a `run:` of one line, `echo "::error::…"`. It
//!    runs only when the download ran and failed.
//! 4. The check, with `id: <stem>-check` and no `if:`, or, in a job whose steps go on after a
//!    failed one (ci.yml's `lint`), `if:` [`GO_ON`]: it runs on a cache hit as on a download, and
//!    nothing uses the file before it.
//! 5. `actions/cache/save` with `if:` [`on_checked`], the same `path`, and `key:` the restore's
//!    primary key: only a file the check passed is cached, and only on a miss.
//!
//! `pinned_cache_tests.rs` shows each defect refused, and runs the conditions of the
//! repository's own steps against every outcome of the cache, the download, the check and the
//! steps before them.

use super::lint::{entries, entry};
use super::xtask_calls::{commands, xtask_args};
use super::yaml::{Node, Value};

/// What a URL of a GitHub release asset holds.
pub const RELEASES: &str = "/releases/download/";

/// The `if:` of the steps of a job that goes on after a failed step.
pub const GO_ON: &str = "${{ !cancelled() }}";

/// What a URL of one of this repository's own releases holds.
pub const OWN: [&str; 2] = [
    "github.com/komira-ai/hostkeeper/releases/download/",
    "github.com/$GITHUB_REPOSITORY/releases/download/",
];

/// The step keys none of the five steps may set: each can let a failed check pass, or run a step
/// other than as written.
pub const UNDOING: [&str; 3] = ["continue-on-error", "shell", "working-directory"];

const RESTORE: &str = "actions/cache/restore@";
const SAVE: &str = "actions/cache/save@";
const NOTE: &str = "echo \"::error::";

/// The `if:` of the download: the cache held no file under the key (and, with `go_on`, though
/// a step before failed).
pub fn on_miss(stem: &str, go_on: bool) -> String {
    let miss = format!("steps.{stem}-cache.outputs.cache-hit != 'true'");
    if go_on {
        format!("${{{{ !cancelled() && {miss} }}}}")
    } else {
        format!("${{{{ {miss} }}}}")
    }
}

/// The `if:` of the note: the download ran and failed.
pub fn on_download_failure(stem: &str) -> String {
    format!("${{{{ !cancelled() && steps.{stem}-download.outcome == 'failure' }}}}")
}

/// The `if:` of the save: the check passed a file the cache did not hold.
pub fn on_checked(stem: &str) -> String {
    format!(
        "${{{{ !cancelled() && steps.{stem}-check.outcome == 'success' && steps.{stem}-cache.outputs.cache-hit != 'true' }}}}"
    )
}

/// The `key:` of the save: the key the restore looked up.
pub fn saved_key(stem: &str) -> String {
    format!("${{{{ steps.{stem}-cache.outputs.cache-primary-key }}}}")
}

/// One line per problem of the pinned downloads of the workflow `doc`.
pub fn problems(doc: &Node) -> Vec<String> {
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    let mut problems = Vec::new();
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or_default();
        let steps = match entry(job, "steps").map(|s| &s.value) {
            Some(Value::Seq(steps)) => &steps[..],
            _ => &[],
        };
        let written: Vec<String> = steps.iter().filter_map(download_file).collect();
        let mut downloads = Vec::new();
        for (at, step) in steps.iter().enumerate() {
            if let Some(file) = checked_file(step).filter(|f| written.contains(f)) {
                let found = at.checked_sub(2).map(|d| &steps[d]).and_then(download_file);
                if found.as_deref() == Some(file.as_str()) {
                    downloads.push(at - 2);
                }
                problems.extend(check_problems(name, steps, at, &file));
            }
        }
        for (at, step) in steps.iter().enumerate() {
            let releases = text(step, "run").is_some_and(|r| {
                commands(r).iter().any(|c| {
                    c.first().is_some_and(|w| w == "curl")
                        && c.iter().any(|w| reaches_release(w, &[step, job, doc]))
                })
            });
            if releases && !downloads.contains(&at) {
                let line = entry(step, "run").map_or(0, Node::line);
                problems.push(format!(
                    "job {name}: the release download (line {line}) is not the download of a pinned check: put `cargo xtask install-tool --download \"$RUNNER_TEMP/<file>\" --sha256 \"$<PIN>\"` two steps after it, in the five steps of a pinned download"
                ));
            }
        }
    }
    problems
}

/// Whether the word `word` of a `curl` names a release asset other than one of [`OWN`]: in
/// itself, or, when it is one whole `$NAME`, in that variable's value under the `env:` of the
/// first of `scopes` (the step, the job, the workflow) that sets it.
fn reaches_release(word: &str, scopes: &[&Node]) -> bool {
    let name = word
        .strip_prefix("${")
        .and_then(|n| n.strip_suffix('}'))
        .or_else(|| word.strip_prefix('$'));
    let value = name
        .and_then(|n| {
            scopes
                .iter()
                .find_map(|scope| entry(scope, "env").and_then(|env| text(env, n)))
        })
        .unwrap_or(word);
    value.contains(RELEASES) && !OWN.iter().any(|own| value.contains(own))
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

/// `<file>` of a `"$RUNNER_TEMP/<file>"` word: one plain file name.
fn temp_file(word: &str) -> Option<&str> {
    word.strip_prefix("$RUNNER_TEMP/")
        .filter(|f| !f.is_empty() && !f.contains(['/', '$']))
}

/// The words of the one command of the step's `run:`.
fn one_command(step: &Node) -> Option<Vec<String>> {
    let mut commands = commands(text(step, "run")?);
    (commands.len() == 1).then(|| commands.remove(0))
}

/// The file the step checks against a pin, if it is a `cargo xtask install-tool` of
/// `"$RUNNER_TEMP/<file>"`.
fn checked_file(step: &Node) -> Option<String> {
    let command = one_command(step)?;
    let args = xtask_args(&command)?;
    if args.first().map(String::as_str) != Some("install-tool") {
        return None;
    }
    temp_file(after(args, "--download")?).map(str::to_owned)
}

/// The file the step downloads, if it is one `curl` to `"$RUNNER_TEMP/<file>"`.
fn download_file(step: &Node) -> Option<String> {
    let command = one_command(step)?;
    if command.first().map(String::as_str) != Some("curl") {
        return None;
    }
    temp_file(after(&command, "--output")?).map(str::to_owned)
}

/// The expression of the variable `word` names (`$NAME`, `${NAME}`, or the first `$NAME` in
/// it): the step's own `env:` value of it, or else `${{ env.<NAME> }}`.
fn variable(step: &Node, word: &str, whole: bool) -> Option<String> {
    let at = word.find('$')?;
    if whole && at != 0 {
        return None;
    }
    let rest = &word[at + 1..];
    let (name, after) = match rest.strip_prefix('{') {
        Some(braced) => {
            let end = braced.find('}')?;
            (&braced[..end], &braced[end + 1..])
        }
        None => {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        }
    };
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid || (whole && !after.is_empty()) {
        return None;
    }
    let own = entry(step, "env").and_then(|env| text(env, name));
    Some(own.map_or_else(|| format!("${{{{ env.{name} }}}}"), str::to_owned))
}

/// The problems of the pinned download whose check is `steps[at]`, of `"$RUNNER_TEMP/<file>"`.
fn check_problems(name: &str, steps: &[Node], at: usize, file: &str) -> Vec<String> {
    let check = &steps[at];
    let line = entry(check, "run").map_or(0, Node::line);
    let who = format!("job {name}: the check of {file} (line {line})");
    let stem = file.split('.').next().unwrap_or(file);
    let path = format!("${{{{ runner.temp }}}}/{file}");
    let mut problems = Vec::new();
    let guard = text(check, "if");
    let go_on = guard == Some(GO_ON);
    if guard.is_some() && !go_on {
        problems.push(format!("{who} has an `if:` other than `{GO_ON}`"));
    }
    if text(check, "id") != Some(&format!("{stem}-check")) {
        problems.push(format!("{who} lacks `id: {stem}-check`"));
    }
    let pin = one_command(check)
        .and_then(|c| {
            xtask_args(&c)
                .and_then(|a| after(a, "--sha256"))
                .map(str::to_owned)
        })
        .and_then(|sha| variable(check, &sha, true));
    if pin.is_none() {
        problems.push(format!("{who}: its `--sha256` is not one variable"));
    }
    let download = at.checked_sub(2).map(|d| &steps[d]);
    let curl = download.filter(|d| download_file(d).as_deref() == Some(file));
    let version = curl.and_then(|d| {
        let command = one_command(d)?;
        variable(d, command.last()?, false)
    });
    match curl {
        None => problems.push(format!(
            "{who}: the second step before it is not one `curl` with `--output \"$RUNNER_TEMP/{file}\"`"
        )),
        Some(d) => {
            let miss = on_miss(stem, go_on);
            if text(d, "id") != Some(&format!("{stem}-download")) || text(d, "if") != Some(&miss)
            {
                problems.push(format!(
                    "{who}: its download lacks `id: {stem}-download` and `if: {miss}`"
                ));
            }
            if version.is_none() {
                problems.push(format!("{who}: its download's URL names no variable"));
            }
        }
    }
    let key = format!(
        "pinned-{stem}-{}-{}",
        version.as_deref().unwrap_or("<version>"),
        pin.as_deref().unwrap_or("<pin>")
    );
    let restored = at.checked_sub(3).map(|r| &steps[r]).is_some_and(|r| {
        text(r, "uses").is_some_and(|u| u.starts_with(RESTORE))
            && text(r, "id") == Some(&format!("{stem}-cache"))
            && text(r, "if") == guard
            && entry(r, "with").map_or(0, |w| entries(w).len()) == 2
            && with(r, "path") == Some(&path)
            && (version.is_none() || pin.is_none() || with(r, "key") == Some(&key))
    });
    if !restored {
        let guarded = guard.map_or(String::new(), |g| format!("`if: {g}`, "));
        problems.push(format!(
            "{who}: the third step before it is not `{RESTORE}` with `id: {stem}-cache`, {guarded}and `with:` only `path: {path}` and `key: {key}`"
        ));
    }
    let failure = on_download_failure(stem);
    let noted = at.checked_sub(1).map(|n| &steps[n]).is_some_and(|n| {
        text(n, "if") == Some(&failure)
            && text(n, "run").is_some_and(|r| r.starts_with(NOTE) && !r.trim_end().contains('\n'))
    });
    if !noted {
        problems.push(format!(
            "{who}: the step before it is not the note `if: {failure}` with a one-line `run: {NOTE}…\"`"
        ));
    }
    for (role, i) in [("restore", 3), ("download", 2), ("note", 1), ("check", 0)]
        .into_iter()
        .filter_map(|(role, back)| at.checked_sub(back).map(|i| (role, i)))
        .chain(Some(("save", at + 1)).filter(|(_, i)| *i < steps.len()))
    {
        let step = &steps[i];
        let mut set: Vec<&str> = UNDOING
            .into_iter()
            .filter(|k| entry(step, k).is_some())
            .collect();
        if entry(step, "env")
            .and_then(|e| entry(e, "RUNNER_TEMP"))
            .is_some()
        {
            set.push("env: RUNNER_TEMP");
        }
        if !set.is_empty() {
            problems.push(format!(
                "{who}: its {role} step sets `{}`",
                set.join("`, `")
            ));
        }
    }
    let checked = on_checked(stem);
    let key = saved_key(stem);
    let saved = steps.get(at + 1).is_some_and(|s| {
        text(s, "uses").is_some_and(|u| u.starts_with(SAVE))
            && text(s, "if") == Some(&checked)
            && entry(s, "with").map_or(0, |w| entries(w).len()) == 2
            && with(s, "path") == Some(&path)
            && with(s, "key") == Some(&key)
    });
    if !saved {
        problems.push(format!(
            "{who}: the step after it is not `{SAVE}` with `if: {checked}` and `with:` only `path: {path}` and `key: {key}`"
        ));
    }
    problems
}
