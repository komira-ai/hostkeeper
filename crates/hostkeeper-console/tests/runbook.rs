//! `docs/operations/console.md`, the console runbook, against the parsers of the three files it
//! shows: every fenced `toml` block whose first line is the mark of one of the console's files
//! (`# /etc/hostkeeper-coordinator/console-operators.toml`, `...-targets.toml` or
//! `...-bmc-credential.toml`) is read by that file's parser ([`Operators::parse`],
//! [`Targets::parse`], [`BmcCredential::parse`]), which the coordinator reads the real file
//! with, and the runbook shows an example of each of the three. A marked block of no known file
//! fails too. A renamed or misspelled key, a name or MAC a parser refuses, a password over its
//! cap, or an example the runbook drops fails here.

use std::path::Path;

use hostkeeper_console::credential::BmcCredential;
use hostkeeper_console::operators::Operators;
use hostkeeper_console::targets::Targets;

/// The runbook, from this crate's directory.
const RUNBOOK: &str = "../../docs/operations/console.md";

/// What every mark begins with: the directory of the coordinator's files and the console's prefix.
const MARK_PREFIX: &str = "# /etc/hostkeeper-coordinator/console-";

/// The console's files, by the mark that begins an example of each.
const FILES: [(&str, File); 3] = [
    (
        "# /etc/hostkeeper-coordinator/console-operators.toml",
        File::Operators,
    ),
    (
        "# /etc/hostkeeper-coordinator/console-targets.toml",
        File::Targets,
    ),
    (
        "# /etc/hostkeeper-coordinator/console-bmc-credential.toml",
        File::Credential,
    ),
];

/// One of the console's three files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum File {
    Operators,
    Targets,
    Credential,
}

impl File {
    /// Why its parser refuses `text`, if it does: the parser's own error text, which names the
    /// file's flag (and, for the credential, none of its text).
    fn refusal(self, text: &str) -> Option<String> {
        match self {
            Self::Operators => Operators::parse(text).err().map(|e| e.to_string()),
            Self::Targets => Targets::parse(text).err().map(|e| e.to_string()),
            Self::Credential => BmcCredential::parse(text).err().map(|e| e.to_string()),
        }
    }
}

/// Every fenced `toml` block of `doc`, in order. A fence may be indented (a block inside a list
/// item); the block's lines lose the opening fence's indentation, and a line indented less (a
/// blank line) loses all of its own. A block still open at the end is returned with a final line
/// `<unterminated>`, which none of the three files parses as.
fn toml_blocks(doc: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, Vec<&str>)> = None;
    for line in doc.lines() {
        let text = line.trim_end();
        let fence = text.trim_start();
        let indent = text.len() - fence.len();
        open = match (open.take(), fence) {
            (None, "```toml") => Some((indent, Vec::new())),
            (Some((_, lines)), "```") => {
                blocks.push(lines.join("\n"));
                None
            }
            (Some((at, mut lines)), _) => {
                lines.push(text.get(at..).unwrap_or(fence));
                Some((at, lines))
            }
            (None, _) => None,
        };
    }
    if let Some((_, mut lines)) = open {
        lines.push("<unterminated>");
        blocks.push(lines.join("\n"));
    }
    blocks
}

/// Every marked example of `doc` its parser refuses, every marked block of no known file, and
/// every file no example shows.
fn problems(doc: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut shown = Vec::new();
    for block in toml_blocks(doc) {
        let first = block.lines().next().unwrap_or_default();
        match FILES.iter().find(|(mark, _)| *mark == first) {
            Some((_, file)) => {
                shown.push(*file);
                if let Some(why) = file.refusal(&block) {
                    problems.push(format!("{block}\n{why}"));
                }
            }
            None if first.starts_with(MARK_PREFIX) => {
                problems.push(format!("{first}: no console file has this mark"));
            }
            None => {}
        }
    }
    for (mark, file) in FILES {
        if !shown.contains(&file) {
            problems.push(format!("no example shows {mark}"));
        }
    }
    problems
}

/// A document holding `blocks` as fenced `toml` blocks, each after a line of prose.
fn document(blocks: &[&str]) -> String {
    blocks
        .iter()
        .map(|block| format!("Text.\n\n```toml\n{block}\n```\n"))
        .collect()
}

/// A good example of each of the three files.
const OPERATORS: &str =
    "# /etc/hostkeeper-coordinator/console-operators.toml\n[[operator]]\nname = \"alice\"";
const TARGETS: &str = "# /etc/hostkeeper-coordinator/console-targets.toml\n[[target]]\n\
                       mac = \"00:00:5e:00:53:02\"\naddress = \"192.0.2.12\"";
const CREDENTIAL: &str = "# /etc/hostkeeper-coordinator/console-bmc-credential.toml\n\
                          user = \"<bmc-user>\"\npassword = \"<long random>\"";

/// The runbook's examples of the operators, targets and BMC credential files are each read by
/// the parser the coordinator reads that file with, and it shows an example of each.
///
/// Catches a runbook whose example the coordinator would refuse at start, or that drops one.
#[test]
fn every_console_file_example_of_the_runbook_is_read_by_its_parser() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(RUNBOOK);
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {RUNBOOK}: {e}"));
    assert_eq!(problems(&doc), Vec::<String>::new(), "{RUNBOOK}");
}

/// A document with one good example of each file has no problem, whatever other `toml` blocks
/// and fenced blocks it holds.
///
/// Catches a check that fails a good runbook, or reads `serve.toml`'s keys as a console file.
#[test]
fn good_examples_and_unmarked_blocks_pass() {
    let doc = format!(
        "{}```sh\n{OPERATORS}\nnot toml\n```\n",
        document(&[
            OPERATORS,
            "console-targets-file = \"/x\"",
            TARGETS,
            CREDENTIAL
        ])
    );
    assert_eq!(problems(&doc), Vec::<String>::new());
}

/// An example its parser refuses is a problem holding the block and the parser's error, which
/// names the file's flag: an operator name a token cannot have, a MAC that is no MAC, a password
/// over the 20 bytes IPMI 2.0 takes, and a key the file does not have.
///
/// Catches a check that reads an example with the wrong parser, or not at all.
#[test]
fn an_example_its_parser_refuses_is_a_problem_naming_its_flag() {
    let cases = [
        (
            OPERATORS.replace("alice", "Alice"),
            "--console-operators-file: operator 1: the name \"Alice\" is not 1 to 32 bytes of a-z, 0-9 and -",
        ),
        (
            TARGETS.replace("00:00:5e:00:53:02", "00:00:5e:00:53"),
            "--console-targets-file: target 1: \"00:00:5e:00:53\" is not a MAC address",
        ),
        (
            CREDENTIAL.replace("<long random>", "<twenty one bytes!!!>"),
            "--console-bmc-credential-file: password must be 1 to 20 bytes of printable ASCII",
        ),
        (
            format!("{CREDENTIAL}\nport = 623"),
            "--console-bmc-credential-file: it is not TOML of exactly user and password",
        ),
    ];
    for (bad, why) in cases {
        let blocks = [OPERATORS, TARGETS, CREDENTIAL].map(|good| {
            if bad.lines().next() == good.lines().next() {
                bad.as_str()
            } else {
                good
            }
        });
        assert_eq!(problems(&document(&blocks)), [format!("{bad}\n{why}")]);
    }
}

/// A runbook that shows no example of a file is a problem naming that file's mark, for each of
/// the three.
///
/// Catches a check that is satisfied by any one example, or by none.
#[test]
fn a_file_with_no_example_is_a_problem() {
    for (index, (mark, _)) in FILES.iter().enumerate() {
        let mut blocks = vec![OPERATORS, TARGETS, CREDENTIAL];
        blocks.remove(index);
        assert_eq!(
            problems(&document(&blocks)),
            [format!("no example shows {mark}")]
        );
    }
    assert_eq!(problems("").len(), FILES.len());
}

/// A block marked as a console file the coordinator does not have is a problem, so a misspelled
/// mark is not passed over as prose.
///
/// Catches an example that silently stops being checked because its mark changed.
#[test]
fn a_marked_block_of_no_known_file_is_a_problem() {
    let stray = "# /etc/hostkeeper-coordinator/console-operator.toml\n[[operator]]\nname = \"a\"";
    assert_eq!(
        problems(&document(&[OPERATORS, TARGETS, CREDENTIAL, stray])),
        ["# /etc/hostkeeper-coordinator/console-operator.toml: no console file has this mark"]
    );
}

/// A block inside a list item, its fences and lines indented, is read with the indentation taken
/// off and a blank line in it kept; a block left open at the end of the text is read with a
/// last line no parser takes, so it fails rather than passing unread.
///
/// Catches an extraction that skips an example set inside a numbered step, or drops an
/// unterminated one.
#[test]
fn an_indented_block_is_read_and_an_unterminated_one_fails() {
    let indented = "1. Write it:\n\n   ```toml\n   [[operator]]\n\n   name = \"a\"\n   ```\n";
    assert_eq!(toml_blocks(indented), ["[[operator]]\n\nname = \"a\""]);
    let open = format!("{}```toml\n{CREDENTIAL}\n", document(&[OPERATORS, TARGETS]));
    assert_eq!(
        toml_blocks(&open).last(),
        Some(&format!("{CREDENTIAL}\n<unterminated>"))
    );
    assert_eq!(
        problems(&open),
        [format!(
            "{CREDENTIAL}\n<unterminated>\n--console-bmc-credential-file: it is not TOML of exactly user and password"
        )]
    );
}
