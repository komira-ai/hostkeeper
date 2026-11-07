//! The scan of a bash script that holds hkctl to what the design promises of it
//! (`docs/design/two-programs.md` section 2.4):
//!
//! - [`newer_than_bash_3_2`]: every construct bash added after 3.2, the bash macOS ships
//!   (associative arrays, `mapfile`, case conversion, `;&`, `|&`, ...);
//! - [`secret_hazards`]: an `export` (a variable a child would inherit) and a here-document or
//!   here-string (bash may write one to a temporary file).
//!
//! Both read the script's code: what is inside single quotes (the help texts) and comments is
//! left out, so a word in a help text is not a construct; a `printf` format, which is quoted, is
//! read on its whole line. Each finding names the line.

use std::sync::LazyLock;

use regex::Regex;

/// One construct a scan refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The line it is on, from 1.
    pub line: usize,
    /// What it is, and why it is refused.
    pub construct: &'static str,
}

/// Each construct newer than bash 3.2, as a pattern of its code and what it is.
const NEWER: [(&str, &str); 16] = [
    (
        r"\b(declare|local|typeset|readonly)\s+(-[A-Za-z]+\s+)*-[A-Za-z]*[AnglucI]",
        "declare -A, -n, -g, -l, -u, -c or -I (bash 4 and later)",
    ),
    (
        r"\bmapfile\b|\breadarray\b",
        "mapfile or readarray (bash 4.0)",
    ),
    (r"\bcoproc\b", "coproc (bash 4.0)"),
    (r"\bcompopt\b", "compopt (bash 4.0)"),
    (
        r"\$\{[#!]?[A-Za-z_][A-Za-z0-9_]*(\[[^\]]*\])?(\^|,)",
        "case conversion ${x^}, ${x^^}, ${x,} or ${x,,} (bash 4.0)",
    ),
    (
        r"\$\{[^}]*@[QEPAaKkUuL]\}",
        "a ${x@op} transformation (bash 4.4)",
    ),
    (r";;&|;&", "case fall-through ;& or ;;& (bash 4.0)"),
    (r"\|&", "|& (bash 4.0)"),
    (r"&>>", "&>> (bash 4.0)"),
    (
        r"\{[A-Za-z_][A-Za-z0-9_]*\}\s*[<>]",
        "{var}> file descriptor allocation (bash 4.1)",
    ),
    (
        r"\b(BASHPID|EPOCHSECONDS|EPOCHREALTIME|SRANDOM|BASH_ARGV0|BASH_COMPAT|READLINE_ARGUMENT|BASH_LOADABLES_PATH)\b",
        "a variable bash added after 3.2",
    ),
    (
        r"\bwait\s+(-[A-Za-z]+\s+)*-[A-Za-z]*[nfp]",
        "wait -n, -f or -p (bash 4.3 and later)",
    ),
    (
        r"\bread\s+([^;|&]*\s)?-[A-Za-z]*[iN]",
        "read -i or -N (bash 4.0, 4.1)",
    ),
    (
        r"(\[|\btest)\s+-[vR]\s",
        "the -v or -R test (bash 4.2, 4.3)",
    ),
    (
        r"\bshopt\s+-[su]\s+.*\b(globstar|lastpipe|autocd|checkjobs|dirspell|direxpand|globasciiranges|inherit_errexit|localvar_inherit|localvar_unset|assoc_expand_once|progcomp_alias|complete_fullquote|patsub_replacement|noexpand_translation|varredir_close|compat3[12]|compat4[0-4])\b",
        "a shopt option bash added after 3.2",
    ),
    (
        r"\$\{[A-Za-z_][A-Za-z0-9_]*\[-|\{[^{}\s]*\.\.[^{}\s]*\.\.[^{}\s]*\}|\blocal\s+-(\s|$)",
        "a negative subscript, a brace expansion with a step or local - (bash 4.0 and later)",
    ),
];

/// Each construct newer than bash 3.2 that sits inside quotes, as a pattern of the code, one of
/// the whole line (quotes and all) and what it is: a `printf` whose format has `%(...)T`.
const NEWER_QUOTED: [(&str, &str, &str); 1] =
    [(r"\bprintf\b", r"%\([^)]*\)T", "printf %(...)T (bash 4.2)")];

/// Each way a script could leave a secret behind, as a pattern of its code and what it is.
const HAZARDS: [(&str, &str); 2] = [
    (
        r"\bexport\b|\b(declare|local|typeset)\s+(-[A-Za-z]+\s+)*-[A-Za-z]*x",
        "an export: a child would inherit the variable",
    ),
    (
        r#"<<<|<<-?\s*['"A-Za-z_\\]"#,
        "a here-document or here-string: bash may write it to a temporary file",
    ),
];

/// The patterns, compiled once.
fn compiled(table: &[(&str, &'static str)]) -> Vec<(Regex, &'static str)> {
    table
        .iter()
        .filter_map(|(pattern, what)| Regex::new(pattern).ok().map(|re| (re, *what)))
        .collect()
}

static NEWER_RE: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| compiled(&NEWER));
static NEWER_QUOTED_RE: LazyLock<Vec<(Regex, Regex, &'static str)>> = LazyLock::new(|| {
    NEWER_QUOTED
        .iter()
        .filter_map(|(code, line, what)| {
            Some((Regex::new(code).ok()?, Regex::new(line).ok()?, *what))
        })
        .collect()
});
static HAZARDS_RE: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| compiled(&HAZARDS));

/// Where the reader of [`code`] is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    /// `'...'`: no escape inside.
    Single,
    /// `$'...'`: a backslash escapes the next character.
    Ansi,
    /// `"..."`: kept, it holds code (`$(...)`, `${...}`).
    Double,
}

/// The code of `script`, one entry per line: each single-quoted or `$'...'` text is `''` and each
/// comment is gone; double-quoted text, which holds expansions, is kept.
pub(crate) fn code(script: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut quote = Quote::None;
    let mut comment = false;
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            lines.push(std::mem::take(&mut current));
            comment = false;
            continue;
        }
        if comment {
            continue;
        }
        // Outside single quotes a backslash takes the next character with it, but never the
        // line break.
        let escaped = if c == '\\' && quote != Quote::Single {
            chars.next_if(|n| *n != '\n')
        } else {
            None
        };
        match quote {
            Quote::Single if c == '\'' => quote = Quote::None,
            Quote::Single => {}
            Quote::Ansi if c == '\'' => quote = Quote::None,
            Quote::Ansi => {}
            Quote::Double | Quote::None if c == '\\' => {
                current.push(c);
                current.extend(escaped);
            }
            Quote::Double => {
                current.push(c);
                if c == '"' {
                    quote = Quote::None;
                }
            }
            Quote::None => match c {
                '\'' => {
                    quote = if current.ends_with('$') {
                        current.pop();
                        Quote::Ansi
                    } else {
                        Quote::Single
                    };
                    current.push_str("''");
                }
                '"' => {
                    current.push(c);
                    quote = Quote::Double;
                }
                '#' if current
                    .chars()
                    .last()
                    .is_none_or(|p| p.is_whitespace() || ";&|()".contains(p)) =>
                {
                    comment = true;
                }
                _ => current.push(c),
            },
        }
    }
    lines.push(current);
    lines
}

/// Each match of `patterns` in `lines`.
fn findings(lines: &[String], patterns: &[(Regex, &'static str)]) -> Vec<Finding> {
    let mut found = Vec::new();
    for (index, text) in lines.iter().enumerate() {
        for (re, construct) in patterns {
            if re.is_match(text) {
                found.push(Finding {
                    line: index + 1,
                    construct,
                });
            }
        }
    }
    found
}

/// Every construct of `script`'s code that bash 3.2 does not have.
pub fn newer_than_bash_3_2(script: &str) -> Vec<Finding> {
    let code = code(script);
    let mut found = findings(&code, &NEWER_RE);
    for (in_code, in_line, construct) in NEWER_QUOTED_RE.iter() {
        for (index, (text, raw)) in code.iter().zip(script.split('\n')).enumerate() {
            if in_code.is_match(text) && in_line.is_match(raw) {
                found.push(Finding {
                    line: index + 1,
                    construct,
                });
            }
        }
    }
    found.sort_by_key(|f| f.line);
    found
}

/// Every export, here-document and here-string of `script`'s code.
pub fn secret_hazards(script: &str) -> Vec<Finding> {
    findings(&code(script), &HAZARDS_RE)
}

#[cfg(test)]
mod tests;
