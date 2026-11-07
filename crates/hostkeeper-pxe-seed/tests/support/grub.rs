//! A reader of GRUB's script language, for the tests: enough of it to read the files this crate
//! generates as GRUB reads them, and to refuse anything it does not model rather than read it
//! some other way.
//!
//! It follows the lexer of GRUB 2's scripts (`grub-core/script/yylex.l` and the GRUB manual,
//! "Shell-like scripting"):
//!
//! - Commands end at a newline or an unquoted `;`.
//! - Words are split by unquoted blanks (space and tab).
//! - Outside quotes, `\` takes the next character literally, and a `\` before a newline joins the
//!   two lines.
//! - Inside `'...'` every character is literal.
//! - Inside `"..."`, `\` escapes only `$`, `"`, `\` and a newline; any other `\` stays as written.
//! - `$name` and `${name}` are variable references, outside single quotes. They are kept as
//!   references, not expanded.
//! - `{` and `}` standing as words open and close a block. A `menuentry` command takes the block
//!   that follows its arguments.
//! - `#` at the start of a word begins a comment, which runs to the end of the line.
//!
//! The metacharacters `|`, `&`, `<` and `>`, and the words `if`, `for`, `while`, `function` and
//! the like, are refused: these files never use them, so a file that did would not be read as
//! GRUB reads it.

/// One piece of a word: literal text, or a variable reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Text(String),
    Var(String),
}

/// One word: its pieces, adjacent pieces of text joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word(pub Vec<Part>);

impl Word {
    /// The word's text when it holds no variable reference.
    pub fn text(&self) -> Option<&str> {
        match self.0.as_slice() {
            [] => Some(""),
            [Part::Text(text)] => Some(text),
            _ => None,
        }
    }

    /// The word written with each variable as `${name}`, for asserting a word that holds one.
    pub fn spelled(&self) -> String {
        self.0
            .iter()
            .map(|part| match part {
                Part::Text(text) => text.clone(),
                Part::Var(name) => format!("${{{name}}}"),
            })
            .collect()
    }
}

/// One command: its words, and for a `menuentry`, the block of commands it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub words: Vec<Word>,
    pub block: Option<Vec<Command>>,
}

impl Command {
    /// The command's words as text; panics on a word that holds a variable.
    pub fn texts(&self) -> Vec<&str> {
        self.words
            .iter()
            .map(|word| word.text().expect("a word with no variable"))
            .collect()
    }
}

/// A token of the lexer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(Word),
    /// A newline or an unquoted `;`.
    End,
}

const RESERVED: [&str; 12] = [
    "if", "then", "elif", "else", "fi", "for", "in", "do", "done", "while", "until", "function",
];

fn push_text(parts: &mut Vec<Part>, c: char) {
    match parts.last_mut() {
        Some(Part::Text(text)) => text.push(c),
        _ => parts.push(Part::Text(c.to_string())),
    }
}

/// Reads a variable reference after its `$`: `${name}` or `$name` (letters, digits and `_`).
fn variable(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String, String> {
    if chars.peek() == Some(&'{') {
        chars.next();
        let mut name = String::new();
        loop {
            match chars.next() {
                Some('}') => break,
                Some(c) if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
                Some(c) => return Err(format!("{c:?} in a ${{...}} variable name")),
                None => return Err("an unterminated ${...}".to_owned()),
            }
        }
        if name.is_empty() {
            return Err("an empty ${}".to_owned());
        }
        return Ok(name);
    }
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        name.push(c);
        chars.next();
    }
    if name.is_empty() {
        return Err("a $ with no variable name".to_owned());
    }
    Ok(name)
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    let mut word: Option<Vec<Part>> = None;
    let finish = |word: &mut Option<Vec<Part>>, out: &mut Vec<Token>| {
        if let Some(parts) = word.take() {
            out.push(Token::Word(Word(parts)));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => finish(&mut word, &mut out),
            '\n' | ';' => {
                finish(&mut word, &mut out);
                out.push(Token::End);
            }
            '#' if word.is_none() => {
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            }
            '|' | '&' | '<' | '>' => return Err(format!("the metacharacter {c:?} is not read")),
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(next) => push_text(word.get_or_insert_with(Vec::new), next),
                None => return Err("a \\ at the end of the text".to_owned()),
            },
            '\'' => {
                let parts = word.get_or_insert_with(Vec::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => push_text(parts, c),
                        None => return Err("an unterminated '...'".to_owned()),
                    }
                }
                if parts.is_empty() {
                    parts.push(Part::Text(String::new()));
                }
            }
            '"' => {
                let parts = word.get_or_insert_with(Vec::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('\n') => {}
                            Some(next @ ('$' | '"' | '\\')) => push_text(parts, next),
                            Some(next) => {
                                push_text(parts, '\\');
                                push_text(parts, next);
                            }
                            None => return Err("an unterminated \"...\"".to_owned()),
                        },
                        Some('$') => parts.push(Part::Var(variable(&mut chars)?)),
                        Some(c) => push_text(parts, c),
                        None => return Err("an unterminated \"...\"".to_owned()),
                    }
                }
                if parts.is_empty() {
                    parts.push(Part::Text(String::new()));
                }
            }
            '$' => {
                let name = variable(&mut chars)?;
                word.get_or_insert_with(Vec::new).push(Part::Var(name));
            }
            _ => push_text(word.get_or_insert_with(Vec::new), c),
        }
    }
    finish(&mut word, &mut out);
    Ok(out)
}

fn is_bare(word: &Word, text: &str) -> bool {
    word.text() == Some(text)
}

/// Reads commands until the end of the text (`nested` false) or a `}` word (`nested` true).
fn commands(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>,
    nested: bool,
) -> Result<Vec<Command>, String> {
    let mut out = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    loop {
        match tokens.next() {
            None if nested => return Err("a { block with no }".to_owned()),
            None => {
                if !words.is_empty() {
                    out.push(Command { words, block: None });
                }
                return Ok(out);
            }
            Some(Token::End) => {
                if !words.is_empty() {
                    out.push(Command {
                        words: std::mem::take(&mut words),
                        block: None,
                    });
                }
            }
            Some(Token::Word(word)) if words.is_empty() && is_bare(&word, "}") => {
                return if nested {
                    Ok(out)
                } else {
                    Err("a } with no {".to_owned())
                };
            }
            Some(Token::Word(word)) if is_bare(&word, "}") => {
                return Err("a } in the middle of a command".to_owned());
            }
            Some(Token::Word(word)) if is_bare(&word, "{") => {
                let is_menuentry = words
                    .first()
                    .is_some_and(|first| is_bare(first, "menuentry"));
                if !is_menuentry {
                    return Err("a { block that does not follow menuentry".to_owned());
                }
                let block = commands(tokens, true)?;
                out.push(Command {
                    words: std::mem::take(&mut words),
                    block: Some(block),
                });
            }
            Some(Token::Word(word)) => {
                if words.is_empty() && RESERVED.iter().any(|r| is_bare(&word, r)) {
                    return Err(format!(
                        "the reserved word {:?} is not read",
                        word.spelled()
                    ));
                }
                words.push(word);
            }
        }
    }
}

/// The commands of a GRUB script, as GRUB reads them; an error for what the reader does not
/// model or GRUB would refuse.
pub fn read(text: &str) -> Result<Vec<Command>, String> {
    let mut tokens = tokens(text)?.into_iter().peekable();
    commands(&mut tokens, false)
}
