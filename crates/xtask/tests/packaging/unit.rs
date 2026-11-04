//! A systemd unit file read as systemd reads it (systemd.syntax(7)): sections, `Key=Value`
//! assignments, comments and continued lines; and a command line split into its words
//! (systemd.service(5), "Command lines").

/// One assignment, with its section and its 1-based line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub section: String,
    pub key: String,
    pub value: String,
    pub line: usize,
}

/// The assignments of a unit file, in file order.
#[derive(Debug)]
pub struct Unit {
    pub entries: Vec<Entry>,
}

impl Unit {
    /// Reads `text`. A line is blank, a comment (`#` or `;` first), a section header
    /// (`[Name]`), or `Key=Value` inside a section; a line ending in `\` goes on on the next
    /// line, joined with a space. Anything else is refused with its line.
    pub fn parse(text: &str) -> Result<Unit, String> {
        let mut entries = Vec::new();
        let mut section: Option<String> = None;
        let mut lines = text.lines().enumerate();
        while let Some((at, raw)) = lines.next() {
            let n = at + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(header) = line.strip_prefix('[') {
                let name = header
                    .strip_suffix(']')
                    .ok_or_else(|| format!("line {n}: a section header without `]`"))?;
                section = Some(name.to_owned());
                continue;
            }
            let mut logical = line.to_owned();
            while let Some(head) = logical.strip_suffix('\\') {
                let (_, next) = lines
                    .next()
                    .ok_or_else(|| format!("line {n}: a `\\` continues past the end"))?;
                logical = format!("{head} {}", next.trim());
            }
            let section = section
                .clone()
                .ok_or_else(|| format!("line {n}: an assignment before any section"))?;
            let (key, value) = logical
                .split_once('=')
                .ok_or_else(|| format!("line {n}: not `Key=Value`: {logical:?}"))?;
            entries.push(Entry {
                section,
                key: key.trim().to_owned(),
                value: value.trim().to_owned(),
                line: n,
            });
        }
        Ok(Unit { entries })
    }

    /// Every value of `key` in `section`, in order.
    pub fn values(&self, section: &str, key: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.section == section && e.key == key)
            .map(|e| e.value.as_str())
            .collect()
    }

    /// The one value of `key` in `section`: an error when it is missing or set more than once.
    pub fn single(&self, section: &str, key: &str) -> Result<&str, String> {
        match self.values(section, key).as_slice() {
            [one] => Ok(one),
            [] => Err(format!("[{section}] has no {key}=")),
            many => Err(format!("[{section}] sets {key}= {} times", many.len())),
        }
    }

    /// The words of a list-valued key (`After=`, `Conflicts=`) as systemd reads them: each
    /// assignment adds its words, and an empty assignment clears what came before.
    pub fn list(&self, section: &str, key: &str) -> Vec<String> {
        let mut words = Vec::new();
        for value in self.values(section, key) {
            if value.is_empty() {
                words.clear();
            }
            words.extend(value.split_whitespace().map(str::to_owned));
        }
        words
    }
}

/// The characters systemd expands in a command line: `$` (an environment variable), `%` (a
/// specifier) and `\` (an escape). The unit uses none of them.
fn plain(c: char) -> Result<char, String> {
    if matches!(c, '$' | '%' | '\\') {
        return Err(format!(
            "the command line holds `{c}`, which systemd expands; write the value itself"
        ));
    }
    Ok(c)
}

/// Splits a command line into words as systemd does: words are separated by white space, and a
/// word in double or single quotes is kept whole. Refused: a character systemd expands (see
/// [`plain`]), an unclosed quote, a `;` word (which would start a second command), and a prefix
/// on the program (`@`, `-`, `:`, `+`, `!`), each of which changes how it runs.
pub fn command_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            in_word = true;
            loop {
                match chars.next() {
                    None => return Err(format!("the command line has an unclosed {c}")),
                    Some(q) if q == c => break,
                    Some(q) => word.push(plain(q)?),
                }
            }
        } else if c.is_whitespace() {
            if in_word {
                words.push(std::mem::take(&mut word));
                in_word = false;
            }
        } else {
            word.push(plain(c)?);
            in_word = true;
        }
    }
    if in_word {
        words.push(word);
    }
    if words.iter().any(|w| w == ";") {
        return Err("the command line holds a `;`, which starts a second command".to_owned());
    }
    match words.first() {
        None => Err("the command line is empty".to_owned()),
        Some(program) if program.starts_with(['@', '-', ':', '+', '!']) => Err(format!(
            "the program {program:?} has a prefix, which changes how systemd runs it"
        )),
        Some(_) => Ok(words),
    }
}
