//! The rule of every `cargo xtask` call in a workflow: its words, as the shell splits them, are
//! parsed by the command line of the `xtask` binary itself (`xtask::test_support::cli`), so a
//! call that names no verb, a verb that does not exist, a flag the verb does not have, a flag
//! given twice, or leaves out a required flag or argument, is refused here, before the
//! workflow runs it. A call with `--help` prints the help and runs nothing; it is how a job
//! compiles xtask (`cargo xtask --help`), and passes.
//!
//! A value that names a shell or workflow variable (it holds a `$`) is known only when the step
//! runs, so it is taken as given; every other value is checked by the verb's own parser (an
//! `--arch` that is not an architecture is refused). An alternate-value expansion
//! (`${FAIL_READY:+--fail-ready}`) is two calls, one without its words and one with them, and
//! each is checked. A call is found as `cargo xtask` (with any
//! of cargo's options between, `cargo +nightly xtask`) anywhere in a
//! command of a `run:` script, or as a path to the binary (`target/debug/xtask`) that starts
//! one; a command starts a script, follows `|`, `&&`, `||`, `;`, `&` or a newline, or is inside
//! `( )`. Its words end at the next of those;
//! a redirection (`> file`, `2>&1`) and a comment are not its words. A
//! call inside a quoted `"$( )"` is not found; the repository's own calls are each shown found
//! (`xtask_calls_tests.rs`), so a call written that way fails that test instead.

use std::ffi::OsStr;

use clap::Command;
use clap::builder::{TypedValueParser, ValueParser};
use clap::error::{ContextKind, ContextValue, ErrorKind};

use super::downloads::collect_runs;
use super::yaml::Node;

/// One word of a script, as the shell would split it, or one of its operators.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    /// A word that ends a command: `|`, `||`, `&&`, `;`, `&`, `(`, `)` or a newline.
    End,
    /// A redirection; `takes_target` when the next word is its file (not `2>&1`).
    Redirect {
        takes_target: bool,
    },
}

/// The tokens of `script`, with quotes and backslashes removed as the shell removes them. A
/// workflow expression (`${{ ... }}`) is replaced before the shell runs, so its spaces split no
/// word.
fn tokens(script: &str) -> Vec<Token> {
    let c: Vec<char> = script.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    // A word was started: `''` is a word, though it is empty.
    let mut started = false;
    let mut i = 0;
    let flush = |word: &mut String, started: &mut bool, out: &mut Vec<Token>| {
        if *started {
            out.push(Token::Word(std::mem::take(word)));
            *started = false;
        }
    };
    while i < c.len() {
        let ch = c[i];
        i += 1;
        match ch {
            '\'' => {
                started = true;
                while i < c.len() && c[i] != '\'' {
                    word.push(c[i]);
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                started = true;
                while i < c.len() && c[i] != '"' {
                    match (c[i], c.get(i + 1)) {
                        ('\\', Some('"' | '\\' | '$' | '`')) => {
                            word.push(c[i + 1]);
                            i += 2;
                        }
                        ('\\', Some('\n')) => i += 2,
                        (q, _) => {
                            word.push(q);
                            i += 1;
                        }
                    }
                }
                i += 1;
            }
            '\\' => {
                if let Some(&e) = c.get(i).filter(|e| **e != '\n') {
                    started = true;
                    word.push(e);
                }
                i += 1;
            }
            '$' if c.get(i..i + 2) == Some(&['{', '{']) => {
                started = true;
                word.push('$');
                let from = i;
                while i < c.len() && !(i >= from + 4 && c[i - 2..i] == ['}', '}']) {
                    word.push(c[i]);
                    i += 1;
                }
            }
            // A parameter expansion (`${NAME:+a b}`) is one word up to its own `}`.
            '$' if c.get(i) == Some(&'{') => {
                started = true;
                word.push('$');
                let mut depth = 0;
                while i < c.len() {
                    let ch = c[i];
                    word.push(ch);
                    i += 1;
                    match ch {
                        '{' => depth += 1,
                        '}' if depth == 1 => break,
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
            }
            '#' if !started => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '>' | '<' => {
                // A file descriptor before the operator (`2>`) is part of it, not a word.
                if started && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    started = false;
                } else {
                    flush(&mut word, &mut started, &mut out);
                }
                while i < c.len() && matches!(c[i], '>' | '<') {
                    i += 1;
                }
                let mut takes_target = true;
                if c.get(i) == Some(&'&') {
                    takes_target = false;
                    i += 1;
                    while i < c.len() && (c[i].is_ascii_digit() || c[i] == '-') {
                        i += 1;
                    }
                }
                out.push(Token::Redirect { takes_target });
            }
            '|' | '&' | ';' | '(' | ')' | '\n' => {
                flush(&mut word, &mut started, &mut out);
                out.push(Token::End);
            }
            w if w.is_whitespace() => flush(&mut word, &mut started, &mut out),
            w => {
                started = true;
                word.push(w);
            }
        }
    }
    flush(&mut word, &mut started, &mut out);
    out
}

/// The commands of `script`: each one's words, redirections and their files left out.
pub fn commands(script: &str) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    let mut skip_target = false;
    for token in tokens(script) {
        match token {
            Token::End => out.push(Vec::new()),
            Token::Redirect { takes_target } => skip_target = takes_target,
            Token::Word(w) if !std::mem::take(&mut skip_target) => {
                if let Some(command) = out.last_mut() {
                    command.push(w);
                }
            }
            Token::Word(_) => {}
        }
    }
    out.retain(|c| !c.is_empty());
    out
}

/// The words after `cargo xtask` in `command`, if it calls xtask that way anywhere in it (after
/// `time`, say; with cargo's options between, `cargo "+$TOOLCHAIN" xtask`), or after a path to the
/// binary (`target/debug/xtask`) that starts the command, after any `NAME=value` words, or after
/// the image of a `docker run` (`docker run ... name@sha256:<digest> xtask`).
pub fn xtask_args(command: &[String]) -> Option<&[String]> {
    for (i, word) in command.iter().enumerate() {
        if word != "cargo" {
            continue;
        }
        // cargo's own options before the alias: a toolchain (`+nightly`), `--locked`, `-q`.
        let after = command[i + 1..]
            .iter()
            .position(|w| !w.starts_with(['+', '-']))
            .map(|n| i + 1 + n);
        if let Some(at) = after.filter(|at| command[*at] == "xtask") {
            return Some(&command[at + 1..]);
        }
    }
    // `docker run ... <image>@sha256:<digest> xtask <args>`: the binary run in a container, the
    // word `xtask` after the image.
    if command.first().map(String::as_str) == Some("docker")
        && let Some(at) = command.iter().position(|w| w.contains("@sha256:"))
        && command.get(at + 1).map(String::as_str) == Some("xtask")
    {
        return Some(&command[at + 2..]);
    }
    let first = command.iter().position(|w| !is_assignment(w))?;
    let word = &command[first];
    (word.contains('/') && word.rsplit('/').next() == Some("xtask")).then(|| &command[first + 1..])
}

/// `NAME=value`: a variable set for the command, not a word of it.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// A value parser that takes a value naming a variable as it is, and checks any other value with
/// the verb's own parser.
#[derive(Clone)]
struct Lenient(ValueParser);

impl TypedValueParser for Lenient {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &Command,
        arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<String, clap::Error> {
        let text = value.to_string_lossy().into_owned();
        if text.contains('$') {
            return Ok(text);
        }
        // The verb's parser, run on this one value as the only argument of a command of its
        // own; a refusal names the verb's flag and the value.
        Command::default()
            .no_binary_name(true)
            .arg(
                clap::Arg::new("value")
                    .required(true)
                    .allow_hyphen_values(true)
                    .value_parser(self.0.clone()),
            )
            .try_get_matches_from([value])
            .map(|_| text.clone())
            .map_err(|inner| {
                let mut e = clap::Error::new(inner.kind()).with_cmd(cmd);
                if let Some(arg) = arg {
                    e.insert(
                        ContextKind::InvalidArg,
                        ContextValue::String(arg.to_string()),
                    );
                }
                e.insert(ContextKind::InvalidValue, ContextValue::String(text));
                e
            })
    }
}

/// `cmd` and every verb under it, with each value parser made [`Lenient`].
fn lenient(cmd: Command) -> Command {
    cmd.mut_args(|a| {
        if a.get_action().takes_values() {
            let original = a.get_value_parser().clone();
            a.value_parser(Lenient(original))
        } else {
            a
        }
    })
    .mut_subcommands(lenient)
}

/// The verbs `args` names, in order (`release nfpm-stage`), as far as `cmd` has them.
fn verb_path(cmd: &Command, args: &[String]) -> Vec<String> {
    let mut path = Vec::new();
    let mut at = cmd;
    for word in args {
        match at.find_subcommand(word) {
            Some(sub) => {
                path.push(sub.get_name().to_owned());
                at = sub;
            }
            None => break,
        }
    }
    path
}

/// What `e` says is wrong, in one line: its kind, then the verb, flag and value it names.
fn describe(e: &clap::Error) -> String {
    let mut out = e.kind().to_string();
    let mut named = Vec::new();
    for kind in [
        ContextKind::InvalidSubcommand,
        ContextKind::InvalidArg,
        ContextKind::InvalidValue,
    ] {
        match e.get(kind) {
            Some(ContextValue::String(s)) => named.push(format!("`{s}`")),
            Some(ContextValue::Strings(v)) => named.extend(v.iter().map(|s| format!("`{s}`"))),
            _ => {}
        }
    }
    if !named.is_empty() {
        out.push_str(": ");
        out.push_str(&named.join(", "));
    }
    out
}

/// The word an alternate-value expansion stands for when its variable is set: `X` of
/// `${NAME:+X}` or `${NAME+X}`.
fn alternate(word: &str) -> Option<&str> {
    let inner = word.strip_prefix("${")?.strip_suffix('}')?;
    let at = inner.find('+')?;
    let name = inner[..at].strip_suffix(':').unwrap_or(&inner[..at]);
    let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then(|| &inner[at + 1..])
}

/// Every way `args` can reach the binary: each alternate-value expansion
/// (`${FAIL_READY:+--fail-ready}`) left out, as when its variable is unset, and replaced by
/// its words, as when it is set. Each way is labelled with the expansions it set.
fn variants(args: &[String]) -> Vec<(Vec<String>, Vec<String>)> {
    let mut out = vec![(Vec::new(), Vec::new())];
    for word in args {
        match alternate(word) {
            Some(value) => {
                let set = out.clone().into_iter().map(|(mut a, mut l)| {
                    a.extend(value.split_whitespace().map(str::to_owned));
                    l.push(format!("`{word}` as `{value}`"));
                    (a, l)
                });
                let set: Vec<_> = set.collect();
                out.extend(set);
            }
            None => out.iter_mut().for_each(|(a, _)| a.push(word.clone())),
        }
    }
    out
}

/// Why the words `args` after `cargo xtask` would be refused by the binary, if they would, in
/// any of their [`variants`].
pub fn call_problem(args: &[String]) -> Option<String> {
    let cli = lenient(xtask::test_support::cli());
    variants(args).into_iter().find_map(|(args, set)| {
        let path = verb_path(&cli, &args);
        let mut shown = std::iter::once("cargo xtask".to_owned())
            .chain(path)
            .collect::<Vec<_>>()
            .join(" ");
        shown = format!("`{shown}`");
        if !set.is_empty() {
            shown = format!("{shown} (with {})", set.join(", "));
        }
        let parsed = cli
            .clone()
            .try_get_matches_from(std::iter::once("xtask").chain(args.iter().map(String::as_str)));
        match parsed {
            Ok(_) => None,
            Err(e) if e.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                Some(format!("{shown} names no verb"))
            }
            // `cargo xtask --help` is how a job compiles xtask before a step that holds the
            // token.
            Err(e) if e.kind() == ErrorKind::DisplayHelp => None,
            Err(e) => Some(format!("{shown}: {}", describe(&e))),
        }
    })
}

/// One line per call of xtask in the workflow `doc` that the binary would refuse.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut runs = Vec::new();
    collect_runs(doc, &mut runs);
    let mut out = Vec::new();
    for (line, run) in runs {
        for command in commands(run) {
            if let Some(problem) = xtask_args(&command).and_then(call_problem) {
                out.push(format!("line {line}: {problem}"));
            }
        }
    }
    out
}
