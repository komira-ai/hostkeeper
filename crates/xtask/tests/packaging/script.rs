//! The package scripts, read in the small part of POSIX sh they are written in, with no shell.
//!
//! A script starts with `#!/bin/sh`. Every other line is blank, a `#` comment, `set -e`, one
//! or more simple commands joined by `||` or `&&`, or a `case` on `"$1"` or `"$1:$2"` whose arms
//! are each one line, `pattern | pattern) commands ;;`, closed by `esac`. A word of a command is
//! letters, digits and `._/:=+-` only, so there is no variable, substitution, glob, quote,
//! redirection, pipe, background job, subshell, test (`[`, `[[`) or function in a command.
//! Anything else is refused with its line.

/// A simple command, with the patterns of the `case` arm it runs under, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub words: Vec<String>,
    pub arm: Option<Vec<String>>,
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
fn commands(text: &str, n: usize, arm: Option<&[String]>) -> Result<Vec<Command>, String> {
    let mut out = Vec::new();
    for part in text.split("||").flat_map(|part| part.split("&&")) {
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
            arm: arm.map(<[String]>::to_vec),
            line: n,
        });
    }
    Ok(out)
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
    let mut in_case = false;
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
            if in_case {
                return Err(format!("line {n}: a `case` inside a `case`"));
            }
            if !SUBJECTS.contains(&subject.trim()) {
                return Err(format!(
                    "line {n}: a `case` on {subject:?}; only \"$1\" and \"$1:$2\" are tested"
                ));
            }
            in_case = true;
        } else if line == "esac" {
            if !in_case {
                return Err(format!("line {n}: an `esac` with no `case`"));
            }
            in_case = false;
        } else if in_case {
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
            out.extend(commands(body, n, Some(&patterns))?);
        } else {
            out.extend(commands(line, n, None)?);
        }
    }
    if in_case {
        return Err("a `case` is not closed by `esac`".to_owned());
    }
    Ok(out)
}
