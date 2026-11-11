//! `docs/operations/network-install.md`, the operator runbook, against the settings code: every
//! example settings file in it is a fenced block with the info string `toml` whose first line is
//! exactly `# /etc/pxe/pxe.toml`, and each is read by the code `serve --settings` and `prepare`
//! read the real file with ([`settings::parse`], then the parser of `serve` with a `--dir`, and
//! that of `[prepare]`). Every block must read, and among the blocks there must be one in proxy
//! mode, one in router mode, one with `reinstall`, and one with a `[prepare]` table that names
//! `platform`. The runbook shows no `pxe serve` or `pxe prepare` command line to type: the unit
//! and the daemon run them. A renamed or misspelled key, a value a flag does not take, or a
//! required flag the runbook leaves out fails here.

use std::collections::BTreeSet;
use std::path::Path;

use clap::Parser;

use crate::cli::{ModeFlag, serve_flags};
use crate::hook::prepare::PrepareFlags;
use crate::settings;

/// The runbook, from this crate's directory.
const RUNBOOK: &str = "../../docs/operations/network-install.md";

/// The first line of an example settings file.
const MARK: &str = "# /etc/pxe/pxe.toml";

/// The kinds of settings file the runbook must show.
const KINDS: [&str; 4] = ["proxy", "router", "reinstall", "prepare platform"];

/// The example settings files of `doc`, in order. A fence may be indented (a block inside a list
/// item); the block's lines lose the opening fence's indentation, and a line indented less (a
/// blank line) loses all of its own. A block still open at the end is returned with a final line
/// `<unterminated>`, which no settings file parses as.
fn examples(doc: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, Vec<&str>)> = None;
    for line in doc.lines() {
        let text = line.trim_end();
        let fence = text.trim_start();
        let indent = text.len() - fence.len();
        open = match (open.take(), fence) {
            (None, "```toml") => Some((indent, Vec::new())),
            (Some((_, lines)), "```") => {
                if lines.first() == Some(&MARK) {
                    blocks.push(lines.join("\n"));
                }
                None
            }
            (Some((at, mut lines)), _) => {
                lines.push(text.get(at..).unwrap_or(fence));
                Some((at, lines))
            }
            (None, _) => None,
        };
    }
    if let Some(mut lines) = open
        .map(|(_, lines)| lines)
        .filter(|lines| lines.first() == Some(&MARK))
    {
        lines.push("<unterminated>");
        blocks.push(lines.join("\n"));
    }
    blocks
}

/// The kinds `block` is, or why it does not read.
fn kinds_of(block: &str) -> Result<BTreeSet<&'static str>, String> {
    let file = settings::parse(block)?;
    let serve = serve_flags(
        Path::new("/etc/pxe/pxe.toml"),
        &file.serve,
        Path::new("/var/lib/pxe/current"),
    )?;
    let prepare = PrepareFlags::try_parse_from(&file.prepare)
        .map_err(|e| format!("[prepare]: {}", crate::cli::one_line(&e)))?;
    let mut kinds = BTreeSet::new();
    kinds.insert(match serve.mode {
        ModeFlag::Proxy => "proxy",
        ModeFlag::Router => "router",
    });
    if !serve.reinstall.is_empty() {
        kinds.insert("reinstall");
    }
    if !prepare.platforms.is_empty() {
        kinds.insert("prepare platform");
    }
    Ok(kinds)
}

/// Every block of `doc` that does not read, and every kind of [`KINDS`] no block shows.
fn problems(doc: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut shown = BTreeSet::new();
    for block in examples(doc) {
        match kinds_of(&block) {
            Ok(kinds) => shown.extend(kinds),
            Err(why) => problems.push(format!("{block}\n{why}")),
        }
    }
    for kind in KINDS {
        if !shown.contains(kind) {
            problems.push(format!("no example settings file shows {kind}"));
        }
    }
    problems
}

/// A settings file of `mode` with `extra` lines in `[serve]` and `prepare` as its `[prepare]`
/// table.
fn settings_file(mode: &str, extra: &str, prepare: &str) -> String {
    format!(
        "{MARK}\n[serve]\nmode = \"{mode}\"\naddress = \"192.0.2.10\"\n\
         coordinator = \"https://192.0.2.1:8444\"\nca-file = \"/etc/pxe/ca.pem\"\n\
         token-file = \"/etc/pxe/token\"\ndaemon-config = \"/etc/hostkeeper/hostkeeper.toml\"\n\
         ssh-authorized-keys = \"/etc/pxe/authorized_keys\"\nuser-id = 65534\ngroup-id = 65534\n\
         {extra}{prepare}"
    )
}

/// A document holding `blocks` as fenced `toml` blocks.
fn document(blocks: &[String]) -> String {
    blocks
        .iter()
        .map(|block| format!("Text.\n\n```toml\n{block}\n```\n"))
        .collect()
}

/// A document with a block of each kind.
fn every_kind() -> Vec<String> {
    vec![
        settings_file("proxy", "", ""),
        settings_file(
            "router",
            "reinstall = [\"web-17\"]\n",
            "\n[prepare]\nplatform = [\"linux-arm64\"]\n",
        ),
    ]
}

/// The runbook's example settings files all read, and it shows each kind of file.
///
/// Catches a runbook that shows a key `pxe` would refuse, or leaves out an example it promises.
#[test]
fn the_runbook_settings_files_read() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(RUNBOOK);
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {RUNBOOK}: {e}"));
    let problems = problems(&doc);
    assert!(problems.is_empty(), "{RUNBOOK}:\n{}", problems.join("\n"));
}

/// Blocks with the mark are read and the rest are not: a block of another info string, one that
/// does not begin with the mark, and prose that mentions the mark.
///
/// Catches an extraction that reads a command or another file's example as a settings file.
#[test]
fn only_marked_toml_blocks_are_examples() {
    let marked = settings_file("proxy", "", "");
    let doc = format!(
        "{MARK}\n```\n{MARK}\nnot toml\n```\n```toml\nmode = 3\n```\n```toml\n{MARK}\nmode = \"x\"\n```\n{}",
        document(std::slice::from_ref(&marked))
    );
    assert_eq!(
        examples(&doc),
        [format!("{MARK}\nmode = \"x\""), marked.clone()]
    );
    assert_eq!(examples(&format!("```toml\n{marked}\n")).len(), 1);
    assert_eq!(kinds_of(&marked), Ok(BTreeSet::from(["proxy"])));
}

/// A block inside a list item, its fences and lines indented, is read with the indentation taken
/// off, a blank line in it kept blank.
///
/// Catches an extraction that skips the runbook's examples set inside a numbered step.
#[test]
fn an_indented_block_is_read_without_its_indentation() {
    let doc = format!(
        "1. Write the file:\n\n   ```toml\n   {MARK}\n   [serve]\n\n   mode = \"router\"\n   ```\n"
    );
    assert_eq!(
        examples(&doc),
        [format!("{MARK}\n[serve]\n\nmode = \"router\"")]
    );
}

/// A document with a block of each kind has no problem.
///
/// Catches a check that fails a good runbook.
#[test]
fn a_document_with_each_kind_has_no_problem() {
    assert_eq!(problems(&document(&every_kind())), Vec::<String>::new());
}

/// A block the parsers refuse is a problem that names the block and the reason: a key `serve`
/// does not take, a mode it does not know, a file that is not TOML, a flag it needs left out, a
/// value of a type no flag takes, a `[prepare]` key that is not a flag, a block never closed.
///
/// Catches the check passing a misspelled key or a value a flag does not take.
#[test]
fn a_block_the_parsers_refuse_is_a_problem() {
    let cases = [
        (
            settings_file("proxy", "reinstal = [\"web-17\"]\n", ""),
            "unexpected argument '--reinstal' found",
        ),
        (
            settings_file("both", "", ""),
            "invalid value 'both' for '--mode <MODE>' [possible values: proxy, router]",
        ),
        (format!("{MARK}\n[serve\n"), "unclosed table, expected `]`"),
        (
            format!("{MARK}\n[serve]\nmode = \"proxy\"\n"),
            "the following required arguments were not provided: --address <ADDRESS> --coordinator <COORDINATOR> --ca-file <CA_FILE> --token-file <TOKEN_FILE> --daemon-config <DAEMON_CONFIG> --ssh-authorized-keys <SSH_AUTHORIZED_KEYS> --user-id <USER_ID> --group-id <GROUP_ID>",
        ),
        (
            settings_file("proxy", "interface = true\n", ""),
            "[serve] \"interface\": a value is a string or an integer, or an array of them",
        ),
        (
            settings_file("proxy", "", "\n[prepare]\nplatforms = [\"linux-arm64\"]\n"),
            "[prepare]: unexpected argument '--platforms' found",
        ),
    ];
    for (block, why) in cases {
        let found = problems(&(document(&every_kind()) + &document(std::slice::from_ref(&block))));
        assert_eq!(found.len(), 1, "{found:?}");
        let expected = format!("{block}\n");
        assert!(found[0].starts_with(&expected), "{found:?}");
        assert!(
            found[0][expected.len()..].contains(why),
            "{}: {why}",
            found[0]
        );
    }
    let cut = problems(&(document(&every_kind()) + &format!("```toml\n{MARK}\n[serve]\n")));
    assert_eq!(cut.len(), 1, "{cut:?}");
}

/// A runbook that leaves out a kind of settings file has a problem that names it.
///
/// Catches a runbook that stops showing proxy mode, router mode, `reinstall` or a `[prepare]`
/// platform.
#[test]
fn a_missing_kind_is_a_problem() {
    let proxy = settings_file("proxy", "", "");
    let router = settings_file("router", "", "");
    let reinstall = settings_file("proxy", "reinstall = [\"web-17\"]\n", "");
    let platform = settings_file("proxy", "", "\n[prepare]\nplatform = [\"linux-x86_64\"]\n");
    let all = [proxy, router, reinstall, platform];
    for (left_out, kind) in [(1, "router"), (2, "reinstall"), (3, "prepare platform")] {
        let blocks: Vec<String> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != left_out)
            .map(|(_, b)| b.clone())
            .collect();
        let found = problems(&document(&blocks));
        let want = format!("no example settings file shows {kind}");
        assert_eq!(found, [want], "{kind}");
    }
    assert_eq!(
        problems("no blocks"),
        KINDS.map(|k| format!("no example settings file shows {k}"))
    );
}
