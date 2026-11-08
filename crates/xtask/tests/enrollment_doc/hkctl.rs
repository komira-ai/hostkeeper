//! `hkctl`'s command line, read as the script reads it: the global flags of `docs/cli.md`'s table
//! (written by `hostkeeper_hkctl::markdown`), then a service and a method of the command table
//! hkctl serves (`hostkeeper_hkctl::is_served`), then that method's flags.
//!
//! The rules are the parser of the script's runtime (`crates/hostkeeper-hkctl/src/runtime.bash`,
//! `_hk_main`, `_hk_run`, `_hk_global` and `_hk_field_flag`), each word in the order the script
//! takes it: `--help` or `-h` anywhere, `--version` only before the service; a flag's value as
//! `--flag value` or `--flag=value`, a value that starts with `--` only in the second form; a
//! switch with no value; each global flag and each singular field at most once; at most one input
//! that reads stdin; a bool `true` or `false`, an enum one of its values, an integer in its kind's
//! range, a float a JSON number; a map `key=value` with each key once; a secret only as
//! `--<flag>-file` or `--<flag>-stdin`. What the script checks only once it runs (a file it reads,
//! the endpoint's host, a request file's keys) is not read here, but a call needs what it needs
//! of every line: `--endpoint` with an `https://` value, `--ca-file`, and exactly one of the
//! token's three inputs.

use hostkeeper_proto::ctl::{Command, Field, FieldKind, Input, Label};

/// What a command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// A call of the method, as `service method`.
    Call(String),
    /// A help text: `--help` or `-h`.
    Help,
    /// `--version`, before the service.
    Version,
}

/// A global flag: its name without `--`, and whether it takes a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Global {
    pub(crate) name: String,
    pub(crate) value: bool,
}

/// The heading of the global flags' table in `docs/cli.md`.
const GLOBALS_HEADING: &str = "## Global flags";

/// The flags of [`GLOBALS_HEADING`]'s table in `cli`, the text `hostkeeper_hkctl::markdown`
/// writes, but `--help` and `--version`, which the script takes before any other flag.
pub(crate) fn globals_of(cli: &str) -> Result<Vec<Global>, String> {
    let mut lines = cli.lines().skip_while(|l| l.trim_end() != GLOBALS_HEADING);
    if lines.next().is_none() {
        return Err(format!("no section `{GLOBALS_HEADING}`"));
    }
    let globals: Vec<Global> = lines
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| l.strip_prefix("| `--"))
        .filter_map(|l| {
            let (name, rest) = l.split_once('`')?;
            let value = rest.trim_start_matches(" |").split('|').next()?.trim();
            Some(Global {
                name: name.to_owned(),
                value: !value.is_empty(),
            })
        })
        .filter(|g| g.name != "help" && g.name != "version")
        .collect();
    if globals.is_empty() {
        return Err(format!("the section `{GLOBALS_HEADING}` has no flags"));
    }
    Ok(globals)
}

/// The commands of `commands` hkctl has.
pub(crate) fn served(commands: &'static [Command]) -> Vec<&'static Command> {
    commands
        .iter()
        .filter(|c| hostkeeper_hkctl::is_served(c))
        .collect()
}

/// The state of one command line as the words are taken.
struct Line<'a> {
    globals: &'a [Global],
    /// The global flags given, by name.
    given: Vec<String>,
    /// The flags of request fields given once, by field name, and the keys of each map.
    fields: Vec<String>,
    map_keys: Vec<(String, String)>,
    /// The input that reads stdin, if any.
    stdin_by: Option<String>,
    /// The value of `--endpoint`.
    endpoint: String,
}

impl Line<'_> {
    /// Marks `flag` as reading stdin; a second input that reads it is a usage error.
    fn stdin(&mut self, flag: &str) -> Result<(), String> {
        if let Some(by) = &self.stdin_by {
            return Err(format!(
                "{by} and {flag} both read stdin; at most one input may"
            ));
        }
        self.stdin_by = Some(flag.to_owned());
        Ok(())
    }

    /// Takes the global flag at the start of `words`: how many words it took, or `None` when
    /// they do not start with one.
    fn global(&mut self, words: &[String]) -> Result<Option<usize>, String> {
        let flag = flag_of(&words[0]);
        let Some(global) = self
            .globals
            .iter()
            .find(|g| flag.strip_prefix("--") == Some(g.name.as_str()))
        else {
            return Ok(None);
        };
        let (value, took) = if global.value {
            arg(flag, words)?
        } else {
            switch(flag, &words[0])?;
            (String::new(), 1)
        };
        if self.given.contains(&global.name) {
            return Err(format!("{flag} is given twice"));
        }
        self.given.push(global.name.clone());
        match global.name.as_str() {
            "token-stdin" => self.stdin(flag)?,
            "endpoint" => self.endpoint = value,
            "timeout-secs" if !is_secs(&value) => {
                return Err(format!(
                    "{flag} takes a whole number of seconds, 1 to 999999"
                ));
            }
            _ => {}
        }
        Ok(Some(took))
    }

    /// Takes the flag of a request field of `command` at the start of `words`: how many words it
    /// took, or `None` when they do not start with one.
    fn field(&mut self, command: &Command, words: &[String]) -> Result<Option<usize>, String> {
        let flag = flag_of(&words[0]);
        for field in command.fields {
            match field.input {
                Input::Flag if flag == format!("--{}", field.flag) => {
                    let (value, took) = arg(flag, words)?;
                    self.put(field, &value)?;
                    return Ok(Some(took));
                }
                Input::SecretFile if flag == format!("--{}-file", field.flag) => {
                    let (_, took) = arg(flag, words)?;
                    self.secret(field)?;
                    return Ok(Some(took));
                }
                Input::SecretFile if flag == format!("--{}-stdin", field.flag) => {
                    switch(flag, &words[0])?;
                    self.secret(field)?;
                    self.stdin(flag)?;
                    return Ok(Some(1));
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// One of the two flags of a secret field, which may be given once in all.
    fn secret(&mut self, field: &Field) -> Result<(), String> {
        if self.fields.iter().any(|f| f == field.name) {
            return Err(format!(
                "--{0}-file and --{0}-stdin: give one, once",
                field.flag
            ));
        }
        self.fields.push(field.name.to_owned());
        Ok(())
    }

    /// One occurrence of the flag of `field` with `value`.
    fn put(&mut self, field: &Field, value: &str) -> Result<(), String> {
        match field.label {
            Label::Repeated => encode(field, value),
            Label::Map => {
                let Some((key, _)) = value.split_once('=') else {
                    return Err(format!("--{} takes key=value", field.flag));
                };
                let entry = (field.name.to_owned(), key.to_owned());
                if self.map_keys.contains(&entry) {
                    return Err(format!("--{} names the key \"{key}\" twice", field.flag));
                }
                self.map_keys.push(entry);
                Ok(())
            }
            _ => {
                if self.fields.iter().any(|f| f == field.name) {
                    return Err(format!("--{} is given twice", field.flag));
                }
                encode(field, value)?;
                self.fields.push(field.name.to_owned());
                Ok(())
            }
        }
    }

    /// What a call needs of the global flags, once every word is taken.
    fn call_needs(&self) -> Result<(), String> {
        let has = |name: &str| self.given.iter().any(|g| g == name);
        if !has("endpoint") {
            return Err("a call needs --endpoint".to_owned());
        }
        if !self.endpoint.starts_with("https://") {
            return Err("--endpoint takes https://<host>[:<port>]".to_owned());
        }
        if !has("ca-file") {
            return Err("a call needs --ca-file".to_owned());
        }
        let credentials = ["token-file", "token-stdin", "github-oidc-audience"]
            .into_iter()
            .filter(|c| has(c))
            .count();
        if credentials != 1 {
            return Err(
                "a call needs exactly one of --token-file, --token-stdin and \
                 --github-oidc-audience"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

/// The flag of a word: what comes before its first `=`.
fn flag_of(word: &str) -> &str {
    word.split_once('=').map_or(word, |(flag, _)| flag)
}

/// The value of the flag `flag`, the first of `words`: `--f=v`, or `--f v`; and how many words it
/// took.
fn arg(flag: &str, words: &[String]) -> Result<(String, usize), String> {
    if let Some((_, value)) = words[0].split_once('=') {
        return Ok((value.to_owned(), 1));
    }
    match words.get(1) {
        Some(value) if !value.starts_with("--") => Ok((value.clone(), 2)),
        _ => Err(format!("{flag} needs a value")),
    }
}

/// Refuses a value given to the switch `flag` as `--f=v`.
fn switch(flag: &str, word: &str) -> Result<(), String> {
    if word.contains('=') {
        return Err(format!("{flag} takes no value"));
    }
    Ok(())
}

/// Whether `value` is a whole number of seconds, 1 to 999999.
fn is_secs(value: &str) -> bool {
    !value.starts_with('0')
        && (1..=6).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `value` is a number as JSON writes it, the script's float pattern
/// `^-?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?$`, or `NaN`, `Infinity` or `-Infinity`.
fn is_float(value: &str) -> bool {
    if matches!(value, "NaN" | "Infinity" | "-Infinity") {
        return true;
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (unsigned, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (digits(whole) && (fraction.is_empty() || digits(fraction)))
                || (whole.is_empty() && digits(fraction))
        }
        None => digits(mantissa),
    };
    let exponent_ok = exponent.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    mantissa_ok && exponent_ok
}

/// Refuses `value` unless it is one of `field`'s kind.
fn encode(field: &Field, value: &str) -> Result<(), String> {
    let flag = field.flag;
    let (what, min, max): (&str, i128, i128) = match field.kind {
        FieldKind::String => return Ok(()),
        FieldKind::Bool => {
            return match value {
                "true" | "false" => Ok(()),
                _ => Err(format!("--{flag} takes true or false")),
            };
        }
        FieldKind::Enum => {
            return if field.enum_values.iter().any(|v| v.flag == value) {
                Ok(())
            } else {
                let names: Vec<&str> = field.enum_values.iter().map(|v| v.flag).collect();
                Err(format!("--{flag} takes one of: {}", names.join(", ")))
            };
        }
        FieldKind::Float | FieldKind::Double => {
            return if is_float(value) {
                Ok(())
            } else {
                Err(format!("--{flag} takes a number"))
            };
        }
        FieldKind::Int32 | FieldKind::Sint32 | FieldKind::Sfixed32 => {
            ("a 32-bit signed integer", i32::MIN.into(), i32::MAX.into())
        }
        FieldKind::Uint32 | FieldKind::Fixed32 => ("a 32-bit unsigned integer", 0, u32::MAX.into()),
        FieldKind::Int64 | FieldKind::Sint64 | FieldKind::Sfixed64 => {
            ("a 64-bit signed integer", i64::MIN.into(), i64::MAX.into())
        }
        FieldKind::Uint64 | FieldKind::Fixed64 => ("a 64-bit unsigned integer", 0, u64::MAX.into()),
        // The table makes these request-file fields only; a flag of one is the table's error.
        FieldKind::Bytes | FieldKind::Message => {
            return Err(format!("--{flag} is no flag hkctl writes"));
        }
    };
    let digits = value.strip_prefix('-').unwrap_or(value);
    let in_range = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && value
            .parse::<i128>()
            .is_ok_and(|n| (min..=max).contains(&n));
    if in_range {
        Ok(())
    } else {
        Err(format!("--{flag} takes {what}"))
    }
}

/// What `words` (the line's words after `hkctl`) ask for, or the usage error the script exits 2
/// with, as it words it.
pub(crate) fn parse(
    commands: &[&'static Command],
    globals: &[Global],
    words: &[String],
) -> Result<Parsed, String> {
    let mut line = Line {
        globals,
        given: Vec::new(),
        fields: Vec::new(),
        map_keys: Vec::new(),
        stdin_by: None,
        endpoint: String::new(),
    };
    let mut at = 0;
    // Before the service.
    while let Some(word) = words.get(at) {
        match word.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" => return Ok(Parsed::Version),
            w if w.starts_with('-') => match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("unknown flag {}", flag_of(w))),
            },
            _ => break,
        }
    }
    let Some(service) = words.get(at) else {
        return Err("no service: hkctl <service> <method> [flags]".to_owned());
    };
    if !commands.iter().any(|c| c.service_command == service) {
        return Err(format!("no service {service}; 'hkctl --help' lists them"));
    }
    at += 1;
    // Between the service and the method.
    while let Some(word) = words.get(at) {
        match word.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            w if w.starts_with('-') => match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("unknown flag {}", flag_of(w))),
            },
            _ => break,
        }
    }
    let Some(method) = words.get(at) else {
        return Err(format!("no method: hkctl {service} <method> [flags]"));
    };
    let Some(command) = commands
        .iter()
        .find(|c| c.service_command == service && c.method_command == method)
    else {
        return Err(format!(
            "the service {service} has no method {method}; 'hkctl {service} --help' lists them"
        ));
    };
    at += 1;
    let name = format!("{service} {method}");
    let mut request = false;
    // The method's flags.
    while let Some(word) = words.get(at) {
        let flag = flag_of(word);
        match flag {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--request" => {
                let (value, took) = arg(flag, &words[at..])?;
                if request {
                    return Err(format!("{flag} is given twice"));
                }
                request = true;
                if value == "-" {
                    line.stdin(flag)?;
                }
                at += took;
                continue;
            }
            f if f.starts_with("--") => {}
            _ => return Err(format!("{name} takes no argument; it has flags only")),
        }
        let took = match line.field(command, &words[at..])? {
            Some(took) => took,
            None => match line.global(&words[at..])? {
                Some(took) => took,
                None => return Err(format!("{name} has no flag {flag}")),
            },
        };
        at += took;
    }
    line.call_needs()?;
    Ok(Parsed::Call(name))
}

#[cfg(test)]
#[path = "hkctl_tests.rs"]
mod tests;
