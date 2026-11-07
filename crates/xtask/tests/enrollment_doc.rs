//! `docs/operations/enrollment.md`, the runbook for enrolling a node, and
//! `docs/operations/network-install.md`, the runbook for installing one from the network, against
//! the code they describe.
//!
//! - Every command line in a fenced block that begins `hostkeeper ctl` parses with ctl's own
//!   parser (`hostkeeper_ctl::parse_with`) over the generated command table, the one
//!   `docs/ctl.md` is written from: a renamed RPC or flag, or a value a bool or enum flag does
//!   not take, fails here. A line continued with a trailing `\` is one line; words are split as
//!   a shell splits them, by white space, with `'...'` and `"..."` quoting.
//! - The runbook uses the calls of the procedure: `add-record`, `list-records`, `remove-record`,
//!   `open-record`, `list` and `approve` of `enrollment`.
//! - The tokens of its two refusal tables are exactly the refusal tokens of the protos: the
//!   table under `## Refusals of the operator calls` is every value of `EnrollmentRefusal`, and
//!   the one under `## Refusals of the host's calls` every value of `CoordinatorRefusal` but the
//!   heartbeat's own `invalid_heartbeat`. A token is the value's name without its enum prefix, in
//!   lower case, as the coordinator sends it. Anywhere else, a token written after "refused"
//!   (``refused `x` `` or ``refused: `x` ``) must be one of them.
//! - The network-install runbook's `hostkeeper ctl` lines parse the same way, it uses the calls of
//!   its two steps per machine (`add-record`, `list-records`, `list` and `approve` of
//!   `enrollment`), and a token it writes after "refused" is one of the protos' refusal tokens.
//!
//! The checks are pure functions of the text, each tested on edited texts as well as on the real
//! file. The file is read from the repository; a missing file fails.

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_ctl::{Parsed, parse_with};
use hostkeeper_proto::ctl::COMMANDS;
use prost_reflect::DescriptorPool;

/// The runbook, from the repository root.
const DOC: &str = "docs/operations/enrollment.md";

/// The network-install runbook, from the repository root.
const NETWORK_INSTALL_DOC: &str = "docs/operations/network-install.md";

/// The calls the network-install runbook's steps need, as `service rpc`.
const NETWORK_INSTALL_REQUIRED: &[&str] = &[
    "enrollment add-record",
    "enrollment list-records",
    "enrollment list",
    "enrollment approve",
];

/// The heading of the table of `EnrollmentRefusal`.
const OPERATOR_TABLE: &str = "## Refusals of the operator calls";

/// The heading of the table of `CoordinatorRefusal`.
const HOST_TABLE: &str = "## Refusals of the host's calls";

/// The `CoordinatorRefusal` tokens that only a heartbeat is answered with.
const HEARTBEAT_ONLY: &[&str] = &["invalid_heartbeat"];

/// The calls the procedure needs, as `service rpc`.
const REQUIRED: &[&str] = &[
    "enrollment add-record",
    "enrollment list-records",
    "enrollment remove-record",
    "enrollment open-record",
    "enrollment list",
    "enrollment approve",
    "enrollment reject",
    "hosts retire",
];

/// The lines inside fenced blocks, with each line ending in `\` joined to the next. A line still
/// continued where its block or the document ends is kept with its `\`, which [`words`]
/// refuses, so that a block never runs on into the next one.
fn fenced_lines(doc: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut fenced = false;
    let mut pending = String::new();
    let cut = |pending: &mut String, lines: &mut Vec<String>| {
        if !pending.is_empty() {
            lines.push(format!("{}\\", std::mem::take(pending)));
        }
    };
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            cut(&mut pending, &mut lines);
            fenced = !fenced;
            continue;
        }
        if !fenced {
            continue;
        }
        let line = line.trim();
        match line.strip_suffix('\\') {
            Some(head) => {
                pending.push_str(head.trim_end());
                pending.push(' ');
            }
            None => {
                pending.push_str(line);
                lines.push(std::mem::take(&mut pending));
            }
        }
    }
    cut(&mut pending, &mut lines);
    lines
}

/// The fenced lines that call `hostkeeper ctl`.
fn ctl_lines(doc: &str) -> Vec<String> {
    fenced_lines(doc)
        .into_iter()
        .filter(|l| {
            let mut words = l.split_whitespace();
            words.next() == Some("hostkeeper") && words.next() == Some("ctl")
        })
        .collect()
}

/// `line` split into words as a shell splits them: by white space, a `'...'` or `"..."` span
/// one word with its quotes taken off, a `\` outside quotes keeping the next character.
fn words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some(q) if q == c => break,
                        Some(other) => word.push(other),
                        None => return Err(format!("an unclosed {c} in: {line}")),
                    }
                }
            }
            '\\' => match chars.next() {
                Some(next) => word.get_or_insert_with(String::new).push(next),
                None => return Err(format!("a trailing \\ in: {line}")),
            },
            c if c.is_whitespace() => words.extend(word.take()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Ok(words)
}

/// The command `line` calls, as `service rpc`, or why it does not parse; `None` for `--help`.
fn command_of(line: &str) -> Result<Option<String>, String> {
    let words = words(line)?;
    // `hostkeeper ctl` is the program's name, as the binary passes it to ctl.
    let args = std::iter::once("hostkeeper ctl".to_owned()).chain(words.into_iter().skip(2));
    match parse_with(COMMANDS, args) {
        Ok(Parsed::Call(c)) => Ok(Some(format!("{} {}", c.service_command, c.method_command))),
        Ok(Parsed::Help) => Ok(None),
        Err(e) => Err(format!("{line}\n{e}")),
    }
}

/// Every command line of the enrollment runbook that does not parse, and every call of
/// [`REQUIRED`] no line makes.
fn command_problems(doc: &str) -> Vec<String> {
    command_problems_for(doc, REQUIRED)
}

/// Every command line of `doc` that does not parse, and every call of `required` no line makes.
fn command_problems_for(doc: &str, required: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut called = BTreeSet::new();
    for line in ctl_lines(doc) {
        match command_of(&line) {
            Ok(command) => called.extend(command),
            Err(e) => problems.push(e),
        }
    }
    for required in required {
        if !called.contains(*required) {
            problems.push(format!("no command line calls `hostkeeper ctl {required}`"));
        }
    }
    problems
}

/// The tokens of an enum of the protos, `UNSPECIFIED` left out.
fn tokens_of(name: &str, prefix: &str) -> BTreeSet<String> {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decode");
    let refusal = pool.get_enum_by_name(name).expect("the refusal enum");
    refusal
        .values()
        .map(|v| {
            v.name()
                .strip_prefix(prefix)
                .expect("the enum's prefix")
                .to_ascii_lowercase()
        })
        .filter(|t| t != "unspecified")
        .collect()
}

/// The refusal tokens of the operator API and of the host's calls, from the protos.
fn code_tokens() -> (BTreeSet<String>, BTreeSet<String>) {
    let operator = tokens_of(
        "hostkeeper.operator.v1.EnrollmentRefusal",
        "ENROLLMENT_REFUSAL_",
    );
    let mut host = tokens_of(
        "hostkeeper.agent.v1.CoordinatorRefusal",
        "COORDINATOR_REFUSAL_",
    );
    for token in HEARTBEAT_ONLY {
        host.remove(*token);
    }
    (operator, host)
}

/// The tokens of the first column of the table under `heading`, in order, or why there are none.
fn table_tokens(doc: &str, heading: &str) -> Result<Vec<String>, String> {
    let mut lines = doc.lines().skip_while(|l| l.trim_end() != heading);
    if lines.next().is_none() {
        return Err(format!("no section `{heading}`"));
    }
    let tokens: Vec<String> = lines
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split_once('`').map(|(token, _)| token.to_owned()))
        .collect();
    if tokens.is_empty() {
        return Err(format!("the section `{heading}` has no table of tokens"));
    }
    Ok(tokens)
}

/// What is wrong with the table under `heading`, against the tokens `code` defines.
fn table_problems(doc: &str, heading: &str, code: &BTreeSet<String>) -> Vec<String> {
    let tokens = match table_tokens(doc, heading) {
        Ok(tokens) => tokens,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for token in &tokens {
        if !code.contains(token) {
            problems.push(format!(
                "`{token}` under `{heading}` is no refusal token of the code"
            ));
        }
        if !seen.insert(token.clone()) {
            problems.push(format!("`{token}` is listed twice under `{heading}`"));
        }
    }
    for token in code.difference(&seen) {
        problems.push(format!("the code's `{token}` is missing under `{heading}`"));
    }
    problems
}

/// Every token written after "refused" in `doc` that neither table's enum defines.
fn mention_problems(doc: &str, known: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    for marker in ["refused `", "refused: `"] {
        for (at, _) in doc.match_indices(marker) {
            let rest = &doc[at + marker.len()..];
            let token = rest.split('`').next().unwrap_or_default();
            if !known.contains(token) {
                problems.push(format!("`{token}` is written as a refusal and is none"));
            }
        }
    }
    problems
}

/// Every problem of the runbook's refusal tokens.
fn token_problems(doc: &str) -> Vec<String> {
    let (operator, host) = code_tokens();
    let mut problems = table_problems(doc, OPERATOR_TABLE, &operator);
    problems.extend(table_problems(doc, HOST_TABLE, &host));
    let known: BTreeSet<String> = operator.union(&host).cloned().collect();
    problems.extend(mention_problems(doc, &known));
    problems
}

/// Catches a command line of the runbook that `hostkeeper ctl` would refuse as a usage error (a
/// renamed or misspelled RPC or flag, a value a flag does not take), a step of the procedure
/// with no command, and a refusal token the code does not define or the runbook leaves out.
#[test]
fn enrollment_doc_commands_parse() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(DOC);
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {DOC}: {e}"));
    let mut problems = command_problems(&doc);
    problems.extend(token_problems(&doc));
    assert!(problems.is_empty(), "{DOC}:\n{}", problems.join("\n"));
    assert!(ctl_lines(&doc).len() >= REQUIRED.len());
}

/// Every problem of the network-install runbook: a command line that does not parse, a call of
/// its steps no line makes, and a token written as a refusal that the protos do not define.
fn network_install_problems(doc: &str) -> Vec<String> {
    let mut problems = command_problems_for(doc, NETWORK_INSTALL_REQUIRED);
    let (operator, host) = code_tokens();
    let known: BTreeSet<String> = operator.union(&host).cloned().collect();
    problems.extend(mention_problems(doc, &known));
    problems
}

/// Catches a `hostkeeper ctl` line of the network-install runbook that ctl would refuse as a
/// usage error, a step of it with no command, and a refusal token the code does not define.
#[test]
fn network_install_doc_commands_parse() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(NETWORK_INSTALL_DOC);
    let doc = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {NETWORK_INSTALL_DOC}: {e}"));
    let problems = network_install_problems(&doc);
    assert!(
        problems.is_empty(),
        "{NETWORK_INSTALL_DOC}:\n{}",
        problems.join("\n")
    );
}

/// Catches the network-install check passing a misspelled call, a runbook that leaves out the
/// approval, and a refusal token no proto defines.
#[test]
fn the_network_install_check_refuses_each_defect() {
    let all: String = NETWORK_INSTALL_REQUIRED
        .iter()
        .map(|r| fenced(&format!("hostkeeper ctl {r}")))
        .collect();
    assert_eq!(network_install_problems(&all), Vec::<String>::new());
    let misspelled = all.clone() + &fenced("hostkeeper ctl enrollment list-record");
    let problems = network_install_problems(&misspelled);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("hostkeeper ctl enrollment list-record\n"),
        "{problems:?}"
    );
    let without_approve: String = NETWORK_INSTALL_REQUIRED
        .iter()
        .filter(|r| **r != "enrollment approve")
        .map(|r| fenced(&format!("hostkeeper ctl {r}")))
        .collect();
    assert_eq!(
        network_install_problems(&without_approve),
        ["no command line calls `hostkeeper ctl enrollment approve`"]
    );
    let token = all + "A heartbeat is refused `revoked` at once.\n";
    assert_eq!(
        network_install_problems(&token),
        ["`revoked` is written as a refusal and is none"]
    );
}

/// A fenced block holding `line`, for the edited texts.
fn fenced(line: &str) -> String {
    format!("text\n\n```\n{line}\n```\n")
}

/// Every required call, in a fenced block each.
fn required_calls() -> String {
    REQUIRED
        .iter()
        .map(|r| fenced(&format!("hostkeeper ctl {r}")))
        .collect()
}

/// Catches the parser accepting a misspelled service, RPC or flag, or a bad value, and a line
/// cut at its `\` read as two.
#[test]
fn a_misspelled_command_is_refused() {
    for line in [
        "hostkeeper ctl enrollment aprove --request-id r",
        "hostkeeper ctl enrolment list",
        "hostkeeper ctl enrollment approve --key-hash k",
        "hostkeeper ctl enrollment approve --replace yes",
        "hostkeeper ctl --output yaml enrollment list",
        "hostkeeper ctl enrollment",
    ] {
        let problems = command_problems(&(required_calls() + &fenced(line)));
        assert_eq!(problems.len(), 1, "{line}: {problems:?}");
        assert!(problems[0].starts_with(line), "{line}: {problems:?}");
    }
    let continued =
        "hostkeeper ctl enrollment approve \\\n    --replace true --reason \"two words\"";
    assert_eq!(
        command_problems(&(required_calls() + &fenced(continued))),
        Vec::<String>::new()
    );
    assert_eq!(
        ctl_lines(&fenced(continued)),
        ["hostkeeper ctl enrollment approve --replace true --reason \"two words\""]
    );
    // A block whose last line is continued neither runs on into the next block nor is lost.
    let cut = "hostkeeper ctl enrollment list \\";
    for doc in [
        fenced(cut) + &fenced("hostkeeper ctl enrollment list"),
        fenced(cut),
    ] {
        assert_eq!(ctl_lines(&doc)[0], "hostkeeper ctl enrollment list \\");
        let problems = command_problems(&(required_calls() + &doc));
        assert_eq!(
            problems,
            ["a trailing \\ in: hostkeeper ctl enrollment list \\"]
        );
    }
    assert_eq!(
        command_of("hostkeeper ctl enrollment approve --help"),
        Ok(None)
    );
}

/// Catches a runbook that drops a step of the procedure, and command lines read from outside a
/// fenced block or from a line that only mentions ctl.
#[test]
fn every_step_of_the_procedure_has_a_command() {
    let without_approve: String = REQUIRED
        .iter()
        .filter(|r| **r != "enrollment approve")
        .map(|r| fenced(&format!("hostkeeper ctl {r}")))
        .collect();
    let outside =
        "Run `hostkeeper ctl enrollment approve` and\nhostkeeper ctl enrollment approve --bad\n";
    let doc = without_approve + outside + &fenced("echo hostkeeper ctl enrollment approve --bad");
    assert_eq!(
        command_problems(&doc),
        ["no command line calls `hostkeeper ctl enrollment approve`"]
    );
    assert_eq!(command_problems(&required_calls()), Vec::<String>::new());
}

/// Catches the word splitter keeping quotes, splitting a quoted span, or accepting an unclosed
/// quote or a trailing `\`.
#[test]
fn words_are_split_as_a_shell_splits_them() {
    assert_eq!(
        words(r#"a  "b c" 'd "e"' f\ g h"i"j"#),
        Ok(vec!["a", "b c", "d \"e\"", "f g", "hij"]
            .into_iter()
            .map(str::to_owned)
            .collect())
    );
    assert_eq!(words("  "), Ok(Vec::new()));
    assert_eq!(words("a ''"), Ok(vec!["a".to_owned(), String::new()]));
    assert_eq!(words("a \"b"), Err("an unclosed \" in: a \"b".to_owned()));
    assert_eq!(words("a 'b"), Err("an unclosed ' in: a 'b".to_owned()));
    assert_eq!(words("a \\"), Err("a trailing \\ in: a \\".to_owned()));
    assert!(command_of("hostkeeper ctl enrollment list --page-token 'x").is_err());
}

/// A refusal table under `heading` with `tokens`.
fn table(heading: &str, tokens: &[&str]) -> String {
    let mut text = format!("{heading}\n\n| token | cause |\n|---|---|\n");
    for token in tokens {
        text.push_str(&format!("| `{token}` | why |\n"));
    }
    text.push('\n');
    text
}

/// Both tables as the code defines them, `edit` applied to the operator one.
fn tables(edit: impl Fn(&mut Vec<String>)) -> String {
    let (operator, host) = code_tokens();
    let mut operator: Vec<String> = operator.into_iter().collect();
    edit(&mut operator);
    let operator: Vec<&str> = operator.iter().map(String::as_str).collect();
    let host: Vec<&str> = host.iter().map(String::as_str).collect();
    table(OPERATOR_TABLE, &operator) + &table(HOST_TABLE, &host)
}

/// Catches a refusal token the code does not define (a misspelling), one the runbook leaves out
/// or lists twice, a heartbeat-only token listed as the host's, a table that is missing, and a
/// refusal written in prose that is no token.
#[test]
fn a_token_the_code_does_not_define_is_refused() {
    assert_eq!(token_problems(&tables(|_| {})), Vec::<String>::new());
    let misspelled = tables(|t| {
        t.retain(|x| x != "pending_full");
        t.push("pending_ful".to_owned());
    });
    assert_eq!(
        token_problems(&misspelled),
        [
            format!("`pending_ful` under `{OPERATOR_TABLE}` is no refusal token of the code"),
            format!("the code's `pending_full` is missing under `{OPERATOR_TABLE}`"),
        ]
    );
    let twice = tables(|t| t.push("capacity".to_owned()));
    assert_eq!(
        token_problems(&twice),
        [format!(
            "`capacity` is listed twice under `{OPERATOR_TABLE}`"
        )]
    );
    let (_, host) = code_tokens();
    let mut with_heartbeat: Vec<&str> = host.iter().map(String::as_str).collect();
    with_heartbeat.push("invalid_heartbeat");
    let (operator, _) = code_tokens();
    let operator: Vec<&str> = operator.iter().map(String::as_str).collect();
    assert_eq!(
        token_problems(&(table(OPERATOR_TABLE, &operator) + &table(HOST_TABLE, &with_heartbeat))),
        [format!(
            "`invalid_heartbeat` under `{HOST_TABLE}` is no refusal token of the code"
        )]
    );
    assert_eq!(
        token_problems(&table(OPERATOR_TABLE, &operator)),
        [format!("no section `{HOST_TABLE}`")]
    );
    assert_eq!(
        token_problems(&(table(OPERATOR_TABLE, &operator) + HOST_TABLE + "\n\nno table\n")),
        [format!("the section `{HOST_TABLE}` has no table of tokens")]
    );
    let prose = tables(|_| {})
        + "It is refused `record_exists`, then refused: `host_id_taken`, then refused `no_such`.\n";
    assert_eq!(
        token_problems(&prose),
        ["`no_such` is written as a refusal and is none"]
    );
}
