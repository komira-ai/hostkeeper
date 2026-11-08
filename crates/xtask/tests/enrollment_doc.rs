//! The operator docs against `hkctl`, the command line they name, and
//! `docs/operations/enrollment.md`, the runbook for enrolling a node, and
//! `docs/operations/network-install.md`, the runbook for installing one from the network, against
//! the code they describe.
//!
//! - Every command line in a fenced block that begins `hkctl` is read as hkctl reads it
//!   ([`hkctl::parse`]): its global flags are those of `docs/cli.md`'s table, and its service,
//!   method and flags those of the command table hkctl is generated from, the one `docs/cli.md`
//!   is written from. A renamed RPC or flag, a flag hkctl does not have (`hostkeeper ctl`'s
//!   `--output`), a value a bool, enum or integer flag does not take, or a call without the
//!   endpoint, CA file and token it needs, fails here. A line continued with a trailing `\` is
//!   one line; words are split as a shell splits them, by white space, with `'...'` and `"..."`
//!   quoting.
//! - This holds in every operator doc: each Markdown file of the repository but the design
//!   documents (`docs/design/`, which also describe methods not built yet). There, an inline
//!   code span that begins `hkctl` names a service and a method hkctl has, and flags that method
//!   or hkctl has; and no fenced line begins `hostkeeper ctl`, but in the files of
//!   [`OLD_CLI_LEFT`], each of which must still hold one.
//! - The enrollment runbook uses the calls of the procedure: `add-record`, `list-records`,
//!   `remove-record`, `open-record`, `list` and `approve` of `enrollment`.
//! - The tokens of its two refusal tables are exactly the refusal tokens of the protos: the
//!   table under `## Refusals of the operator calls` is every value of `EnrollmentRefusal`, and
//!   the one under `## Refusals of the host's calls` every value of `CoordinatorRefusal` but the
//!   heartbeat's own `invalid_heartbeat`. A token is the value's name without its enum prefix, in
//!   lower case, as the coordinator sends it. Anywhere else, a token written after "refused"
//!   (``refused `x` `` or ``refused: `x` ``) must be one of them.
//! - The network-install runbook uses the calls of its two steps per machine (`add-record`,
//!   `list-records`, `list` and `approve` of `enrollment`), and a token it writes after
//!   "refused" is one of the protos' refusal tokens.
//!
//! The checks are pure functions of the text, each tested on edited texts as well as on the real
//! files. The files are read from the repository; a missing runbook fails.

#[path = "enrollment_doc/hkctl.rs"]
mod hkctl;
#[path = "../src/testdir.rs"]
mod testdir;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hkctl::{Global, Parsed};
use hostkeeper_proto::ctl::{COMMANDS, Command, Input};
use prost_reflect::DescriptorPool;

/// The runbook, from the repository root.
const DOC: &str = "docs/operations/enrollment.md";

/// The network-install runbook, from the repository root.
const NETWORK_INSTALL_DOC: &str = "docs/operations/network-install.md";

/// The design documents, from the repository root: not operator docs.
const DESIGN_DIR: &str = "docs/design/";

/// The operator docs that still hold a fenced `hostkeeper ctl` line, each with why. Each must
/// hold one, so that the entry goes when the line does.
const OLD_CLI_LEFT: &[(&str, &str)] = &[(
    "docs/deploy/ci-client.md",
    "its section `The calls` is what `hostkeeper ci` runs in its own process, until \
     set-desired.yml calls hkctl",
)];

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

/// The fenced lines whose first two words are `first`.
fn lines_of(doc: &str, first: &[&str]) -> Vec<String> {
    fenced_lines(doc)
        .into_iter()
        .filter(|l| {
            l.split_whitespace()
                .take(first.len())
                .eq(first.iter().copied())
        })
        .collect()
}

/// The fenced lines that call `hkctl`.
fn hkctl_lines(doc: &str) -> Vec<String> {
    lines_of(doc, &["hkctl"])
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

/// hkctl's global flags, from `docs/cli.md` as the generator writes it.
fn globals() -> Vec<Global> {
    hkctl::globals_of(&hostkeeper_hkctl::markdown(COMMANDS)).expect("the global flags")
}

/// The command `line` calls, as `service rpc`, or why hkctl refuses it; `None` for `--help` and
/// `--version`.
fn command_of(line: &str) -> Result<Option<String>, String> {
    let words = words(line)?;
    match hkctl::parse(&hkctl::served(COMMANDS), &globals(), &words[1..]) {
        Ok(Parsed::Call(c)) => Ok(Some(c)),
        Ok(Parsed::Help | Parsed::Version) => Ok(None),
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
    for line in hkctl_lines(doc) {
        match command_of(&line) {
            Ok(command) => called.extend(command),
            Err(e) => problems.push(e),
        }
    }
    for required in required {
        if !called.contains(*required) {
            problems.push(format!("no command line calls `hkctl {required}`"));
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

/// The repository's root.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The operator docs: every `.md` file under `root` but those of [`DESIGN_DIR`], as paths from
/// `root` with `/`, in order. A directory whose name starts with `.` (git's, an editor's) and the
/// build's `target` are not walked.
fn operator_docs(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("read {dir:?}: {e}"));
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry
                .file_type()
                .unwrap_or_else(|e| panic!("stat {path:?}: {e}"));
            if kind.is_dir() {
                if !name.starts_with('.') && name != "target" {
                    walk(root, &path, out);
                }
            } else if kind.is_file() && name.ends_with(".md") {
                let rel = path.strip_prefix(root).expect("under the root");
                let rel: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let rel = rel.join("/");
                if !rel.starts_with(DESIGN_DIR) {
                    out.push(rel);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The text of every inline code span of `doc` outside its fenced blocks, white space (a line
/// break among it) made one space: a run of backticks opens a span that the next run of as many
/// closes.
fn inline_spans(doc: &str) -> Vec<String> {
    let mut prose = String::new();
    let mut fenced = false;
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    let mut spans = Vec::new();
    let mut rest = prose.as_str();
    while let Some(open) = rest.find('`') {
        let after = &rest[open..];
        let ticks = after.len() - after.trim_start_matches('`').len();
        let body = &after[ticks..];
        let fence = "`".repeat(ticks);
        let close = body.match_indices(&fence).find(|(at, _)| {
            !body[at + ticks..].starts_with('`') && (*at == 0 || !body[..*at].ends_with('`'))
        });
        let Some((at, _)) = close else { break };
        spans.push(body[..at].split_whitespace().collect::<Vec<_>>().join(" "));
        rest = &body[at + ticks..];
    }
    spans
}

/// The flags of `command` on hkctl's command line: each field's own, `--request`, and `--help`.
fn flags_of(command: &Command) -> Vec<String> {
    let mut flags = vec!["--request".to_owned(), "--help".to_owned()];
    for field in command.fields {
        match field.input {
            Input::Flag => flags.push(format!("--{}", field.flag)),
            Input::SecretFile => {
                flags.push(format!("--{}-file", field.flag));
                flags.push(format!("--{}-stdin", field.flag));
            }
            Input::RequestFile => {}
        }
    }
    flags
}

/// What is wrong with an inline span `span` that begins `hkctl`: a service or method hkctl does
/// not have, or a flag neither the method nor hkctl has. A span whose first word after the
/// global flags is not a name of the command grammar (`[a-z][a-z0-9-]*`: a version, a
/// placeholder such as `<service>`) is no command; `...` stands for global flags before the
/// service and for the rest after it; values are passed over.
fn span_problems(span: &str, globals: &[Global]) -> Vec<String> {
    let commands = hkctl::served(COMMANDS);
    let is_global = |w: &str| {
        let flag = w.split('=').next().unwrap_or(w);
        matches!(flag, "--help" | "-h" | "--version")
            || globals
                .iter()
                .any(|g| flag.strip_prefix("--") == Some(g.name.as_str()))
    };
    let takes_value = |w: &str| {
        !w.contains('=')
            && globals
                .iter()
                .any(|g| g.value && w.strip_prefix("--") == Some(g.name.as_str()))
    };
    let mut words = span.split_whitespace().skip(1).peekable();
    let mut names: Vec<String> = Vec::new();
    while names.len() < 2 {
        let Some(word) = words.next() else { break };
        if word == "..." {
            if names.is_empty() {
                continue;
            }
            return Vec::new();
        }
        if word.starts_with('-') {
            if takes_value(word) {
                words.next();
            }
            continue;
        }
        let mut chars = word.chars();
        let is_name = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !is_name {
            return Vec::new();
        }
        names.push(word.to_owned());
    }
    let Some(service) = names.first() else {
        return Vec::new();
    };
    if !commands.iter().any(|c| c.service_command == service) {
        return vec![format!("`{span}`: hkctl has no service {service}")];
    }
    let Some(method) = names.get(1) else {
        return Vec::new();
    };
    let Some(command) = commands
        .iter()
        .find(|c| c.service_command == service && c.method_command == method)
    else {
        return vec![format!(
            "`{span}`: the service {service} has no method {method}"
        )];
    };
    let flags = flags_of(command);
    words
        .filter(|w| w.starts_with("--"))
        .map(|w| w.split('=').next().unwrap_or(w))
        .filter(|f| !flags.iter().any(|g| g == f) && !is_global(f))
        .map(|f| format!("`{span}`: {service} {method} has no flag {f}"))
        .collect()
}

/// Every problem of the operator doc `path` (from the root) whose text is `doc`: a fenced `hkctl`
/// line hkctl refuses, an inline `hkctl` span that names what hkctl does not have, and a fenced
/// `hostkeeper ctl` line where [`OLD_CLI_LEFT`] does not keep one, or none where it does.
fn doc_problems(path: &str, doc: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for line in hkctl_lines(doc) {
        if let Err(e) = command_of(&line) {
            problems.push(format!("{path}: {e}"));
        }
    }
    let globals = globals();
    for span in inline_spans(doc) {
        if span == "hkctl" || span.starts_with("hkctl ") {
            problems.extend(
                span_problems(&span, &globals)
                    .into_iter()
                    .map(|p| format!("{path}: {p}")),
            );
        }
    }
    let old = lines_of(doc, &["hostkeeper", "ctl"]);
    let left = OLD_CLI_LEFT.iter().any(|(p, _)| *p == path);
    match (old.first(), left) {
        (Some(line), false) => problems.push(format!(
            "{path}: a fenced `hostkeeper ctl` line; the operator docs name hkctl: {line}"
        )),
        (None, true) => problems.push(format!(
            "{path}: holds no fenced `hostkeeper ctl` line; take it out of OLD_CLI_LEFT"
        )),
        _ => {}
    }
    problems
}

/// Catches an operator doc (a runbook, README, CONTRIBUTING, a reference page) whose fenced
/// `hkctl` line hkctl would refuse as a usage error, whose inline `hkctl` span names a service,
/// method or flag hkctl does not have, or that still gives a `hostkeeper ctl` command.
#[test]
fn every_operator_doc_names_hkctl() {
    let root = repo_root();
    let docs = operator_docs(&root);
    for needed in [DOC, NETWORK_INSTALL_DOC, "README.md", "CONTRIBUTING.md"] {
        assert!(
            docs.iter().any(|d| d == needed),
            "{needed} not found: {docs:?}"
        );
    }
    assert!(!docs.iter().any(|d| d.starts_with(DESIGN_DIR)));
    let mut problems = Vec::new();
    let mut lines = 0;
    for path in &docs {
        let doc =
            std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
        lines += hkctl_lines(&doc).len();
        problems.extend(doc_problems(path, &doc));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(lines > REQUIRED.len(), "{lines} fenced hkctl lines");
}

/// The globals a call needs, as the runbooks write them.
const CALL: &str =
    "hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file>";

/// A fenced block holding the call `rest` with [`CALL`]'s globals.
fn fenced_call(rest: &str) -> String {
    fenced(&format!("{CALL} {rest}"))
}

/// Catches the doc check passing a fenced `hostkeeper ctl` line, a line hkctl refuses, an inline
/// span with a misspelled method or a flag of `hostkeeper ctl` only; an entry of
/// [`OLD_CLI_LEFT`] kept after its file's last line went; and catches it refusing a placeholder,
/// a span cut by a line break, or a fenced line that only mentions hkctl.
#[test]
fn the_doc_check_refuses_each_defect() {
    let good = fenced_call("hosts get --node-id <id>")
        + "Run `hkctl hosts get\n--node-id <id>`, `hkctl <service> <method>`, `hkctl`,\n"
        + "`hkctl --endpoint https://<l> releases ... --service api`, `hkctl --help` and \n"
        + "``hkctl enrollment approve --replace=true ``, `hkctl 0.1.2+abc`; see `hostkeeper ctl`.\n"
        + &fenced("echo hkctl nothing")
        + &fenced("hkctl --version");
    assert_eq!(doc_problems("docs/x.md", &good), Vec::<String>::new());
    let cases = [
        (
            fenced("hostkeeper ctl hosts get --node-id n"),
            "docs/x.md: a fenced `hostkeeper ctl` line; the operator docs name hkctl: \
             hostkeeper ctl hosts get --node-id n",
        ),
        (
            fenced("hkctl hosts get --node-id n"),
            "docs/x.md: hkctl hosts get --node-id n\na call needs --endpoint",
        ),
        (
            "`hkctl hosts gt --node-id n`".to_owned(),
            "docs/x.md: `hkctl hosts gt --node-id n`: the service hosts has no method gt",
        ),
        (
            "`hkctl host get`".to_owned(),
            "docs/x.md: `hkctl host get`: hkctl has no service host",
        ),
        (
            "`hkctl --output json\nhosts get --node-id n`".to_owned(),
            "docs/x.md: `hkctl --output json hosts get --node-id n`: hkctl has no service json",
        ),
        (
            "`hkctl hosts get --transport=gateway`".to_owned(),
            "docs/x.md: `hkctl hosts get --transport=gateway`: hosts get has no flag --transport",
        ),
    ];
    for (doc, problem) in cases {
        assert_eq!(doc_problems("docs/x.md", &doc), [problem], "{doc}");
    }
    let (left, _) = OLD_CLI_LEFT[0];
    assert_eq!(
        doc_problems(left, &fenced("hostkeeper ctl hosts get --node-id n")),
        Vec::<String>::new()
    );
    assert_eq!(
        doc_problems(left, &fenced_call("hosts get --node-id n")),
        [format!(
            "{left}: holds no fenced `hostkeeper ctl` line; take it out of OLD_CLI_LEFT"
        )]
    );
}

/// Catches the walk of the operator docs reading a design document, a file that is not
/// Markdown, or a hidden or `target` directory, and listing them out of order.
#[test]
fn the_operator_docs_are_every_markdown_file_but_the_design() {
    let root = testdir::TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "enrollment-doc");
    for file in [
        "README.md",
        "docs/b.md",
        "docs/a.md",
        "docs/design/d.md",
        "docs/designs.md",
        "docs/notes.txt",
        ".git/x.md",
        "target/x.md",
        "crates/c/README.md",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, "x").expect("write");
    }
    assert_eq!(
        operator_docs(&root),
        [
            "README.md",
            "crates/c/README.md",
            "docs/a.md",
            "docs/b.md",
            "docs/designs.md"
        ]
    );
}

/// Catches a command line of the runbook that hkctl would refuse as a usage error (a renamed or
/// misspelled RPC or flag, a value a flag does not take, a call without its globals), a step of
/// the procedure with no command, and a refusal token the code does not define or the runbook
/// leaves out.
#[test]
fn enrollment_doc_commands_parse() {
    let path = repo_root().join(DOC);
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {DOC}: {e}"));
    let mut problems = command_problems(&doc);
    problems.extend(token_problems(&doc));
    assert!(problems.is_empty(), "{DOC}:\n{}", problems.join("\n"));
    assert!(hkctl_lines(&doc).len() >= REQUIRED.len());
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

/// Catches an `hkctl` line of the network-install runbook that hkctl would refuse as a usage
/// error, a step of it with no command, and a refusal token the code does not define.
#[test]
fn network_install_doc_commands_parse() {
    let path = repo_root().join(NETWORK_INSTALL_DOC);
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
        .map(|r| fenced_call(r))
        .collect();
    assert_eq!(network_install_problems(&all), Vec::<String>::new());
    let misspelled = all.clone() + &fenced_call("enrollment list-record");
    let problems = network_install_problems(&misspelled);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!("{CALL} enrollment list-record\n")),
        "{problems:?}"
    );
    let without_approve: String = NETWORK_INSTALL_REQUIRED
        .iter()
        .filter(|r| **r != "enrollment approve")
        .map(|r| fenced_call(r))
        .collect();
    assert_eq!(
        network_install_problems(&without_approve),
        ["no command line calls `hkctl enrollment approve`"]
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
    REQUIRED.iter().map(|r| fenced_call(r)).collect()
}

/// Catches the parser accepting a misspelled service, RPC or flag, a bad value, a flag of
/// `hostkeeper ctl` only, or a call without its globals; and a line cut at its `\` read as two.
#[test]
fn a_misspelled_command_is_refused() {
    for line in [
        format!("{CALL} enrollment aprove --request-id r"),
        format!("{CALL} enrolment list"),
        format!("{CALL} enrollment approve --key-hash k"),
        format!("{CALL} enrollment approve --replace yes"),
        format!("{CALL} enrollment list --limit ten"),
        format!("{CALL} --output yaml enrollment list"),
        format!("{CALL} --transport gateway enrollment list"),
        format!("{CALL} enrollment"),
        "hkctl enrollment list".to_owned(),
        "hkctl --endpoint https://c.example.com --ca-file c enrollment list".to_owned(),
    ] {
        let problems = command_problems(&(required_calls() + &fenced(&line)));
        assert_eq!(problems.len(), 1, "{line}: {problems:?}");
        assert!(problems[0].starts_with(&line), "{line}: {problems:?}");
    }
    let continued =
        format!("{CALL} \\\n    enrollment approve \\\n    --replace true --reason \"two words\"");
    assert_eq!(
        command_problems(&(required_calls() + &fenced(&continued))),
        Vec::<String>::new()
    );
    assert_eq!(
        hkctl_lines(&fenced(&continued)),
        [format!(
            "{CALL} enrollment approve --replace true --reason \"two words\""
        )]
    );
    // A block whose last line is continued neither runs on into the next block nor is lost.
    let cut = "hkctl enrollment list \\";
    for doc in [fenced(cut) + &fenced_call("enrollment list"), fenced(cut)] {
        assert_eq!(hkctl_lines(&doc)[0], "hkctl enrollment list \\");
        let problems = command_problems(&(required_calls() + &doc));
        assert_eq!(problems, ["a trailing \\ in: hkctl enrollment list \\"]);
    }
    assert_eq!(command_of("hkctl enrollment approve --help"), Ok(None));
    assert_eq!(command_of("hkctl --version"), Ok(None));
}

/// Catches a runbook that drops a step of the procedure, and command lines read from outside a
/// fenced block or from a line that only mentions hkctl.
#[test]
fn every_step_of_the_procedure_has_a_command() {
    let without_approve: String = REQUIRED
        .iter()
        .filter(|r| **r != "enrollment approve")
        .map(|r| fenced_call(r))
        .collect();
    let outside = "Run `hkctl enrollment approve` and\nhkctl enrollment approve --bad\n";
    let doc = without_approve + outside + &fenced("echo hkctl enrollment approve --bad");
    assert_eq!(
        command_problems(&doc),
        ["no command line calls `hkctl enrollment approve`"]
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
    assert!(command_of("hkctl enrollment list --page-token 'x").is_err());
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
