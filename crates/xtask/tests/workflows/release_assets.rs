//! The rule of a release file named in a job that works on a release's commit: every file such a
//! job names in `dist/` is one the build of every commit it can check out makes. A job whose
//! last `actions/checkout` names a `ref` other than the workflow's own commit (in `release.yml`,
//! the commit of a release the run verifies or publishes, [`super::release_ref`]) holds in
//! `dist/` the files of that commit's build, downloaded from its `artifacts.yml` run; the
//! workflow file is main's tip, while the commit can be any commit newer than the newest
//! published release. A step that unpacks a file that commit's build did not make stops the job
//! (`tar: ... Cannot open`), and the release with it.
//!
//! So [`problems`] refuses, in such a job, a `run:` that names a file `dist/<name>` (the version
//! written `$RELEASE_VERSION`) unless `<name>` is:
//!
//! - an asset of a release today ([`CURRENT_FILE`], the release contract's baseline, less the
//!   names it withdrew), and an asset of [`super::release_ref::FLOOR`], the published release
//!   every commit a release run can check out is newer than ([`FLOOR_FILE`]): every commit
//!   between the two makes it, as long as no name is withdrawn and added back between them (the
//!   baseline refuses a withdrawn name added back); or
//! - an asset of a release today, named by a step that runs only under the exact `if:` of
//!   [`guard`] (the file is in `dist`) and after a step that runs `cargo xtask release check
//!   dist` ([`CHECK`]): that check is the commit's own and holds `dist` to exactly the assets of
//!   that commit, so the step runs for every commit whose build makes the file, and a commit
//!   whose contract has the file and whose build lacks it has already failed.
//!
//! A name written with anything else than `$RELEASE_VERSION` for the version is refused as
//! unread. It also refuses a `run:` that runs `$RUNNER_TEMP/binary/<program>` (where the
//! verify job unpacks the binaries it runs) unless an earlier step of the job unpacked
//! `<program>` there and the two steps run under the same `if:`: the run of a binary whose
//! tarball was not there must be skipped with its unpacking.
//!
//! [`FLOOR_FILE`] moves with [`super::release_ref::FLOOR`]: the names `gh release view` lists for
//! that release, its version written `<version>`, under the header the test of the file names.

use std::collections::BTreeSet;

use super::lint::{entries, entry};
use super::release_ref::{FLOOR, OWN_REFS};
use super::yaml::{Node, Value};

/// The asset names of [`FLOOR`], one per line, its version written `<version>`.
pub const FLOOR_FILE: &str = include_str!("release_ref_assets.txt");

/// The asset names of a release today and the names withdrawn (`withdrawn: <name>`), the release
/// contract's baseline.
pub const CURRENT_FILE: &str = include_str!("../release/asset_names.baseline");

/// The one way a name of `dist/` writes the version: the verify and publish jobs set
/// `RELEASE_VERSION` to the release's.
const VERSION_WORDS: [&str; 2] = ["${RELEASE_VERSION}", "$RELEASE_VERSION"];

/// The directory the verify job unpacks the binaries it runs into, with the `/` before a name.
pub const BINARY_DIR: &str = "$RUNNER_TEMP/binary/";

/// How the step that holds `dist` to the released commit's own assets starts.
pub const CHECK: &str = "cargo xtask release check dist ";

/// The `if:` of a step that names the file `dist/<name>`, which [`FLOOR`] does not have: the
/// file is in `dist`.
pub fn guard(name: &str) -> String {
    format!(
        "${{{{ hashFiles(format('dist/{}', env.RELEASE_VERSION)) != '' }}}}",
        name.replace("<version>", "{0}")
    )
}

/// The names of a list of assets: every line that is not empty or a `#` comment, but the
/// withdrawn ones.
fn names(text: &str) -> BTreeSet<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("withdrawn:"))
        .collect()
}

fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    entry(node, key).and_then(Node::as_str)
}

/// The words of `run` that follow `prefix` where it starts a word (at the start, or after a
/// space, a quote or `=`), each up to the next space, quote, `;`, `|` or `)`.
fn named_after<'a>(run: &'a str, prefix: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    for (at, _) in run.match_indices(prefix) {
        let starts = run[..at]
            .chars()
            .next_back()
            .is_none_or(|c| matches!(c, ' ' | '"' | '\'' | '=' | '\n'));
        if !starts {
            continue;
        }
        let rest = &run[at + prefix.len()..];
        let end = rest
            .find([' ', '"', '\'', ';', '|', ')', '\n'])
            .unwrap_or(rest.len());
        if end > 0 {
            found.push(&rest[..end]);
        }
    }
    found
}

/// The name `dist/<word>` stands for, its version written `<version>`.
fn asset_of(word: &str) -> String {
    VERSION_WORDS
        .iter()
        .fold(word.to_owned(), |name, v| name.replace(v, "<version>"))
}

/// The program a `tar -xzf ... -C "$RUNNER_TEMP/binary" <program>` step unpacks: its last word.
fn unpacked(run: &str) -> Option<&str> {
    let line = run.trim();
    let into = format!("-C \"{}\" ", BINARY_DIR.trim_end_matches('/'));
    if !line.starts_with("tar -xzf ") || !line.contains(&into) {
        return None;
    }
    line.rsplit(' ').next()
}

/// What one job of [`problems`] has seen so far.
#[derive(Default)]
struct Seen {
    /// The job's last checkout is of a release's commit.
    release: bool,
    /// A step after that checkout ran [`CHECK`].
    checked: bool,
    /// Each program unpacked into [`BINARY_DIR`], with the `if:` of its step.
    unpacked: Vec<(String, Option<String>)>,
}

/// One line per file of `dist/` that a job of the workflow `doc` (the file `file`) on a release's
/// commit names and the build of such a commit may not have made, and per binary run without its
/// unpacking (the module says which).
pub fn problems(file: &str, doc: &Node) -> Vec<String> {
    let floor = names(FLOOR_FILE);
    let current = names(CURRENT_FILE);
    let mut out = Vec::new();
    let Some(jobs) = entry(doc, "jobs") else {
        return out;
    };
    for (name, job) in entries(jobs) {
        let job_name = name.as_str().unwrap_or_default();
        let steps = match entry(job, "steps").map(|s| &s.value) {
            Some(Value::Seq(items)) => &items[..],
            _ => continue,
        };
        let mut seen = Seen::default();
        for (at, step) in steps.iter().enumerate() {
            let at = format!("{file}: job {job_name} step {}", at + 1);
            if text(step, "uses").is_some_and(|u| u.starts_with("actions/checkout@")) {
                let reference = entry(step, "with").and_then(|w| text(w, "ref"));
                seen = Seen {
                    release: reference.is_some_and(|r| !OWN_REFS.contains(&r)),
                    ..Seen::default()
                };
                continue;
            }
            let Some(run) = text(step, "run").filter(|_| seen.release) else {
                continue;
            };
            let condition = text(step, "if");
            for word in named_after(run, "dist/") {
                let name = asset_of(word);
                if name.contains(['$', '{', '`']) {
                    out.push(format!(
                        "{at} names dist/{word}, a name this rule cannot read (the version is written $RELEASE_VERSION)"
                    ));
                } else if !current.contains(name.as_str()) {
                    out.push(format!(
                        "{at} names dist/{name}, which is not an asset of a release (crates/xtask/tests/release/asset_names.baseline)"
                    ));
                } else if !floor.contains(name.as_str())
                    && (condition != Some(guard(&name).as_str()) || !seen.checked)
                {
                    out.push(format!(
                        "{at} names dist/{name}, which release {} does not have, so the build of an older commit a release can be of did not make it: the step runs only under `if: {}`, after a step that runs `{}`",
                        FLOOR.0,
                        guard(&name),
                        CHECK.trim_end()
                    ));
                }
            }
            if run.contains(CHECK) {
                seen.checked = true;
            }
            for program in named_after(run, BINARY_DIR) {
                let unpacking = seen.unpacked.iter().find(|(p, _)| p == program);
                match unpacking {
                    None => out.push(format!(
                        "{at} runs {BINARY_DIR}{program}, which no step before it unpacks"
                    )),
                    Some((_, unpacked_if)) if unpacked_if.as_deref() != condition => {
                        out.push(format!(
                            "{at} runs {BINARY_DIR}{program} under `if: {}`, not the `if:` of the step that unpacks it, `{}`",
                            condition.unwrap_or("(none)"),
                            unpacked_if.as_deref().unwrap_or("(none)")
                        ));
                    }
                    Some(_) => {}
                }
            }
            if let Some(program) = unpacked(run) {
                seen.unpacked
                    .push((program.to_owned(), condition.map(str::to_owned)));
            }
        }
    }
    out
}
