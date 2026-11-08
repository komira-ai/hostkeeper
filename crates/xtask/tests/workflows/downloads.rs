//! The rule every download of a workflow is held to: each `curl` a `run:` runs carries the HTTPS
//! and failure guards (`--proto =https --proto-redir =https --tlsv1.2 --fail`) and retries a
//! transient failure (`--retry <n> --retry-delay <n> --retry-max-time <n> --retry-all-errors`),
//! and no `run:` downloads another way (`wget`, `gh release download`), which these flags
//! cannot reach.
//!
//! GitHub's release downloads answer a 504 from time to time. With `--fail`, curl ends such a
//! transfer with exit 22 or, on some builds and protocols, exit 56 (a receive error). Plain
//! `--retry` retries exit 22 on a 504 and not exit 56: curl reads the HTTP status only after a
//! result of 0 or 22. So `--retry-all-errors` is required too, bounded by `--retry` and
//! `--retry-max-time`. A retry writes the `--output` file (or, with `--remote-name-all`, each
//! file it names) again from the start, and a check of a digest or an attestation is a command
//! after the curl, which runs whatever curl retried.
//!
//! A `curl` is a word `curl` in a `run:`, on its own or after `(` or a backquote (`$(curl`), on a
//! line that is not a comment (lines ending in `\` are joined first). Its options are the words
//! after it up to `|`, `||`, `&&`, `;` or `)`. Options are matched in their long spelling,
//! exactly; every `--proto` and `--proto-redir` must have the value `=https` (curl takes the
//! last), and each retry count or time is a whole number above 0. `downloads_tests.rs` shows each
//! defect refused.

use super::yaml::{Node, Value};

/// The retry options that take a value, a whole number above 0.
const VALUED: [&str; 3] = ["--retry", "--retry-delay", "--retry-max-time"];

/// The words that end a command's options.
const ENDS: [&str; 5] = ["|", "||", "&&", ";", ")"];

/// One line per problem of the downloads in the workflow `doc`.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut runs = Vec::new();
    collect_runs(doc, &mut runs);
    runs.into_iter()
        .flat_map(|(line, run)| run_problems(line, run))
        .collect()
}

/// Every `run:` scalar of the tree, with its line.
pub fn collect_runs<'a>(node: &'a Node, out: &mut Vec<(usize, &'a str)>) {
    match &node.value {
        Value::Scalar { .. } => {}
        Value::Seq(items) => items.iter().for_each(|i| collect_runs(i, out)),
        Value::Map(entries) => {
            for (k, v) in entries {
                match (k.as_str(), v.as_str()) {
                    (Some("run"), Some(run)) => out.push((v.line(), run)),
                    _ => collect_runs(v, out),
                }
            }
        }
    }
}

/// The problems of the downloads of one `run:` on `line`.
fn run_problems(line: usize, run: &str) -> Vec<String> {
    let joined = run.replace("\\\n", " ");
    let mut problems = Vec::new();
    for text in joined.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let words: Vec<&str> = text.split_whitespace().collect();
        for (at, word) in words.iter().enumerate() {
            let command = word.rsplit(['(', '`']).next().unwrap_or_default();
            let rest = &words[at + 1..];
            let options = &rest[..rest
                .iter()
                .position(|w| ENDS.contains(w))
                .unwrap_or(rest.len())];
            if command == "wget"
                || (command == "gh" && options.starts_with(&["release", "download"]))
            {
                problems.push(format!(
                    "line {line}: `{}` downloads without curl's guards and retries: use curl",
                    if command == "wget" {
                        "wget"
                    } else {
                        "gh release download"
                    }
                ));
            }
            if command == "curl" {
                problems.extend(curl_problems(line, options));
            }
        }
    }
    problems
}

/// The problems of one `curl` with the words `options` after it.
fn curl_problems(line: usize, options: &[&str]) -> Vec<String> {
    let mut missing = Vec::new();
    let mut wrong = Vec::new();
    let value = |flag: &str| {
        options
            .iter()
            .position(|w| *w == flag)
            .map(|at| options.get(at + 1).copied().unwrap_or_default())
    };
    for flag in ["--proto", "--proto-redir"] {
        if value(flag).is_none() {
            missing.push(format!("`{flag} =https`"));
        }
        for (at, _) in options.iter().enumerate().filter(|(_, w)| **w == flag) {
            let given = options.get(at + 1).copied().unwrap_or_default();
            if given != "=https" {
                wrong.push(format!("`{flag}` is `{given}`, not `=https`"));
            }
        }
    }
    for flag in ["--tlsv1.2", "--fail", "--retry-all-errors"] {
        if !options.contains(&flag) {
            missing.push(format!("`{flag}`"));
        }
    }
    for flag in VALUED {
        match value(flag) {
            None => missing.push(format!("`{flag} <n>`")),
            Some(given) if given.parse::<u32>().is_ok_and(|n| n > 0) => {}
            Some(given) => wrong.push(format!("`{flag}` is `{given}`, not a whole number above 0")),
        }
    }
    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!("line {line}: a curl lacks {}", missing.join(", ")));
    }
    problems.extend(
        wrong
            .into_iter()
            .map(|w| format!("line {line}: a curl's {w}")),
    );
    problems
}
