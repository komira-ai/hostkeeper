//! A mirror of the readers kbf v0.0.64 reads its state files with: the table of them in
//! `fixtures/recorded/strict-readers.json` and the checks that hold a document against it. A file
//! this coordinator writes passes when kbf's readers take it whole: no key they refuse, every
//! key they require and not null, no null in a key they do not read as an `Option`, the schema
//! they read, and a value of the type they read in every place, down into the records of the
//! halts file and the report in them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::recorded;

/// What one of kbf's readers takes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reader {
    source: String,
    #[serde(default)]
    schema: Option<u64>,
    /// Whether the reader skips a key it does not know (it has no `deny_unknown_fields`).
    #[serde(default)]
    lenient: bool,
    allowed: Vec<String>,
    required: Vec<String>,
    /// The allowed keys whose value may be null: the fields kbf reads as an `Option`, or
    /// leniently, where null is none. Any other allowed key refuses null (serde does, for a
    /// map, a list or a count that has a default and is no `Option`), and so does a required one.
    #[serde(default)]
    nullable: Vec<String>,
    nested: BTreeMap<String, String>,
    /// The type of the value of each allowed key that is not nested ([`Ty`]).
    #[serde(default)]
    types: BTreeMap<String, String>,
}

/// The most any count of a desired state may be (kbf's `MAX_COUNT`).
const MAX_COUNT: u64 = 10_000;

/// A type of the vocabulary that `strict-readers.json` documents in its `from`.
#[derive(Debug)]
enum Ty {
    Text,
    U32,
    U64,
    Version,
    Token,
    Limits,
    MinUp,
    Enum(Vec<String>),
    Map(Box<Ty>),
    List(Box<Ty>),
}

/// Whether `text` is a version kbf's `Version::parse` takes: a digit first, then letters, digits
/// and `. + ~ -`, at most 128 bytes.
pub fn is_version(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_digit)
        && bytes.len() <= 128
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b".+~-".contains(b))
}

/// Whether `text` is a token of kbf's wire: 1 to 64 bytes of `a-z`, `0-9` and `_`.
pub fn is_token(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `text` is a role name (kbf's `HostRole` grammar): 1 to 32 bytes of `a-z`, `0-9` and
/// `-`, a letter first, no `-` last and no `--`.
pub fn is_role_name(text: &str) -> bool {
    (1..=32).contains(&text.len())
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !text.ends_with('-')
        && !text.contains("--")
}

/// Whether `value` is what `Desired::checked` takes of `max_updating` or `min_up`: a map from
/// role names to counts of at most [`MAX_COUNT`].
fn limits(value: &Value, at: &str) -> Result<(), String> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("{at}: {value} is not a map of limits"))?;
    for (role, count) in map {
        if !is_role_name(role) {
            return Err(format!("{at}.{role}: {role:?} is not a role name"));
        }
        if count.as_u64().is_none_or(|n| n > MAX_COUNT) {
            return Err(format!(
                "{at}.{role}: {count} is not a count of at most {MAX_COUNT}"
            ));
        }
    }
    Ok(())
}

impl Ty {
    /// The type that `text` names.
    fn parse(text: &str) -> Result<Self, String> {
        if let Some(spellings) = text.strip_prefix("enum:") {
            let spellings: Vec<String> = spellings.split('|').map(str::to_owned).collect();
            return if spellings.iter().any(String::is_empty) {
                Err(format!("{text}: an empty spelling"))
            } else {
                Ok(Self::Enum(spellings))
            };
        }
        if let Some(inner) = text
            .strip_prefix("map<string,")
            .and_then(|rest| rest.strip_suffix('>'))
        {
            return Self::parse(inner).map(|inner| Self::Map(Box::new(inner)));
        }
        if let Some(inner) = text
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            return Self::parse(inner).map(|inner| Self::List(Box::new(inner)));
        }
        match text {
            "string" => Ok(Self::Text),
            "u32" => Ok(Self::U32),
            "u64" => Ok(Self::U64),
            "version" => Ok(Self::Version),
            "token" => Ok(Self::Token),
            "limits" => Ok(Self::Limits),
            "min_up" => Ok(Self::MinUp),
            other => Err(format!("no type named {other}")),
        }
    }

    /// Whether `value` is of this type; `Err` says where (`at`, which grows by `.key` and `[n]`
    /// into a map or an array) and what the value is not.
    fn check(&self, value: &Value, at: &str) -> Result<(), String> {
        let not =
            |what: &str| -> Result<(), String> { Err(format!("{at}: {value} is not {what}")) };
        match self {
            Self::Text if value.is_string() => Ok(()),
            Self::Text => not("a string"),
            Self::U32 if value.as_u64().is_some_and(|n| u32::try_from(n).is_ok()) => Ok(()),
            Self::U32 => not("a u32"),
            Self::U64 if value.is_u64() => Ok(()),
            Self::U64 => not("a u64"),
            Self::Version if value.as_str().is_some_and(is_version) => Ok(()),
            Self::Version => not("a version"),
            Self::Token if value.as_str().is_some_and(is_token) => Ok(()),
            Self::Token => not("a token"),
            Self::Enum(spellings)
                if value
                    .as_str()
                    .is_some_and(|text| spellings.iter().any(|s| s == text)) =>
            {
                Ok(())
            }
            Self::Enum(spellings) => not(&format!("one of {}", spellings.join(", "))),
            Self::Limits => limits(value, at),
            Self::MinUp => {
                limits(value, at)?;
                match ["storage", "frontend"]
                    .into_iter()
                    .find(|role| value.get(*role).is_none())
                {
                    Some(role) => Err(format!(
                        "{at}: {value} lacks {role}, which kbf requires of a min_up"
                    )),
                    None => Ok(()),
                }
            }
            Self::Map(inner) => {
                let map = value
                    .as_object()
                    .ok_or_else(|| format!("{at}: {value} is not a map"))?;
                map.iter()
                    .try_for_each(|(key, one)| inner.check(one, &format!("{at}.{key}")))
            }
            Self::List(inner) => {
                let items = value
                    .as_array()
                    .ok_or_else(|| format!("{at}: {value} is not an array"))?;
                items
                    .iter()
                    .enumerate()
                    .try_for_each(|(n, one)| inner.check(one, &format!("{at}[{n}]")))
            }
        }
    }
}

/// `strict-readers.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Readers {
    pub from: String,
    pub files: BTreeMap<String, String>,
    pub structs: BTreeMap<String, Reader>,
}

impl Readers {
    /// The recorded table.
    pub fn recorded() -> Self {
        serde_json::from_str(&recorded("strict-readers.json")).expect("strict-readers.json")
    }

    /// The struct kbf reads the state file `file` with.
    pub fn reader_of(&self, file: &str) -> &str {
        self.files
            .get(file)
            .unwrap_or_else(|| panic!("no state file named {file}"))
    }

    /// What is wrong with the table itself, one line each: a file that names no struct, an
    /// allowed key that is neither typed nor nested (or is both), a key of `required`,
    /// `nullable`, `types` or `nested` that is not allowed, a key that is both required and
    /// nullable, a type the mirror has no name for and a nested struct the table lacks.
    pub fn problems(&self) -> Vec<String> {
        let mut found = Vec::new();
        for (file, name) in &self.files {
            if !self.structs.contains_key(name) {
                found.push(format!("file {file}: no reader named {name}"));
            }
        }
        for (name, reader) in &self.structs {
            for key in &reader.allowed {
                match (
                    reader.types.contains_key(key),
                    reader.nested.contains_key(key),
                ) {
                    (true, false) | (false, true) => {}
                    (false, false) => {
                        found.push(format!("{name}: {key} has no type and is not nested"));
                    }
                    (true, true) => found.push(format!("{name}: {key} is typed and nested")),
                }
            }
            let mentioned = reader
                .required
                .iter()
                .chain(&reader.nullable)
                .chain(reader.types.keys())
                .chain(reader.nested.keys());
            for key in mentioned.filter(|key| !reader.allowed.contains(key)) {
                found.push(format!("{name}: {key} is not an allowed key"));
            }
            for key in reader
                .nullable
                .iter()
                .filter(|key| reader.required.contains(key))
            {
                found.push(format!("{name}: {key} is required and nullable"));
            }
            for (key, ty) in &reader.types {
                if let Err(why) = Ty::parse(ty) {
                    found.push(format!("{name}.{key}: {why}"));
                }
            }
            for (key, shape) in &reader.nested {
                let inner = shape.trim_start_matches(['[', '{']);
                let inner = inner.trim_end_matches([']', '}']);
                if !self.structs.contains_key(inner) {
                    found.push(format!("{name}.{key}: no reader named {inner}"));
                }
            }
        }
        found
    }

    /// Whether kbf's reader `name` takes `value`: no key it does not know (unless the reader
    /// skips them), every key it requires and none of those null, no null in a key that is not
    /// nullable, the schema it reads, every value of the type the table gives it, and the same
    /// of everything nested in it.
    pub fn check(&self, name: &str, value: &Value, at: &str) -> Result<(), String> {
        let reader = self
            .structs
            .get(name)
            .ok_or_else(|| format!("{at}: no reader named {name}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("{at}: {name} is not an object"))?;
        if !reader.lenient
            && let Some(key) = object.keys().find(|k| !reader.allowed.contains(k))
        {
            return Err(format!(
                "{at}: {key} is a key {name} refuses ({})",
                reader.source
            ));
        }
        if let Some(key) = reader.required.iter().find(|k| !object.contains_key(*k)) {
            return Err(format!("{at}: {name} requires {key} ({})", reader.source));
        }
        if let Some(key) = reader
            .required
            .iter()
            .find(|k| object.get(*k).is_some_and(Value::is_null))
        {
            return Err(format!(
                "{at}: {name} requires {key}, which is null ({})",
                reader.source
            ));
        }
        if let Some(key) = reader
            .allowed
            .iter()
            .find(|k| object.get(*k).is_some_and(Value::is_null) && !reader.nullable.contains(k))
        {
            return Err(format!(
                "{at}: {name} takes no null for {key} ({})",
                reader.source
            ));
        }
        if let Some(schema) = reader.schema
            && object.get("schema").and_then(Value::as_u64) != Some(schema)
        {
            return Err(format!("{at}: {name} reads schema {schema} only"));
        }
        for (key, ty) in &reader.types {
            let Some(inner) = object.get(key).filter(|v| !v.is_null()) else {
                continue;
            };
            let at = format!("{at}.{key}");
            Ty::parse(ty)
                .map_err(|why| format!("{at}: {name} types it {ty}: {why}"))
                .and_then(|ty| ty.check(inner, &at))
                .map_err(|why| format!("{why} ({name}: {})", reader.source))?;
        }
        for (key, shape) in &reader.nested {
            let Some(inner) = object.get(key).filter(|v| !v.is_null()) else {
                continue;
            };
            let at = format!("{at}.{key}");
            if let Some(item) = shape.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                let items = inner.as_array().ok_or(format!("{at}: not an array"))?;
                for (n, one) in items.iter().enumerate() {
                    self.check(item, one, &format!("{at}[{n}]"))?;
                }
            } else if let Some(item) = shape.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                let map = inner.as_object().ok_or(format!("{at}: not an object"))?;
                for (name, one) in map {
                    self.check(item, one, &format!("{at}.{name}"))?;
                }
            } else {
                self.check(shape, inner, &at)?;
            }
        }
        Ok(())
    }
}

/// The file at `path`, which holds one JSON value and a newline, as kbf's readers read it.
pub fn parse(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("a file");
    assert!(
        text.ends_with("}\n"),
        "{}: one JSON value and a newline",
        path.display()
    );
    serde_json::from_str(&text).expect("JSON")
}

impl Readers {
    /// The desired-state file `desired.json` in `dir`, checked against kbf's reader of `file`:
    /// `desired-single` for the single state, `desired-map` for the map.
    pub fn check_desired_in(&self, dir: &Path, file: &str) {
        let path = dir.join("desired.json");
        self.check(self.reader_of(file), &parse(&path), "desired.json")
            .unwrap_or_else(|why| panic!("{why}"));
    }

    /// The ops file `desired.json.ops` in `dir` and the halts file beside it, checked against
    /// kbf's readers, down into the records of both; the halts file names the digest of the ops
    /// file it goes with.
    pub fn check_ops_in(&self, dir: &Path) {
        let ops = dir.join("desired.json.ops");
        let value = parse(&ops);
        self.check(self.reader_of("ops"), &value, "ops")
            .unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(value["schema"], 1);
        let halts = dir.join("desired.json.ops.halts");
        if halts.exists() {
            let halts = parse(&halts);
            self.check(self.reader_of("halts"), &halts, "halts")
                .unwrap_or_else(|why| panic!("{why}"));
            // kbf reads the halts file leniently: the one key this coordinator adds to it, where
            // each service's update plan stands, is skipped there, and costs kbf only the plan's
            // position.
            let mut keys: Vec<&str> = halts
                .as_object()
                .expect("an object")
                .keys()
                .map(String::as_str)
                .filter(|key| *key != "rollouts")
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["ops", "services"], "the halts file's keys");
            let mut digest = String::new();
            for b in Sha256::digest(std::fs::read(&ops).expect("ops file")).iter() {
                write!(digest, "{b:02x}").expect("a string takes text");
            }
            assert_eq!(
                halts["ops"],
                digest.as_str(),
                "the halts file goes with this ops file"
            );
        }
    }
}
