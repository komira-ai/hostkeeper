//! The value of a Rust `const` or `static`, for the cells that quote one.
//!
//! The value expression is evaluated, not searched for literals, so a cell is held to what the
//! constant is: `1 << 20` is 1,048,576 (and `20 MiB` is not it), `7 * 24 * 60 * 60` is a week in
//! seconds, `0x1000` is 4096. The grammar is what the constants of this repository are written
//! in: integer and float literals (decimal with `_` separators, an exponent and a type suffix;
//! `0x`, `0o` and `0b`), `+`, `-`, `*`, `/`, `<<`, parentheses, and a call of a path with one
//! argument. A call is its argument, except `Duration::from_millis` (seconds are the unit of a
//! `Duration`, so it is its argument over 1000) and `from_micros` and `from_nanos` likewise.
//! Anything else (a string, a method call, a second argument, a name) cannot be evaluated, and
//! the cell that quotes it is refused.

/// One token of a value expression.
#[derive(Debug, PartialEq)]
enum Tok {
    Num(f64),
    Path(String),
    Open,
    Close,
    Op(&'static str),
}

/// The tokens of `expr`, or what cannot be read.
fn lex(expr: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = expr.chars().collect();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let radix = match (c, chars.get(i + 1)) {
                ('0', Some('x')) => 16,
                ('0', Some('o')) => 8,
                ('0', Some('b')) => 2,
                _ => 10,
            };
            let mut digits = String::new();
            if radix != 10 {
                i += 2;
                while i < chars.len() && (chars[i].is_digit(radix) || chars[i] == '_') {
                    digits.extend(chars[i].is_digit(radix).then_some(chars[i]));
                    i += 1;
                }
                let value = u32::from_str_radix(&digits, radix)
                    .map_err(|_| format!("{digits:?} is no 32-bit literal in {expr:?}"))?;
                out.push(Tok::Num(f64::from(value)));
            } else {
                while i < chars.len() {
                    let d = chars[i];
                    let fraction = d == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit);
                    let exponent = (d == 'e' || d == 'E')
                        && (chars.get(i + 1).is_some_and(char::is_ascii_digit)
                            || (matches!(chars.get(i + 1), Some('+' | '-'))
                                && chars.get(i + 2).is_some_and(char::is_ascii_digit)));
                    if d.is_ascii_digit() || fraction {
                        digits.push(d);
                    } else if exponent {
                        digits.push('e');
                        if matches!(chars[i + 1], '+' | '-') {
                            i += 1;
                            digits.push(chars[i]);
                        }
                    } else if d != '_' {
                        break;
                    }
                    i += 1;
                }
                let value: f64 = digits
                    .parse()
                    .map_err(|_| format!("{digits:?} is not a number"))?;
                out.push(Tok::Num(value));
            }
            while i < chars.len() && word(chars[i]) {
                i += 1;
            }
        } else if word(c) {
            let mut path = String::new();
            while i < chars.len() && (word(chars[i]) || chars[i] == ':') {
                path.push(chars[i]);
                i += 1;
            }
            out.push(Tok::Path(path));
        } else {
            let (tok, len) = match (c, chars.get(i + 1)) {
                ('<', Some('<')) => (Tok::Op("<<"), 2),
                ('(', _) => (Tok::Open, 1),
                (')', _) => (Tok::Close, 1),
                ('*', _) => (Tok::Op("*"), 1),
                ('/', _) => (Tok::Op("/"), 1),
                ('+', _) => (Tok::Op("+"), 1),
                ('-', _) => (Tok::Op("-"), 1),
                _ => return Err(format!("{c:?} in {expr:?} cannot be evaluated")),
            };
            out.push(tok);
            i += len;
        }
    }
    Ok(out)
}

/// A recursive-descent evaluator over the tokens: `shift := sum ("<<" sum)*`,
/// `sum := product (("+" | "-") product)*`, `product := atom (("*" | "/") atom)*`,
/// `atom := number | "-" atom | "(" shift ")" | path "(" shift ")"`.
struct Eval<'a> {
    toks: &'a [Tok],
    at: usize,
}

impl Eval<'_> {
    fn peek_op(&self, ops: &[&str]) -> Option<&'static str> {
        match self.toks.get(self.at) {
            Some(Tok::Op(op)) if ops.contains(op) => Some(op),
            _ => None,
        }
    }

    fn shift(&mut self) -> Result<f64, String> {
        let mut value = self.sum()?;
        while self.peek_op(&["<<"]).is_some() {
            self.at += 1;
            value *= 2f64.powf(self.sum()?);
        }
        Ok(value)
    }

    fn sum(&mut self) -> Result<f64, String> {
        let mut value = self.product()?;
        while let Some(op) = self.peek_op(&["+", "-"]) {
            self.at += 1;
            let rhs = self.product()?;
            value = if op == "+" { value + rhs } else { value - rhs };
        }
        Ok(value)
    }

    fn product(&mut self) -> Result<f64, String> {
        let mut value = self.atom()?;
        while let Some(op) = self.peek_op(&["*", "/"]) {
            self.at += 1;
            let rhs = self.atom()?;
            value = if op == "*" { value * rhs } else { value / rhs };
        }
        Ok(value)
    }

    fn expect_close(&mut self) -> Result<(), String> {
        if self.toks.get(self.at) == Some(&Tok::Close) {
            self.at += 1;
            Ok(())
        } else {
            Err("a call or a parenthesis is not closed".to_owned())
        }
    }

    fn atom(&mut self) -> Result<f64, String> {
        let tok = self.toks.get(self.at);
        self.at += 1;
        match tok {
            Some(Tok::Num(n)) => Ok(*n),
            Some(Tok::Op("-")) => Ok(-self.atom()?),
            Some(Tok::Open) => {
                let value = self.shift()?;
                self.expect_close()?;
                Ok(value)
            }
            Some(Tok::Path(path)) if self.toks.get(self.at) == Some(&Tok::Open) => {
                self.at += 1;
                let arg = self.shift()?;
                self.expect_close()?;
                let last = path.rsplit("::").next().unwrap_or(path);
                Ok(match last {
                    "from_millis" => arg / 1e3,
                    "from_micros" => arg / 1e6,
                    "from_nanos" => arg / 1e9,
                    _ => arg,
                })
            }
            Some(Tok::Path(path)) => Err(format!("the name {path} cannot be evaluated")),
            other => Err(format!("{other:?} cannot start a value")),
        }
    }
}

/// The value of a Rust expression in the grammar of the module.
pub fn evaluate(expr: &str) -> Result<f64, String> {
    let toks = lex(expr)?;
    let mut eval = Eval { toks: &toks, at: 0 };
    let value = eval.shift()?;
    if eval.at == toks.len() {
        Ok(value)
    } else {
        Err(format!("{expr:?} has more after its value"))
    }
}

/// The value text of the `const` or `static` named `item` in `source`: the declaration is a line
/// that starts, after white space and an optional `pub` or `pub(...)`, with `const <item>:`,
/// `static <item>:` or `static mut <item>:` (so one in a comment is not read), and its value is
/// the text after its `=` up to the first `;` outside brackets and strings. `None` when no such
/// declaration exists.
pub fn declared_text<'a>(source: &'a str, item: &str) -> Option<&'a str> {
    let mut offset = 0;
    let mut at = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let after_vis = match trimmed.strip_prefix("pub") {
            Some(rest) if rest.starts_with('(') => rest.find(')').map_or(rest, |c| &rest[c + 1..]),
            Some(rest) if rest.starts_with(' ') => rest,
            _ => trimmed,
        }
        .trim_start();
        let declares = ["const ", "static mut ", "static "].iter().any(|kw| {
            after_vis
                .strip_prefix(kw)
                .and_then(|rest| rest.strip_prefix(item))
                .is_some_and(|rest| rest.trim_start().starts_with(':'))
        });
        if declares {
            at = Some(offset);
            break;
        }
        offset += line.len();
    }
    let rest = &source[at?..];
    let value = &rest[rest.find('=').map_or(rest.len(), |eq| eq + 1)..];
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let end = value
        .char_indices()
        .find(|&(_, c)| {
            if in_string {
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => in_string = false,
                    _ => {}
                }
                return false;
            }
            match c {
                '"' => in_string = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
            c == ';' && depth == 0
        })
        .map_or(value.len(), |(i, _)| i);
    Some(value[..end].trim())
}
