//! The rule of a `cargo xtask` call that runs from a checkout of another commit than the
//! workflow's own. A step runs the xtask of the job's last `actions/checkout` before it. A
//! checkout with no `ref`, or `ref: ${{ github.sha }}`, is the commit the workflow file is read
//! from, whose command line [`super::xtask_calls`] holds every call to. Any other `ref` (in
//! `release.yml`, the commit of a release the run publishes) may name a commit older than the
//! workflow: a release run releases the commits of main since the newest published release,
//! while the workflow file is main's tip. Such a commit's xtask lacks every verb and flag added
//! after it, and a call of one stops the job (`unrecognized subcommand`).
//!
//! So [`problems`] refuses a workflow when a call under such a checkout would be refused by the
//! command line of [`FLOOR`], a published release, as [`SURFACE_FILE`] records it: a verb it does
//! not have, a flag it does not have, a required flag or argument left out, or more arguments
//! than it takes. Every commit a release run can check out is newer than the newest published
//! release, so newer than [`FLOOR`], and holds what [`FLOOR`] holds as long as no verb or flag is
//! removed and added back between the two (a removal alone is refused by
//! [`super::xtask_calls`], which holds the call to the workflow's own commit too). Each
//! alternate-value expansion (`${FAIL_READY:+--fail-ready}`) is checked both ways, and a call
//! with `--help` runs nothing and passes. A value is not checked against the floor: its parser
//! is the workflow commit's, through [`super::xtask_calls`].
//!
//! It also refuses, in a job that calls xtask, a checkout of another repository or into a
//! `path`: a later call would not run the checkout this rule reads.
//!
//! What the floor's command line records, and so what is checked, is each verb's flags (a short
//! form as an alias of its long one), whether each takes a value or is always required, and its
//! positional arguments. Not recorded, so not checked: a requirement that depends on another
//! flag (`required_unless_present`, `conflicts_with`), a global flag, a flag of several values,
//! and a short flag with its value attached (`-sVALUE`); xtask's own command line uses none of
//! the last three. A checkout under an `if:` is taken as run, and a `working-directory` is not
//! read.
//!
//! A job that needs a verb newer than [`FLOOR`] runs it from a checkout of the workflow's own
//! commit, or waits
//! until a release that has the verb is published and [`FLOOR`] and [`SURFACE_FILE`] move to it:
//! [`surface`] of that release's `xtask::test_support::cli()`, run from a checkout of its tag,
//! with the header line the test of the file names.

use clap::{ArgAction, Command};

use super::downloads::collect_runs;
use super::lint::{entries, entry};
use super::xtask_calls::{commands, variants, xtask_args};
use super::yaml::{Node, Value};

/// The published release whose command line [`SURFACE_FILE`] records: its version and commit.
pub const FLOOR: (&str, &str) = ("0.1.208", "22880ebf409a35539d7ebe66abb80a4277119880");

/// The command line of [`FLOOR`]'s xtask, as [`surface`] writes it.
pub const SURFACE_FILE: &str = include_str!("release_ref_surface.txt");

/// A `ref` of a checkout that is the workflow's own commit.
pub const OWN_REFS: [&str; 1] = ["${{ github.sha }}"];

/// One argument of a verb in [`SURFACE_FILE`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Arg {
    /// `--name` (no value) or `--name=` (a value), `!` when required.
    Flag {
        name: String,
        takes_value: bool,
        required: bool,
    },
    /// `<name>`, `!` when required, `...` when it takes any number of words.
    Positional { required: bool, many: bool },
}

/// The command line of `cmd`: one line per verb that has no verbs under it, its words, then each
/// flag sorted by name and each positional argument in order, as [`Arg`] writes them.
pub fn surface(cmd: &Command) -> String {
    let mut lines = Vec::new();
    leaves(cmd, &mut Vec::new(), &mut lines);
    lines.sort();
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn leaves(cmd: &Command, path: &mut Vec<String>, out: &mut Vec<String>) {
    let subs: Vec<&Command> = cmd
        .get_subcommands()
        .filter(|s| s.get_name() != "help")
        .collect();
    if subs.is_empty() {
        if path.is_empty() {
            return;
        }
        let mut flags = Vec::new();
        let mut positionals = Vec::new();
        for a in cmd.get_arguments() {
            let builtin = matches!(
                a.get_action(),
                ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
            );
            if builtin || a.is_global_set() {
                continue;
            }
            let required = if a.is_required_set() { "!" } else { "" };
            if a.is_positional() {
                let many = a.get_num_args().is_some_and(|n| n.max_values() > 1);
                let many = if many { "..." } else { "" };
                positionals.push(format!("<{}>{required}{many}", a.get_id()));
                continue;
            }
            let value = if a.get_action().takes_values() {
                "="
            } else {
                ""
            };
            if let Some(long) = a.get_long() {
                flags.push(format!("--{long}{value}{required}"));
            }
            // A short form is an alias: the long one, when there is one, carries the requirement.
            if let Some(short) = a.get_short() {
                let required = if a.get_long().is_some() { "" } else { required };
                flags.push(format!("-{short}{value}{required}"));
            }
        }
        flags.sort();
        let words = path.iter().cloned().chain(flags).chain(positionals);
        out.push(words.collect::<Vec<_>>().join(" "));
        return;
    }
    for sub in subs {
        path.push(sub.get_name().to_owned());
        leaves(sub, path, out);
        path.pop();
    }
}

/// The verbs of a surface text, each its words and its arguments.
fn parse_surface(text: &str) -> Vec<(Vec<String>, Vec<Arg>)> {
    let mut out = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut verb = Vec::new();
        let mut args = Vec::new();
        for word in line.split(' ') {
            if let Some(flag) = word.strip_prefix('-') {
                let (flag, required) = flag.strip_suffix('!').map_or((flag, false), |f| (f, true));
                let (flag, takes_value) =
                    flag.strip_suffix('=').map_or((flag, false), |f| (f, true));
                args.push(Arg::Flag {
                    name: format!("-{flag}"),
                    takes_value,
                    required,
                });
            } else if word.starts_with('<') {
                let (rest, many) = word
                    .strip_suffix("...")
                    .map_or((word, false), |w| (w, true));
                args.push(Arg::Positional {
                    required: rest.ends_with('!'),
                    many,
                });
            } else {
                verb.push(word.to_owned());
            }
        }
        out.push((verb, args));
    }
    out
}

/// Why `args`, the words after `cargo xtask`, would be refused by the command line `verbs`, in
/// one of their alternate-value variants, if they would.
pub fn floor_problem(verbs: &str, args: &[String]) -> Option<String> {
    let verbs = parse_surface(verbs);
    variants(args).into_iter().find_map(|(args, _)| {
        if args.iter().any(|w| w == "--help" || w == "-h") {
            return None;
        }
        let Some((verb, spec)) = verbs
            .iter()
            .filter(|(verb, _)| args.starts_with(verb))
            .max_by_key(|(verb, _)| verb.len())
        else {
            let shown: Vec<String> = args
                .iter()
                .take_while(|w| !w.starts_with('-'))
                .cloned()
                .collect();
            if shown.is_empty() {
                return Some(format!(
                    "`cargo xtask` names no verb of release {}",
                    FLOOR.0
                ));
            }
            return Some(format!(
                "`cargo xtask {}` is not a verb of release {}",
                shown.join(" "),
                FLOOR.0
            ));
        };
        let shown = format!("`cargo xtask {}`", verb.join(" "));
        let mut given = Vec::new();
        let mut positionals = 0;
        let mut words = args[verb.len()..].iter();
        while let Some(word) = words.next() {
            if !word.starts_with('-') || word == "-" {
                positionals += 1;
                continue;
            }
            let (name, inline) = word
                .split_once('=')
                .map_or((&word[..], false), |(n, _)| (n, true));
            let found = spec.iter().find_map(|a| match a {
                Arg::Flag {
                    name: n,
                    takes_value,
                    ..
                } if n == name => Some(*takes_value),
                _ => None,
            });
            match found {
                None => {
                    return Some(format!(
                        "{shown} of release {} has no flag `{name}`",
                        FLOOR.0
                    ));
                }
                Some(true) if !inline => {
                    words.next();
                }
                Some(_) => {}
            }
            given.push(name.to_owned());
        }
        for a in spec {
            if let Arg::Flag {
                name,
                required: true,
                ..
            } = a
                && !given.contains(name)
            {
                return Some(format!("{shown} of release {} requires `{name}`", FLOOR.0));
            }
        }
        let takes: Vec<(bool, bool)> = spec
            .iter()
            .filter_map(|a| match a {
                Arg::Positional { required, many } => Some((*required, *many)),
                Arg::Flag { .. } => None,
            })
            .collect();
        let needed = takes.iter().filter(|(required, _)| *required).count();
        let most = if takes.iter().any(|(_, many)| *many) {
            usize::MAX
        } else {
            takes.len()
        };
        if positionals < needed || positionals > most {
            return Some(format!(
                "{shown} of release {} takes {needed} to {} arguments, not {positionals}",
                FLOOR.0,
                if most == usize::MAX {
                    "any number of".to_owned()
                } else {
                    most.to_string()
                }
            ));
        }
        None
    })
}

fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    entry(node, key).and_then(Node::as_str)
}

/// What a job's workspace holds after a step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Workspace {
    /// The workflow's own commit; before any checkout too, since a call there runs no xtask
    /// of another commit.
    Own,
    /// Another commit of the repository, by this `ref`.
    Other(String),
    /// A checkout this rule does not read: another repository, or into a `path`.
    Unread,
}

/// One line per call of xtask in the workflow `doc` (the file `file`) that the commit its job
/// checked out may not have, and per checkout or call outside the rule (the module says which).
pub fn problems(file: &str, doc: &Node) -> Vec<String> {
    let mut out = Vec::new();
    let Some(jobs) = entry(doc, "jobs") else {
        return out;
    };
    for (name, job) in entries(jobs) {
        let name = name.as_str().unwrap_or_default();
        let steps = match entry(job, "steps").map(|s| &s.value) {
            Some(Value::Seq(items)) => &items[..],
            _ => continue,
        };
        let calls_xtask = steps.iter().any(|s| !calls(s).is_empty());
        let mut workspace = Workspace::Own;
        for (at, step) in steps.iter().enumerate() {
            let n = at + 1;
            if text(step, "uses").is_some_and(|u| u.starts_with("actions/checkout@")) {
                let with = entry(step, "with");
                let of = |key| with.and_then(|w| text(w, key));
                workspace = if of("repository").is_some() || of("path").is_some() {
                    if calls_xtask {
                        out.push(format!(
                            "{file}: job {name} step {n} checks out another repository or into a path, in a job that calls xtask"
                        ));
                    }
                    Workspace::Unread
                } else {
                    match of("ref") {
                        None => Workspace::Own,
                        Some(r) if OWN_REFS.contains(&r) => Workspace::Own,
                        Some(r) => Workspace::Other(r.to_owned()),
                    }
                };
                continue;
            }
            for (line, args) in calls(step) {
                match &workspace {
                    Workspace::Own | Workspace::Unread => {}
                    Workspace::Other(r) => {
                        if let Some(problem) = floor_problem(SURFACE_FILE, &args) {
                            out.push(format!(
                                "{file}: job {name} step {n} (line {line}) runs the xtask of `{r}`, which may be as old as release {}: {problem}",
                                FLOOR.0
                            ));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Each call of xtask in the `run:` of `step`, with its line and its words after `xtask`.
pub fn calls(step: &Node) -> Vec<(usize, Vec<String>)> {
    let mut runs = Vec::new();
    collect_runs(step, &mut runs);
    let mut out = Vec::new();
    for (line, run) in runs {
        for command in commands(run) {
            if let Some(args) = xtask_args(&command) {
                out.push((line, args.to_vec()));
            }
        }
    }
    out
}
