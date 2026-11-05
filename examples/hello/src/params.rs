//! The `--params <file>` table, read without a TOML library.
//!
//! The daemon writes the file with `Params::to_toml` of the hook contract crate: a flat table of
//! scalars, reserved keys quoted (`"updater.target_version" = "1.2.3"`). This reader takes what
//! that writer can write: bare and quoted keys, basic and literal strings (single-line and
//! multi-line, with the escapes of TOML), integers, booleans and floats (read loosely, kept as
//! [`Value::Other`]), `#` comments and blank lines. Tables, arrays, dotted keys, dates and a key
//! given twice are refused, and so is any other text.

use std::collections::BTreeMap;

/// One value of the table.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Integer(i64),
    Bool(bool),
    /// A float: the hook uses none and keeps none.
    Other,
}

impl Value {
    /// The text, when the value is text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// A flat table of scalars.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table(BTreeMap<String, Value>);

impl Table {
    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }
}

/// Reads the text of a params file. The error says on which line and why.
pub fn parse(text: &str) -> Result<Table, String> {
    let mut parser = Parser {
        chars: text.chars().collect(),
        pos: 0,
    };
    let mut table = BTreeMap::new();
    loop {
        parser.skip_blank();
        if parser.peek().is_none() {
            return Ok(Table(table));
        }
        let line = parser.line();
        let (key, value) = parser
            .entry()
            .map_err(|why| format!("line {line}: {why}"))?;
        if table.contains_key(&key) {
            return Err(format!("line {line}: key {key:?} is given twice"));
        }
        table.insert(key, value);
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.pos += usize::from(c.is_some());
        c
    }

    /// Takes `c` if it is next.
    fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        self.pos += usize::from(found);
        found
    }

    /// The line (from 1) the parser is on.
    fn line(&self) -> usize {
        1 + self.chars[..self.pos]
            .iter()
            .filter(|c| **c == '\n')
            .count()
    }

    /// Spaces and tabs.
    fn skip_space(&mut self) {
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.pos += 1;
        }
    }

    /// Whitespace, line ends and comments.
    fn skip_blank(&mut self) {
        while let Some(c) = self.peek() {
            if c == '#' {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
            } else if c.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// `key = value`, then the end of the line (a comment may follow).
    fn entry(&mut self) -> Result<(String, Value), String> {
        let key = self.key()?;
        self.skip_space();
        if !self.eat('=') {
            return Err(format!("expected `=` after the key {key:?}"));
        }
        self.skip_space();
        let value = self.value()?;
        self.skip_space();
        match self.peek() {
            None | Some('#' | '\n' | '\r') => Ok((key, value)),
            Some(_) => Err("unexpected text after the value".to_owned()),
        }
    }

    fn key(&mut self) -> Result<String, String> {
        match self.peek() {
            Some(quote @ ('"' | '\'')) => {
                self.pos += 1;
                self.string(quote)
            }
            Some(c) if is_bare(c) => {
                let start = self.pos;
                while self.peek().is_some_and(is_bare) {
                    self.pos += 1;
                }
                Ok(self.chars[start..self.pos].iter().collect())
            }
            _ => Err("expected a key".to_owned()),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(quote @ ('"' | '\'')) => {
                self.pos += 1;
                self.string(quote).map(Value::Text)
            }
            _ => self.word(),
        }
    }

    /// A string whose opening quote is taken: single-line, or multi-line if two more quotes
    /// follow. A basic string (`"`) has escapes, a literal one (`'`) has none.
    fn string(&mut self, quote: char) -> Result<String, String> {
        let multiline = self.chars.get(self.pos) == Some(&quote)
            && self.chars.get(self.pos + 1) == Some(&quote);
        if !multiline {
            return self.single_line(quote);
        }
        self.pos += 2;
        // A newline right after the opening quotes is not part of the string.
        self.eat('\r');
        self.eat('\n');
        self.multi_line(quote)
    }

    fn single_line(&mut self, quote: char) -> Result<String, String> {
        let mut text = String::new();
        loop {
            match self.next() {
                None | Some('\n') => return Err("a string is not closed on its line".to_owned()),
                Some(c) if c == quote => return Ok(text),
                Some('\\') if quote == '"' => text.push(self.escape()?),
                Some(c) => text.push(c),
            }
        }
    }

    fn multi_line(&mut self, quote: char) -> Result<String, String> {
        let mut text = String::new();
        loop {
            match self.next() {
                None => return Err("a multi-line string is not closed".to_owned()),
                Some(c) if c == quote => {
                    // Three quotes close it; one or two more before them belong to the text.
                    let mut run = 1;
                    while self.eat(quote) {
                        run += 1;
                    }
                    if run >= 3 {
                        if run > 5 {
                            return Err("too many quotes at the end of a string".to_owned());
                        }
                        text.extend(std::iter::repeat_n(quote, run - 3));
                        return Ok(text);
                    }
                    text.extend(std::iter::repeat_n(quote, run));
                }
                Some('\\') if quote == '"' => {
                    // A backslash at the end of a line takes the line break and the blanks after.
                    let mut ahead = self.pos;
                    while self
                        .chars
                        .get(ahead)
                        .is_some_and(|c| *c == ' ' || *c == '\t')
                    {
                        ahead += 1;
                    }
                    if matches!(self.chars.get(ahead), Some('\n' | '\r')) {
                        self.pos = ahead;
                        self.skip_blank_lines_in_string();
                    } else {
                        text.push(self.escape()?);
                    }
                }
                Some(c) => text.push(c),
            }
        }
    }

    fn skip_blank_lines_in_string(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    /// The character after a backslash.
    fn escape(&mut self) -> Result<char, String> {
        let code = match self.next() {
            Some('b') => '\u{8}',
            Some('t') => '\t',
            Some('n') => '\n',
            Some('f') => '\u{c}',
            Some('r') => '\r',
            Some('e') => '\u{1b}',
            Some('"') => '"',
            Some('\\') => '\\',
            Some('x') => return self.hex(2),
            Some('u') => return self.hex(4),
            Some('U') => return self.hex(8),
            _ => return Err("a string has an escape that TOML does not have".to_owned()),
        };
        Ok(code)
    }

    /// `digits` hexadecimal digits as one character.
    fn hex(&mut self, digits: usize) -> Result<char, String> {
        let mut code: u32 = 0;
        for _ in 0..digits {
            let digit = self
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or("a unicode escape has too few hexadecimal digits")?;
            code = code.saturating_mul(16).saturating_add(digit);
        }
        char::from_u32(code).ok_or_else(|| format!("U+{code:04X} is not a character"))
    }

    /// An unquoted value: `true`, `false`, an integer or a float.
    fn word(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| !c.is_whitespace() && c != '#') {
            self.pos += 1;
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        let number = word.replace('_', "");
        if word == "true" || word == "false" {
            Ok(Value::Bool(word == "true"))
        } else if let Ok(n) = number.parse::<i64>() {
            Ok(Value::Integer(n))
        } else if number.parse::<f64>().is_ok() {
            Ok(Value::Other)
        } else {
            Err(format!("{word:?} is not a value"))
        }
    }
}

/// A character of a bare key.
fn is_bare(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

#[cfg(test)]
mod tests;
