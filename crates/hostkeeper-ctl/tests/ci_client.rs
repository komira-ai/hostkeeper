//! The `hostkeeper ctl` commands the CI client documents and runs, against the generated command
//! table.
//!
//! Every invocation in `.github/workflows/set-desired.yml` and `docs/deploy/ci-client.md` must
//! name an RPC of the table and only flags that RPC (or every RPC) takes, so a command or flag
//! renamed in the protos fails here instead of in a release.
//!
//! An invocation starts at the words `hostkeeper ctl` and runs to the end of the line; a line
//! ending in a backslash continues on the next. Inline code ends it at the closing backtick; a
//! redirection, pipe, `&&`, `;` or `)` ends it too. The first two words that are not flags or
//! flag values are the service and the RPC.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hostkeeper_ctl::doc::tree_lines;
use hostkeeper_proto::ctl::COMMANDS;

/// The names of the command table that decide how an invocation reads.
struct Table {
    /// `service rpc` of each command.
    commands: BTreeSet<String>,
    /// Global flag name, and whether it takes a value.
    globals: BTreeMap<String, bool>,
    /// (`service rpc`, flag name), and whether the flag takes a value.
    flags: BTreeMap<(String, String), bool>,
    /// Flag name of any RPC, and whether it takes a value: how a flag before the RPC is read.
    any: BTreeMap<String, bool>,
}

impl Table {
    fn new(lines: &[String]) -> Self {
        let mut t = Table {
            commands: BTreeSet::new(),
            globals: BTreeMap::new(),
            flags: BTreeMap::new(),
            any: BTreeMap::new(),
        };
        let name = |flag: &str| flag.trim_start_matches("--").to_owned();
        for line in lines {
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                ["command", svc, rpc, ..] => {
                    t.commands.insert(format!("{svc} {rpc}"));
                }
                ["global", flag, rest @ ..] => {
                    t.globals.insert(name(flag), !rest.is_empty());
                }
                ["flag", svc, rpc, flag, rest @ ..] => {
                    t.add(format!("{svc} {rpc}"), name(flag), !rest.is_empty());
                }
                ["secret", svc, rpc, file, stdin, ..] => {
                    t.add(format!("{svc} {rpc}"), name(file), true);
                    t.add(format!("{svc} {rpc}"), name(stdin), false);
                }
                _ => {}
            }
        }
        t
    }

    fn add(&mut self, at: String, name: String, value: bool) {
        *self.any.entry(name.clone()).or_insert(false) |= value;
        self.flags.insert((at, name), value);
    }

    fn takes_value(&self, name: &str) -> bool {
        self.globals
            .get(name)
            .or_else(|| self.any.get(name))
            .copied()
            .unwrap_or(false)
    }
}

/// One `hostkeeper ctl` invocation: the line of `hostkeeper`, the words that are not flags, and
/// the flag names, each with its line.
struct Call {
    line: usize,
    positional: Vec<(usize, String)>,
    flags: Vec<(usize, String)>,
}

fn unquote(s: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner;
        }
    }
    s
}

/// A word that ends an invocation: a redirection, pipe, `&&`, `;` or `)`.
fn ends_invocation(word: &str) -> bool {
    word.trim_start_matches(|c: char| c.is_ascii_digit())
        .starts_with(['>', '<', '|', '&', ';', ')'])
}

/// The words of an invocation up to its end, and how many words of `rest` that took.
fn collect<'a>(rest: &[(usize, &'a str)]) -> (Vec<(usize, &'a str)>, usize) {
    let mut args = Vec::new();
    let mut used = 0;
    for &(line, word) in rest {
        used += 1;
        if ends_invocation(word) {
            break;
        }
        let mut word = word;
        let mut end = false;
        if let Some((before, _)) = word.split_once('`') {
            word = before;
            end = true;
        }
        let stripped = word.trim_end_matches([';', ')']);
        if stripped.len() != word.len() {
            word = stripped;
            end = true;
        }
        if !word.is_empty() {
            args.push((line, word));
        }
        if end {
            break;
        }
    }
    (args, used)
}

/// The invocations of `text`, with line continuations joined.
fn calls(text: &str, table: &Table) -> Vec<Call> {
    let mut logical: Vec<Vec<(usize, &str)>> = Vec::new();
    let mut current: Vec<(usize, &str)> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let trimmed = raw.trim_end();
        let (body, more) = match trimmed.strip_suffix('\\') {
            Some(body) => (body, true),
            None => (trimmed, false),
        };
        current.extend(body.split_whitespace().map(|w| (i + 1, w)));
        if !more {
            logical.push(std::mem::take(&mut current));
        }
    }
    logical.push(current);
    let mut out = Vec::new();
    for words in &logical {
        let mut i = 0;
        while i + 1 < words.len() {
            let starts = words[i].1.trim_start_matches('`') == "hostkeeper"
                && words[i + 1].1.split('`').next() == Some("ctl");
            if !starts {
                i += 1;
                continue;
            }
            let (args, used) = if words[i + 1].1.contains('`') {
                (Vec::new(), 0)
            } else {
                collect(&words[i + 2..])
            };
            out.push(build(words[i].0, args, table));
            i += 2 + used;
        }
    }
    out
}

fn build(line: usize, args: Vec<(usize, &str)>, table: &Table) -> Call {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut words = args.into_iter();
    while let Some((l, word)) = words.next() {
        let word = unquote(word);
        match word.strip_prefix("--") {
            Some(body) => match body.split_once('=') {
                Some((name, _)) => flags.push((l, name.to_owned())),
                None => {
                    flags.push((l, body.to_owned()));
                    if table.takes_value(body) {
                        words.next();
                    }
                }
            },
            None => positional.push((l, word.to_owned())),
        }
    }
    Call {
        line,
        positional,
        flags,
    }
}

/// One line per invocation of `text` that does not resolve against the generated `lines`:
/// `<file>:<line>: <problem>`.
fn unresolved(file: &str, text: &str, lines: &[String]) -> Vec<String> {
    let table = Table::new(lines);
    let mut errors = Vec::new();
    for call in calls(text, &table) {
        let [(_, svc), (rpc_line, rpc), ..] = call.positional.as_slice() else {
            errors.push(format!("{file}:{}: hostkeeper ctl names no rpc", call.line));
            continue;
        };
        let at = format!("{svc} {rpc}");
        if !table.commands.contains(&at) {
            errors.push(format!("{file}:{rpc_line}: {at} is not a command"));
            continue;
        }
        for (line, name) in &call.flags {
            if !table.globals.contains_key(name)
                && !table.flags.contains_key(&(at.clone(), name.clone()))
            {
                errors.push(format!("{file}:{line}: --{name} is not a flag of {at}"));
            }
        }
    }
    errors
}

/// The `service rpc` of each invocation in `text` that names two words, and how many
/// invocations there are.
fn invoked(text: &str, lines: &[String]) -> (BTreeSet<String>, usize) {
    let all = calls(text, &Table::new(lines));
    let named = all
        .iter()
        .filter_map(|c| match c.positional.as_slice() {
            [(_, svc), (_, rpc), ..] => Some(format!("{svc} {rpc}")),
            _ => None,
        })
        .collect();
    (named, all.len())
}

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Catches a command or flag renamed in the protos while `set-desired.yml` or `ci-client.md`
/// still uses the old name, and an extraction that finds no invocation at all. The workflow runs
/// the client (`hostkeeper ci set-desired`), which calls ctl in its own process, so it may name
/// no ctl command; any it does name must resolve. The client's own calls are checked against the
/// table where they are made, by the tests of `hostkeeper-ci`, which run them through ctl.
#[test]
fn the_documented_ctl_commands_exist_in_the_descriptor_table() {
    let lines = tree_lines(COMMANDS);
    // (short name, path, whether it must invoke ctl at least once)
    let files = [
        (
            "set-desired.yml",
            ".github/workflows/set-desired.yml",
            false,
        ),
        ("ci-client.md", "docs/deploy/ci-client.md", true),
    ];
    let mut errors = Vec::new();
    let mut all = BTreeSet::new();
    for (name, path, required) in files {
        let text = read(path);
        let (named, count) = invoked(&text, &lines);
        assert!(
            count > 0 || !required,
            "{name} has no `hostkeeper ctl` invocation"
        );
        all.extend(named);
        errors.extend(unresolved(name, &text, &lines));
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    for needed in ["releases set-desired", "releases converge"] {
        assert!(all.contains(needed), "no file invokes {needed}: {all:?}");
    }
}

fn fixture_lines() -> Vec<String> {
    [
        "global --endpoint URL",
        "global --token-stdin",
        "global --request FILE|-",
        "global --help",
        "command releases set-desired /x/SetDesired",
        "flag releases set-desired --service STRING",
        "flag releases set-desired --version STRING",
        "command releases converge /x/Converge",
        "flag releases converge --include-held true|false",
        "command hosts list /x/List",
    ]
    .map(String::from)
    .to_vec()
}

/// Catches a checker that accepts a renamed RPC, an unknown flag, a flag of another RPC, a bad
/// flag on a continued line, an invocation without an RPC or an unknown RPC in inline code, and
/// one that refuses `--flag=value`, a valueless global, quoted values or a redirection.
#[test]
fn a_renamed_command_or_flag_in_the_ci_client_is_refused() {
    let lines = fixture_lines();
    let check = |file: &str, text: &str| unresolved(file, text, &lines);
    assert_eq!(
        check(
            "f.yml",
            "      hostkeeper ctl releases set-desire --service a\n"
        ),
        ["f.yml:1: releases set-desire is not a command"]
    );
    assert_eq!(
        check(
            "f.yml",
            "hostkeeper ctl releases set-desired --versionn 1\n"
        ),
        ["f.yml:1: --versionn is not a flag of releases set-desired"]
    );
    assert_eq!(
        check("f.yml", "hostkeeper ctl releases converge --service a\n"),
        ["f.yml:1: --service is not a flag of releases converge"]
    );
    assert_eq!(
        check(
            "f.yml",
            "run: hostkeeper ctl releases set-desired \\\n  --service a \\\n  --bogus b\n"
        ),
        ["f.yml:3: --bogus is not a flag of releases set-desired"]
    );
    assert_eq!(
        check("f.yml", "x\nhostkeeper ctl releases\n"),
        ["f.yml:2: hostkeeper ctl names no rpc"]
    );
    assert_eq!(
        check("f.md", "See `hostkeeper ctl hosts lst`.\n"),
        ["f.md:1: hosts lst is not a command"]
    );
    assert_eq!(
        check(
            "f.md",
            "Run `hostkeeper ctl releases converge` then `hostkeeper ctl releases conversge --endpoint x`.\n"
        ),
        ["f.md:1: releases conversge is not a command"]
    );
    let good = "hostkeeper ctl --endpoint=https://coordinator.example --token-stdin releases set-desired \\\n  \
                --service \"$SERVICE\" --version '<version>' --request - 2> \"$RUNNER_TEMP/err\"\n\
                hostkeeper ctl hosts list --endpoint=u; echo done\n";
    assert_eq!(check("f.yml", good), Vec::<String>::new());
    let (named, count) = invoked(good, &lines);
    assert_eq!(count, 2);
    assert_eq!(
        named.into_iter().collect::<Vec<_>>(),
        ["hosts list", "releases set-desired"]
    );
}
