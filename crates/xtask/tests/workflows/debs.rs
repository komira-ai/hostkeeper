//! The rule of Debian packages in every workflow: no step runs apt, no value names one of
//! Canonical's hosts, and a job installs Debian packages only from a pinned set of
//! `.github/debs/<set>.toml`, so that no run's success hangs on Canonical's names resolving.
//!
//! - **No apt.** No command of a `run:` names one of [`APT`] as a word, its path left out,
//!   wherever it stands (`sudo -u root apt`, `timeout 60 /usr/bin/apt-get`, `bash -c "apt ..."`),
//!   but among the arguments of `cargo xtask` (`cargo xtask channels apt` runs no apt).
//! - **No Canonical host.** No value of the workflow (a `run:`, an `env:` value, an action's
//!   input) names a host of [`CANONICAL`] (`releases.ubuntu.com`, `archive.ubuntu.com`, ...). The
//!   pinned packages and the ISOs name Canonical's archive in their own files, as the first of
//!   several sources (`.github/debs/*.toml`, `packaging/netboot/pins.toml`).
//! - **Pinned packages, in four steps in a row**, `<set>` being `[a-z0-9][a-z0-9-]*`:
//!   1. `actions/cache/restore` with an `id: <id>`, no `if:`, and `with:` only
//!      `path: ${{ runner.temp }}/debs/<set>` and
//!      `key: debs-<set>-${{ runner.arch }}-${{ hashFiles('.github/debs/<set>.toml') }}` (the
//!      pins file's hash covers every pinned SHA-256);
//!   2. `run: cargo xtask debs --pins .github/debs/<set>.toml --dir "$RUNNER_TEMP/debs/<set>"`,
//!      no `if:` (it checks a restored set as it checks a fetched one);
//!   3. `actions/cache/save` with `if: ${{ steps.<id>.outputs.cache-hit != 'true' }}` and `with:`
//!      only the same `path` and `key: ${{ steps.<id>.outputs.cache-primary-key }}`: saved only
//!      on a miss, and only once the check passed (a failed step skips it);
//!   4. `run: sudo dpkg --install "$RUNNER_TEMP"/debs/<set>/*.deb`, no `if:`.
//!
//!   None of the four sets a key of [`UNDOING`] or `RUNNER_TEMP` in its `env:`. A restore of a
//!   `debs/` path that does not start the four, and a command naming `dpkg` (found as apt is)
//!   that does not end them, are refused.
//!
//! `debs_tests.rs` shows each defect refused.

use super::lint::{entries, entry, scalars};
use super::pinned_cache::UNDOING;
use super::xtask_calls::{commands, xtask_args};
use super::yaml::{Node, Value};

/// The programs that are apt.
pub const APT: [&str; 5] = [
    "apt",
    "apt-get",
    "apt-cache",
    "aptitude",
    "add-apt-repository",
];

/// Canonical's domains: a host that is one of them or ends in `.<domain>`.
pub const CANONICAL: [&str; 4] = [
    "ubuntu.com",
    "launchpad.net",
    "launchpadcontent.net",
    "canonical.com",
];

const RESTORE: &str = "actions/cache/restore@";
const SAVE: &str = "actions/cache/save@";

/// The run of the check of `set` (step 2).
pub fn debs_run(set: &str) -> String {
    format!("cargo xtask debs --pins .github/debs/{set}.toml --dir \"$RUNNER_TEMP/debs/{set}\"")
}

/// The run of the install of `set` (step 4).
pub fn install_run(set: &str) -> String {
    format!("sudo dpkg --install \"$RUNNER_TEMP\"/debs/{set}/*.deb")
}

/// The cache path of `set`.
pub fn cache_path(set: &str) -> String {
    format!("${{{{ runner.temp }}}}/debs/{set}")
}

/// The cache key of `set`.
pub fn cache_key(set: &str) -> String {
    format!("debs-{set}-${{{{ runner.arch }}}}-${{{{ hashFiles('.github/debs/{set}.toml') }}}}")
}

/// Whether `c` may stand in a host name beside a domain's letters.
fn in_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

/// Whether `text` names a host of one of [`CANONICAL`]'s domains, in any case: the domain, not
/// inside a longer name (one that only starts or ends with the domain's letters), with or
/// without the trailing `.` of a fully qualified name.
pub fn names_canonical(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    CANONICAL.iter().any(|domain| {
        text.match_indices(domain).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let mut after = text[at + domain.len()..].chars();
            let ends = match after.next() {
                None => true,
                Some('.') => after.next().is_none_or(|c| !in_name(c) && c != '.'),
                Some(c) => !in_name(c),
            };
            before.is_none_or(|c| !in_name(c)) && ends
        })
    })
}

/// Whether a command of `run` names one of `programs` as a word, its path left out
/// (`/usr/bin/apt`), wherever it stands (after `sudo -u root`, `timeout 60`, `xargs`), but among
/// the arguments of `cargo xtask`, whose verbs and values run nothing (`cargo xtask channels
/// apt`). A word that holds a space (`bash -c "apt install x"`) is read as a script too.
fn names_program(run: &str, programs: &[&str]) -> bool {
    commands(run)
        .iter()
        .filter(|command| xtask_args(command).is_none())
        .flatten()
        .any(|word| {
            let base = word.rsplit('/').next().unwrap_or(word);
            // A workflow expression (`${{ a }}`) is one word however it is read again, so a
            // word is read as a script only when that splits it.
            let splits = commands(word).concat() != [word.clone()];
            programs.contains(&base)
                || (word.contains(char::is_whitespace) && splits && names_program(word, programs))
        })
}

/// Whether `run` runs apt.
pub fn runs_apt(run: &str) -> bool {
    names_program(run, &APT)
}

/// Whether `run` runs `dpkg`.
fn runs_dpkg(run: &str) -> bool {
    names_program(run, &["dpkg"])
}

/// The value of `key` in the mapping `node`, if it is text.
fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    entry(node, key).and_then(Node::as_str)
}

/// Whether `run` calls `cargo xtask debs` in any form.
fn calls_debs(run: &str) -> bool {
    commands(run)
        .iter()
        .any(|c| xtask_args(c).is_some_and(|args| args.first().map(String::as_str) == Some("debs")))
}

/// The set of a step whose run is [`debs_run`] of a set, if it is one.
fn debs_set(step: &Node) -> Option<&str> {
    let run = text(step, "run")?.trim_end();
    let mut command = commands(run);
    let args = (command.len() == 1).then(|| command.remove(0))?;
    let args = xtask_args(&args)?;
    if args.first().map(String::as_str) != Some("debs") {
        return None;
    }
    let set = run
        .strip_prefix("cargo xtask debs --pins .github/debs/")?
        .split_once(".toml")?
        .0;
    let named = set.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && set
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    (named && run == debs_run(set)).then_some(set)
}

/// One line per problem of the Debian packages of the workflow `doc`.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let mut values = Vec::new();
    scalars(doc, &mut values);
    for value in values.iter().filter(|v| names_canonical(v)) {
        problems.push(format!(
            "`{}` names a host of Canonical's: fetch from the pinned sources of .github/debs or the ISO's mirrors",
            value.trim_end()
        ));
    }
    for (name, job) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        let steps = match entry(job, "steps").map(|s| &s.value) {
            Some(Value::Seq(steps)) => &steps[..],
            _ => &[],
        };
        problems.extend(
            job_problems(steps)
                .into_iter()
                .map(|p| format!("job {name}: {p}")),
        );
    }
    problems
}

/// The problems of the steps of one job.
fn job_problems(steps: &[Node]) -> Vec<String> {
    let mut problems = Vec::new();
    for (at, step) in steps.iter().enumerate() {
        let run = text(step, "run").unwrap_or_default();
        if runs_apt(run) {
            problems.push(format!(
                "the step `{}` runs apt: install a pinned set of .github/debs with `cargo xtask debs` and dpkg",
                run.trim_end()
            ));
        }
        if let Some(set) = debs_set(step) {
            problems.extend(four_steps(steps, at, set));
            continue;
        }
        if calls_debs(run) {
            problems.push(format!(
                "the step `{}` is not `{}` of one set",
                run.trim_end(),
                debs_run("<set>")
            ));
        }
        let is_debs = |at: usize| steps.get(at).and_then(debs_set);
        if runs_dpkg(run) && at.checked_sub(2).and_then(is_debs).is_none() {
            problems.push(format!(
                "the dpkg step `{}` does not install a pinned set: it is the fourth of restore, `cargo xtask debs`, save and install",
                run.trim_end()
            ));
        }
        let debs_path = text(step, "uses").is_some_and(|u| u.starts_with(RESTORE))
            && entry(step, "with")
                .and_then(|w| text(w, "path"))
                .is_some_and(|p| p.starts_with("${{ runner.temp }}/debs/"));
        if debs_path && is_debs(at + 1).is_none() {
            problems.push(
                "a restore of a debs/ path is not followed by its `cargo xtask debs`".to_owned(),
            );
        }
    }
    problems
}

/// The problems of the four steps of `set` whose check is `steps[at]`.
fn four_steps(steps: &[Node], at: usize, set: &str) -> Vec<String> {
    let who = format!("the pinned set {set}");
    let path = cache_path(set);
    let mut problems = Vec::new();
    let only = |step: &Node, keys: &[(&str, &str)]| {
        let with = entry(step, "with");
        with.map_or(0, |w| entries(w).len()) == keys.len()
            && keys
                .iter()
                .all(|(k, v)| with.and_then(|w| text(w, k)) == Some(*v))
    };
    let restore = at.checked_sub(1).map(|r| &steps[r]);
    let id = restore.and_then(|r| text(r, "id")).unwrap_or_default();
    let restored = restore.is_some_and(|r| {
        text(r, "uses").is_some_and(|u| u.starts_with(RESTORE))
            && !id.is_empty()
            && entry(r, "if").is_none()
            && only(r, &[("path", &path), ("key", &cache_key(set))])
    });
    if !restored {
        problems.push(format!(
            "{who}: the step before its check is not `{RESTORE}` with an `id:`, no `if:`, and `with:` only `path: {path}` and `key: {}`",
            cache_key(set)
        ));
    }
    let miss = format!("${{{{ steps.{id}.outputs.cache-hit != 'true' }}}}");
    let primary = format!("${{{{ steps.{id}.outputs.cache-primary-key }}}}");
    let saved = steps.get(at + 1).is_some_and(|s| {
        text(s, "uses").is_some_and(|u| u.starts_with(SAVE))
            && text(s, "if") == Some(&miss)
            && only(s, &[("path", &path), ("key", &primary)])
    });
    if !saved {
        problems.push(format!(
            "{who}: the step after its check is not `{SAVE}` with `if: {miss}` and `with:` only `path: {path}` and `key: {primary}`"
        ));
    }
    let install = install_run(set);
    let installed = steps.get(at + 2).is_some_and(|s| {
        text(s, "run").map(str::trim_end) == Some(&install) && entry(s, "if").is_none()
    });
    if !installed {
        problems.push(format!(
            "{who}: the second step after its check is not `run: {install}` with no `if:`"
        ));
    }
    if entry(&steps[at], "if").is_some() {
        problems.push(format!("{who}: its check has an `if:`"));
    }
    let around = [at.checked_sub(1), Some(at), Some(at + 1), Some(at + 2)];
    for step in around.into_iter().flatten().filter_map(|i| steps.get(i)) {
        let mut set_keys: Vec<&str> = UNDOING
            .into_iter()
            .filter(|k| entry(step, k).is_some())
            .collect();
        if entry(step, "env")
            .and_then(|e| entry(e, "RUNNER_TEMP"))
            .is_some()
        {
            set_keys.push("env: RUNNER_TEMP");
        }
        if !set_keys.is_empty() {
            problems.push(format!(
                "{who}: a step of its four sets `{}`",
                set_keys.join("`, `")
            ));
        }
    }
    problems
}
