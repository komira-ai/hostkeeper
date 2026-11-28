//! `hk`'s command line, read as `hk` reads it: the global flags of `docs/cli.md`'s table
//! (written by `hostkeeper_hk::markdown`), then a service and a method of the command table
//! hk serves (`hostkeeper_hk::is_served`), then that method's flags; or one of the hand verbs
//! of the ssh tunnel, `ssh <host-id>`, `ssh-proxy`, `ssh-revoke` and `ssh-status`, or
//! `xcode-push <version>|<archive>` (until X7), `tailnet-wait` and `settings init`, which are not in the table (a word after `ssh` that is a method of the service `ssh` is that method, any
//! other is a host id).
//!
//! The rules are those of hk's own parser (`crates/hostkeeper-hk/src/dispatch.rs`), each word in
//! the order hk takes it: `--help` or `-h` anywhere, `--version` only before the service; a
//! flag's value as `--flag value` or `--flag=value`, a value that starts with `--` only in the
//! second form; a switch with no value; each global flag and each singular field at most once; at
//! most one input that reads stdin; a bool `true` or `false`, an enum one of its values, an
//! integer in its kind's range, a float a JSON number; a map `key=value` with each key once; a
//! secret only as `--<flag>-file` or `--<flag>-stdin`. What hk checks only once it runs (a file it
//! reads, a request file's keys) is not read here, but a call needs what it needs of every line:
//! `--endpoint` as `https://<host>[:<port>]` (a host of letters, digits, `.` and `-`, or `[...]`
//! of an IPv6 address, or a placeholder `<...>` in its place; a port of one to five digits),
//! `--ca-file`, and exactly one of the token's three inputs. The hand verbs take `--token-file`
//! alone of the credentials and no `--response-file`.

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

/// The flags of [`GLOBALS_HEADING`]'s table in `cli`, the text `hostkeeper_hk::markdown`
/// writes, but `--help` and `--version`, which hk takes before any other flag.
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

/// The commands of `commands` hk has.
pub(crate) fn served(commands: &'static [Command]) -> Vec<&'static Command> {
    commands
        .iter()
        .filter(|c| hostkeeper_hk::is_served(c))
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
        if !is_endpoint(&self.endpoint) {
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

/// Whether `value` is an endpoint hk takes, its pattern
/// `^https://([A-Za-z0-9.-]+|\[[0-9A-Fa-f:.]+\])(:[0-9]{1,5})?/?$`, with a placeholder `<...>`
/// (no white space in it) allowed for the host.
fn is_endpoint(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    // The host (`None` for an IPv6 address, already checked) and the port.
    let (host, port) = match rest.strip_prefix('[') {
        Some(v6) => {
            let Some((address, after)) = v6.split_once(']') else {
                return false;
            };
            let address_ok = !address.is_empty()
                && address
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
            let port = match after.strip_prefix(':') {
                Some(port) => Some(port),
                None if after.is_empty() => None,
                None => return false,
            };
            if !address_ok {
                return false;
            }
            (None, port)
        }
        None => match rest.split_once(':') {
            Some((host, port)) => (Some(host), Some(port)),
            None => (Some(rest), None),
        },
    };
    let host_ok = host.is_none_or(|h| {
        (h.len() > 2 && h.starts_with('<') && h.ends_with('>'))
            || (!h.is_empty()
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
    });
    let port_ok =
        port.is_none_or(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()));
    host_ok && port_ok
}

/// Whether `value` is a whole number of seconds, 1 to 999999.
fn is_secs(value: &str) -> bool {
    !value.starts_with('0')
        && (1..=6).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `value` is a number as JSON writes it, hk's float pattern
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
            return Err(format!("--{flag} is no flag hk writes"));
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

/// The verbs hk has by hand, besides `ssh` with a host id: not in the command table.
pub(crate) const HAND_VERBS: &[&str] = &[
    "ssh-proxy",
    "ssh-revoke",
    "ssh-status",
    "xcode-push",
    "tailnet-wait",
    "settings",
];

/// The methods of a generated service that hk has by hand (`hostkeeper_hk`'s `hand.rs`): the
/// service, the method and the flags of its own.
pub(crate) const HAND_METHODS: &[(&str, &str, &[&str])] = &[
    ("xcode", "add", &["--mac"]),
    ("xcode", "verify", &["--apple-root", "--mac"]),
    (
        "releases",
        "wait",
        &["--service", "--version", "--wait-secs", "--host-group"],
    ),
];

/// The flags of its own of the hand method `method` of `service`, when it is one.
pub(crate) fn hand_method(service: &str, method: &str) -> Option<&'static [&'static str]> {
    HAND_METHODS
        .iter()
        .find(|(s, m, _)| *s == service && *m == method)
        .map(|(.., flags)| *flags)
}

/// What `hk xcode-push` says when it is not given exactly one version or archive.
const XCODE_PUSH_ONE: &str = "hk xcode-push takes one Xcode version (27.2) or the path of an \
                              archive (~/Downloads/Xcode_27.2.xip)";

/// The flags of a hand verb of its own, as `hk ssh --identity FILE --reason TEXT <host-id>`:
/// those of `ssh` and `ssh-proxy`, and those of `ssh-revoke`.
const HAND_IDENTITY_FLAGS: &[&str] = &["--identity", "--reason"];
const HAND_REVOKE_FLAGS: &[&str] = &["--operator", "--serial", "--host"];
/// The flag of `hk xcode-push` of its own: the farm Mac that checks the archive off macOS.
const HAND_XCODE_PUSH_FLAGS: &[&str] = &["--mac"];
/// The flags of `hk tailnet-wait`, each with the whole numbers it takes.
const TAILNET_WAIT_FLAGS: &[(&str, u32, u32)] =
    &[("--attempts", 1, 100), ("--interval-secs", 0, 600)];
const HAND_TAILNET_WAIT_FLAGS: &[&str] = &["--attempts", "--interval-secs"];

/// The flags of the hand verb `verb` (`ssh` for `ssh <host-id>`) of its own, not the global ones.
pub(crate) fn hand_flags(verb: &str) -> &'static [&'static str] {
    match verb {
        "ssh" | "ssh-proxy" => HAND_IDENTITY_FLAGS,
        "ssh-revoke" => HAND_REVOKE_FLAGS,
        "tailnet-wait" => HAND_TAILNET_WAIT_FLAGS,
        "xcode-push" => HAND_XCODE_PUSH_FLAGS,
        _ => &[],
    }
}

/// What `words` (the words after the hand verb `verb`) ask for: `ssh <host-id>` with its flags
/// and, after `--`, ssh's own arguments, `ssh-proxy <host-id>`, `ssh-revoke` with one of
/// `--operator`, `--serial` and `--host`, `ssh-status <host-id>`, `xcode-push` with one version
/// or archive path. They take `--token-file` alone of the credentials and no `--response-file`,
/// and need the endpoint and the CA file.
fn hand(line: &mut Line<'_>, verb: &str, words: &[String]) -> Result<Parsed, String> {
    if verb == "tailnet-wait" {
        return tailnet_wait(line, words);
    }
    if verb == "settings" {
        return settings(line, words);
    }
    let own = hand_flags(verb);
    let mut host: Option<&str> = None;
    let mut selectors: Vec<&str> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let flag = flag_of(word);
        if word == "--" && verb == "ssh" {
            break;
        }
        if matches!(flag, "--help" | "-h") {
            return Ok(Parsed::Help);
        }
        if let Some(own) = own.iter().find(|f| **f == flag) {
            let (value, took) = arg(flag, &words[at..])?;
            if seen.contains(own) {
                return Err(format!("{flag} is given twice"));
            }
            seen.push(own);
            if *own == "--serial"
                && (value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err("--serial takes a whole number".to_owned());
            }
            if verb == "ssh-revoke" {
                selectors.push(own);
            }
            at += took;
        } else if word.starts_with('-') {
            match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("{verb} has no flag {flag}")),
            }
        } else if verb == "ssh-revoke" {
            return Err(format!("{verb} takes no argument; it has flags only"));
        } else if host.replace(word.as_str()).is_some() {
            if verb == "xcode-push" {
                return Err(XCODE_PUSH_ONE.to_owned());
            }
            return Err(format!("{verb} takes one host id"));
        } else {
            at += 1;
        }
    }
    if verb == "ssh-revoke" {
        if selectors.len() != 1 {
            return Err(
                "ssh-revoke takes exactly one of --operator, --serial and --host".to_owned(),
            );
        }
    } else if host.is_none() && verb == "xcode-push" {
        return Err(XCODE_PUSH_ONE.to_owned());
    } else if host.is_none() {
        return Err(format!("{verb} needs a host id"));
    }
    let has = |name: &str| line.given.iter().any(|g| g == name);
    if (has("token-stdin") || has("github-oidc-audience") || has("response-file"))
        && verb == "xcode-push"
    {
        return Err(
            "hk xcode-push takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it (stdin is the terminal xcodes asks a two-factor code \
             on)"
            .to_owned(),
        );
    }
    if has("token-stdin") || has("github-oidc-audience") || has("response-file") {
        return Err(format!(
            "{verb} takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it"
        ));
    }
    line.call_needs()?;
    Ok(Parsed::Call(verb.to_owned()))
}

/// What `words` (the words after `settings`) ask for: the one word `init`, the global flags, the
/// three of `--endpoint`, `--ca-file` and `--token-file`, and no other credential and no
/// `--response-file`; in hk's words.
fn settings(line: &mut Line<'_>, words: &[String]) -> Result<Parsed, String> {
    let mut rest: Vec<&str> = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let flag = flag_of(word);
        if matches!(flag, "--help" | "-h") {
            return Ok(Parsed::Help);
        }
        if word.starts_with('-') {
            match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("hk settings has no flag {flag}")),
            }
        } else {
            rest.push(word);
            at += 1;
        }
    }
    if rest != ["init"] {
        return Err(
            "settings takes the one word init; 'hk settings --help' says what it does".to_owned(),
        );
    }
    let has = |name: &str| line.given.iter().any(|g| g == name);
    if has("token-stdin") || has("github-oidc-audience") || has("response-file") {
        return Err(
            "hk settings init takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it"
                .to_owned(),
        );
    }
    if !(has("endpoint") && has("ca-file") && has("token-file")) {
        return Err("hk settings init needs --endpoint, --ca-file and --token-file".to_owned());
    }
    if !is_endpoint(&line.endpoint) {
        return Err("--endpoint takes https://<host>[:<port>]".to_owned());
    }
    Ok(Parsed::Call("settings".to_owned()))
}

/// What `words` (the words after `xcode add`) ask for: at most one version or archive path,
/// `--mac` once with a value, and the global flags, of which the token file is the only credential
/// and none is needed (the settings file gives what the line lacks); in hk's words.
fn xcode_add(line: &mut Line<'_>, words: &[String]) -> Result<Parsed, String> {
    let mut paths = 0;
    let mut mac = false;
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let flag = flag_of(word);
        if matches!(flag, "--help" | "-h") {
            return Ok(Parsed::Help);
        }
        if flag == "--mac" {
            let (_, took) = arg(flag, &words[at..])?;
            if std::mem::replace(&mut mac, true) {
                return Err("--mac is given twice".to_owned());
            }
            at += took;
        } else if word.starts_with('-') {
            match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("hk xcode add has no flag {flag}")),
            }
        } else {
            paths += 1;
            at += 1;
        }
    }
    if paths > 1 {
        return Err(
            "hk xcode add takes at most one Xcode version (27.2) or the path of an \
                    archive (~/Downloads/Xcode_27.2.xip)"
                .to_owned(),
        );
    }
    let has = |name: &str| line.given.iter().any(|g| g == name);
    if has("token-stdin") || has("github-oidc-audience") || has("response-file") {
        return Err(
            "hk xcode add takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it (stdin is the terminal xcodes asks a two-factor code \
             on)"
            .to_owned(),
        );
    }
    Ok(Parsed::Call("xcode add".to_owned()))
}

/// What `words` (the words after `xcode verify`) ask for: one archive, `--apple-root` and `--mac`
/// each once with a value, and the global flags, none of which it needs; in hk's words.
fn xcode_verify(line: &mut Line<'_>, words: &[String]) -> Result<Parsed, String> {
    let mut paths = 0;
    let mut root = false;
    let mut mac = false;
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let flag = flag_of(word);
        if matches!(flag, "--help" | "-h") {
            return Ok(Parsed::Help);
        }
        if flag == "--apple-root" {
            let (_, took) = arg(flag, &words[at..])?;
            if std::mem::replace(&mut root, true) {
                return Err("--apple-root is given twice".to_owned());
            }
            at += took;
        } else if flag == "--mac" {
            let (_, took) = arg(flag, &words[at..])?;
            if std::mem::replace(&mut mac, true) {
                return Err("--mac is given twice".to_owned());
            }
            at += took;
        } else if word.starts_with('-') {
            match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("hk xcode verify has no flag {flag}")),
            }
        } else {
            paths += 1;
            at += 1;
        }
    }
    if paths != 1 {
        return Err(
            "hk xcode verify takes the path of one archive (~/Downloads/Xcode_27.2.xip)".to_owned(),
        );
    }
    Ok(Parsed::Call("xcode verify".to_owned()))
}

/// What `words` (the words after `releases wait`) ask for: `--service`, `--version` and
/// `--wait-secs` (a whole number from 1 to 1800), and `--host-group`, each once with a value, no
/// argument, and the global flags of a call; in hk's words.
fn releases_wait(line: &mut Line<'_>, words: &[String]) -> Result<Parsed, String> {
    let own = ["--service", "--version", "--wait-secs", "--host-group"];
    let mut seen: Vec<&str> = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let flag = flag_of(word);
        if matches!(flag, "--help" | "-h") {
            return Ok(Parsed::Help);
        }
        if let Some(own) = own.iter().find(|f| **f == flag) {
            let (value, took) = arg(flag, &words[at..])?;
            if seen.contains(own) {
                return Err(format!("{flag} is given twice"));
            }
            seen.push(own);
            let digits = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
            let in_range = digits && value.parse::<u32>().is_ok_and(|n| (1..=1800).contains(&n));
            if flag == "--wait-secs" && !in_range {
                return Err("--wait-secs takes a whole number from 1 to 1800".to_owned());
            }
            at += took;
        } else if word.starts_with('-') {
            match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("hk releases wait has no flag {flag}")),
            }
        } else {
            return Err(format!("hk releases wait takes no argument: {word}"));
        }
    }
    if own[..3].iter().any(|f| !seen.contains(f)) {
        return Err("hk releases wait needs --service, --version and --wait-secs".to_owned());
    }
    line.call_needs()?;
    Ok(Parsed::Call("releases wait".to_owned()))
}

/// What `words` (the words after `tailnet-wait`) ask for: `--attempts` and `--interval-secs`,
/// each once with a whole number in its range, no argument, no credential and no
/// `--response-file`, and the endpoint and the CA file; in hk's words.
fn tailnet_wait(line: &mut Line<'_>, words: &[String]) -> Result<Parsed, String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let flag = flag_of(word);
        if matches!(flag, "--help" | "-h") {
            return Ok(Parsed::Help);
        }
        if let Some((own, min, max)) = TAILNET_WAIT_FLAGS.iter().find(|(f, ..)| *f == flag) {
            let (value, took) = arg(flag, &words[at..])?;
            if seen.contains(own) {
                return Err(format!("{flag} is given twice"));
            }
            seen.push(own);
            let in_range = !value.is_empty()
                && value.len() <= 6
                && value.bytes().all(|b| b.is_ascii_digit())
                && value
                    .parse::<u32>()
                    .is_ok_and(|n| (*min..=*max).contains(&n));
            if !in_range {
                return Err(format!("{flag} takes a whole number from {min} to {max}"));
            }
            at += took;
        } else if word.starts_with('-') {
            match line.global(&words[at..])? {
                Some(took) => at += took,
                None => return Err(format!("hk tailnet-wait has no flag {flag}")),
            }
        } else {
            return Err(format!("hk tailnet-wait takes no argument: {word}"));
        }
    }
    let has = |name: &str| line.given.iter().any(|g| g == name);
    if [
        "token-file",
        "token-stdin",
        "github-oidc-audience",
        "response-file",
    ]
    .iter()
    .any(|c| has(c))
    {
        return Err(
            "hk tailnet-wait takes no token and writes no answer: --token-file, \
                    --token-stdin, --github-oidc-audience and --response-file are not for it"
                .to_owned(),
        );
    }
    if !(has("endpoint") && has("ca-file")) {
        return Err("hk tailnet-wait needs --endpoint and --ca-file".to_owned());
    }
    if !is_endpoint(&line.endpoint) {
        return Err("--endpoint takes https://<host>[:<port>]".to_owned());
    }
    Ok(Parsed::Call("tailnet-wait".to_owned()))
}

/// What `words` (the line's words after `hk`) ask for, or the usage error hk exits 2
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
        return Err("no service: hk <service> <method> [flags]".to_owned());
    };
    if HAND_VERBS.contains(&service.as_str()) {
        return hand(&mut line, service, &words[at + 1..]);
    }
    if !commands.iter().any(|c| c.service_command == service) {
        return Err(format!("no service {service}; 'hk --help' lists them"));
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
        return Err(format!("no method: hk {service} <method> [flags]"));
    };
    let Some(command) = commands
        .iter()
        .find(|c| c.service_command == service && c.method_command == method)
    else {
        if service == "ssh" {
            // Not a method of the service `ssh`: a host id.
            return hand(&mut line, service, &words[at..]);
        }
        let rest = &words[at + 1..];
        match (service.as_str(), method.as_str()) {
            ("xcode", "add") => return xcode_add(&mut line, rest),
            ("xcode", "verify") => return xcode_verify(&mut line, rest),
            ("releases", "wait") => return releases_wait(&mut line, rest),
            _ => {}
        }
        return Err(format!(
            "the service {service} has no method {method}; 'hk {service} --help' lists them"
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
#[path = "hk_tests.rs"]
mod tests;
