//! The package scripts, read and run in the small part of POSIX sh they are written in, with
//! no shell.
//!
//! A script starts with `#!/bin/sh`. Every other line is blank, a `#` comment, `set -e`, one
//! or more simple commands joined by `||` or `&&`, or a `case` on `"$1"` or `"$1:$2"` whose arms
//! are each one line, `pattern | pattern) commands ;;`, closed by `esac`. A word of a command is
//! letters, digits and `._/:=+-` only, so there is no variable, substitution, glob, quote,
//! redirection, pipe, background job, subshell, test (`[`, `[[`) or function in a command.
//! Anything else is refused with its line.
//!
//! [`run`] runs the commands as sh does, with the package manager's arguments and a fake in
//! place of every program, so a test sees which programs a script runs and its exit status.

/// How a command is joined to the one before it on its line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    /// The first command of its line.
    Start,
    /// `||`: runs when the command before it failed.
    Or,
    /// `&&`: runs when the command before it succeeded.
    And,
}

/// The `case` arm a command is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arm {
    /// Which `case` of the script, from 0.
    pub case: usize,
    /// What it tests: `"$1"` or `"$1:$2"`.
    pub subject: String,
    /// Which arm of that `case`, from 0.
    pub index: usize,
    pub patterns: Vec<String>,
}

/// A simple command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub words: Vec<String>,
    pub join: Join,
    /// The patterns of the `case` arm it runs under, if any.
    pub arm: Option<Vec<String>>,
    pub case: Option<Arm>,
    /// 1-based.
    pub line: usize,
}

const SHEBANG: &str = "#!/bin/sh";

/// The subjects a `case` may test: the arguments the package manager passes.
const SUBJECTS: [&str; 2] = ["\"$1\"", "\"$1:$2\""];

fn is_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/:=+-".contains(&b))
}

fn is_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":-".contains(&b))
}

/// The simple commands of `text` (commands joined by `||` or `&&`), at line `n`.
fn commands(text: &str, n: usize, arm: Option<&Arm>) -> Result<Vec<Command>, String> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut join = Join::Start;
    loop {
        let next = [("||", Join::Or), ("&&", Join::And)]
            .into_iter()
            .filter_map(|(op, kind)| rest.find(op).map(|at| (at, kind)))
            .min_by_key(|(at, _)| *at);
        let (part, following) = match next {
            Some((at, kind)) => (&rest[..at], Some((at, kind))),
            None => (rest, None),
        };
        let words: Vec<String> = part.split_whitespace().map(str::to_owned).collect();
        if words.is_empty() {
            return Err(format!("line {n}: an empty command"));
        }
        if let Some(bad) = words.iter().find(|w| !is_word(w)) {
            return Err(format!(
                "line {n}: the word {bad:?} is outside the script grammar (letters, digits and `._/:=+-`)"
            ));
        }
        out.push(Command {
            words,
            join,
            arm: arm.map(|a| a.patterns.clone()),
            case: arm.cloned(),
            line: n,
        });
        match following {
            Some((at, kind)) => {
                rest = &rest[at + 2..];
                join = kind;
            }
            None => return Ok(out),
        }
    }
}

/// Reads a script: its commands in order. See the module for the grammar.
pub fn parse(text: &str) -> Result<Vec<Command>, String> {
    let mut lines = text.lines().enumerate();
    match lines.next() {
        Some((_, first)) if first == SHEBANG => {}
        _ => {
            return Err(format!(
                "line 1: the script does not start with `{SHEBANG}`"
            ));
        }
    }
    let mut out = Vec::new();
    // The open `case`: its number, its subject and the number of arms read so far.
    let mut open: Option<(usize, String, usize)> = None;
    let mut cases = 0;
    for (at, raw) in lines {
        let n = at + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(subject) = line
            .strip_prefix("case ")
            .and_then(|rest| rest.strip_suffix(" in"))
        {
            if open.is_some() {
                return Err(format!("line {n}: a `case` inside a `case`"));
            }
            let subject = subject.trim();
            if !SUBJECTS.contains(&subject) {
                return Err(format!(
                    "line {n}: a `case` on {subject:?}; only \"$1\" and \"$1:$2\" are tested"
                ));
            }
            open = Some((cases, subject.to_owned(), 0));
            cases += 1;
        } else if line == "esac" {
            if open.take().is_none() {
                return Err(format!("line {n}: an `esac` with no `case`"));
            }
        } else if let Some((case, subject, arms)) = open.as_mut() {
            let (patterns, rest) = line
                .split_once(')')
                .ok_or_else(|| format!("line {n}: a `case` arm without `)`"))?;
            let body = rest
                .trim()
                .strip_suffix(";;")
                .ok_or_else(|| format!("line {n}: a `case` arm that does not end in `;;`"))?;
            let patterns: Vec<String> = patterns.split('|').map(|p| p.trim().to_owned()).collect();
            if let Some(bad) = patterns.iter().find(|p| !is_pattern(p)) {
                return Err(format!(
                    "line {n}: the pattern {bad:?} is outside the script grammar"
                ));
            }
            let arm = Arm {
                case: *case,
                subject: subject.clone(),
                index: *arms,
                patterns,
            };
            *arms += 1;
            out.extend(commands(body, n, Some(&arm))?);
        } else {
            out.extend(commands(line, n, None)?);
        }
    }
    if open.is_some() {
        return Err("a `case` is not closed by `esac`".to_owned());
    }
    Ok(out)
}

/// What running a script did.
#[derive(Debug, PartialEq, Eq)]
pub struct Run {
    /// The programs it ran, in order (not the shell's own `set`, `true` and `exit`).
    pub ran: Vec<Vec<String>>,
    /// Its exit status.
    pub status: i32,
}

/// The value of a `case` subject with the positional arguments `args` (a missing one is empty).
fn expand(subject: &str, args: &[&str]) -> String {
    let arg = |i: usize| args.get(i).copied().unwrap_or_default();
    match subject {
        "\"$1\"" => arg(0).to_owned(),
        _ => format!("{}:{}", arg(0), arg(1)),
    }
}

/// Whether the arm of `command` is the one its `case` runs: the first arm with a pattern equal
/// to the subject (the grammar has no globs). A command outside a `case` always runs.
fn selected(command: &Command, commands: &[Command], args: &[&str]) -> bool {
    let Some(arm) = &command.case else {
        return true;
    };
    let value = expand(&arm.subject, args);
    let first = commands
        .iter()
        .filter_map(|c| c.case.as_ref())
        .find(|a| a.case == arm.case && a.patterns.contains(&value));
    first.is_some_and(|a| a.index == arm.index)
}

/// Runs `commands` as sh runs them with the positional arguments `args`. Each program (any
/// command but `set -e`, `true` and `exit`) is given to `program`, which returns its exit
/// status. A list (commands joined by `||` and `&&`) runs left to right, each `||` command
/// only after a failure and each `&&` command only after a success; with `set -e`, a list
/// whose last command ran and failed ends the script with that status.
pub fn run(commands: &[Command], args: &[&str], mut program: impl FnMut(&[String]) -> i32) -> Run {
    let mut errexit = false;
    let mut ran = Vec::new();
    let mut status = 0;
    let mut start = 0;
    while start < commands.len() {
        let end = commands[start + 1..]
            .iter()
            .position(|c| c.join == Join::Start)
            .map_or(commands.len(), |at| start + 1 + at);
        let list = &commands[start..end];
        start = end;
        if !selected(&list[0], commands, args) {
            continue;
        }
        let mut last_ran = false;
        for (at, command) in list.iter().enumerate() {
            let go = match command.join {
                Join::Start => true,
                Join::Or => status != 0,
                Join::And => status == 0,
            };
            last_ran = go && at + 1 == list.len();
            if !go {
                continue;
            }
            let words: Vec<&str> = command.words.iter().map(String::as_str).collect();
            status = match words.as_slice() {
                ["set", "-e"] => {
                    errexit = true;
                    0
                }
                ["true"] => 0,
                ["exit"] => return Run { ran, status },
                ["exit", code] => {
                    let status = code.parse().unwrap_or(2);
                    return Run { ran, status };
                }
                _ => {
                    ran.push(command.words.clone());
                    program(&command.words)
                }
            };
        }
        if errexit && last_ran && status != 0 {
            return Run { ran, status };
        }
    }
    Run { ran, status }
}
