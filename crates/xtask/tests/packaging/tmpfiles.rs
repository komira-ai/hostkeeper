//! A tmpfiles.d(5) file: one line per path, `Type Path Mode User Group Age [Argument]`.

/// One line, its fields as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: String,
    pub path: String,
    pub mode: String,
    pub user: String,
    pub group: String,
    pub age: String,
    /// The seventh field, if any (`-` and absent are both no argument).
    pub argument: Option<String>,
    /// 1-based.
    pub line: usize,
}

/// Reads `text`: blank lines and `#` comments are skipped; every other line has six or seven
/// fields separated by white space, or it is refused with its line. (tmpfiles.d also allows
/// fewer fields, with defaults; this file writes every one.)
pub fn parse(text: &str) -> Result<Vec<Line>, String> {
    let mut out = Vec::new();
    for (at, raw) in text.lines().enumerate() {
        let n = at + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [kind, path, mode, user, group, age, rest @ ..] = fields.as_slice() else {
            return Err(format!(
                "line {n}: {} fields; a line here has Type, Path, Mode, User, Group and Age",
                fields.len()
            ));
        };
        let argument = match rest {
            [] | ["-"] => None,
            [one] => Some((*one).to_owned()),
            _ => return Err(format!("line {n}: more than six fields")),
        };
        out.push(Line {
            kind: (*kind).to_owned(),
            path: (*path).to_owned(),
            mode: (*mode).to_owned(),
            user: (*user).to_owned(),
            group: (*group).to_owned(),
            age: (*age).to_owned(),
            argument,
            line: n,
        });
    }
    Ok(out)
}
