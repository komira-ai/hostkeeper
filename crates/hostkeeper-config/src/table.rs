//! Reading a TOML document against the keys it may hold.
//!
//! Each file is read in two passes. [`unknown_key`] walks the whole document against a [`Shape`]
//! and finds the first key no table knows, so an unknown key is reported before any value is
//! judged. [`Fields`] then reads each value, naming the dotted key of a missing or ill-typed one.

/// What a key holds.
pub(crate) enum Shape {
    /// A value, judged when it is read.
    Value,
    /// A table with exactly these keys.
    Table(&'static [(&'static str, Shape)]),
}

/// What is wrong with a document, before it becomes the error of one file.
pub(crate) enum Problem {
    /// `key` is in `table` (`""` for the top level), which does not know it.
    Unknown { table: String, key: String },
    /// The value of the dotted `key` is missing or not what it must be.
    Invalid { key: String, why: String },
}

/// The dotted name of `key` in the table named `table` (`""` for the top level).
pub(crate) fn dotted(table: &str, key: &str) -> String {
    [table, key]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

/// The first key of `table` (named `name`) or of a table below it that its `known` keys do not
/// list. Keys are visited in order, so the answer does not depend on how the file is laid out.
pub(crate) fn unknown_key(
    name: &str,
    table: &toml::Table,
    known: &[(&'static str, Shape)],
) -> Option<Problem> {
    table.iter().find_map(|(key, value)| {
        let Some((_, shape)) = known.iter().find(|(known, _)| known == key) else {
            return Some(Problem::Unknown {
                table: name.to_owned(),
                key: key.clone(),
            });
        };
        let below = dotted(name, key);
        match (shape, value) {
            (Shape::Table(keys), toml::Value::Table(inner)) => unknown_key(&below, inner, keys),
            _ => None,
        }
    })
}

/// The values of one table, read by key.
pub(crate) struct Fields<'a> {
    name: &'a str,
    table: &'a toml::Table,
}

impl<'a> Fields<'a> {
    pub fn new(name: &'a str, table: &'a toml::Table) -> Fields<'a> {
        Fields { name, table }
    }

    /// The problem of the value of `key`.
    pub fn invalid(&self, key: &str, why: impl Into<String>) -> Problem {
        Problem::Invalid {
            key: dotted(self.name, key),
            why: why.into(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&'a toml::Value> {
        self.table.get(key)
    }

    fn required(&self, key: &str) -> Result<&'a toml::Value, Problem> {
        self.get(key).ok_or_else(|| self.invalid(key, "is missing"))
    }

    fn typed<T>(
        &self,
        key: &str,
        value: &'a toml::Value,
        read: impl FnOnce(&'a toml::Value) -> Option<T>,
        what: &str,
    ) -> Result<T, Problem> {
        read(value).ok_or_else(|| {
            self.invalid(
                key,
                format!("is {} {}, not {what}", article(value), value.type_str()),
            )
        })
    }

    pub fn string(&self, key: &str) -> Result<&'a str, Problem> {
        self.typed(key, self.required(key)?, toml::Value::as_str, "a string")
    }

    /// A whole number from 0 up.
    pub fn number(&self, key: &str) -> Result<u64, Problem> {
        let value = self.required(key)?;
        self.typed(
            key,
            value,
            |value| value.as_integer().and_then(|n| u64::try_from(n).ok()),
            "a whole number from 0 up",
        )
    }

    /// [`Fields::number`], or `default` when the key is absent.
    pub fn number_or(&self, key: &str, default: u64) -> Result<u64, Problem> {
        self.get(key).map_or(Ok(default), |_| self.number(key))
    }

    pub fn boolean(&self, key: &str) -> Result<bool, Problem> {
        self.typed(key, self.required(key)?, toml::Value::as_bool, "a boolean")
    }

    /// A list of strings.
    pub fn strings(&self, key: &str) -> Result<Vec<&'a str>, Problem> {
        let items = self.typed(
            key,
            self.required(key)?,
            toml::Value::as_array,
            "a list of strings",
        )?;
        items
            .iter()
            .map(|item| self.typed(key, item, toml::Value::as_str, "a list of strings"))
            .collect()
    }

    /// The table under `key`, or `None` when it is absent.
    pub fn table(&self, key: &str) -> Result<Option<&'a toml::Table>, Problem> {
        self.get(key)
            .map(|value| self.typed(key, value, toml::Value::as_table, "a table"))
            .transpose()
    }
}

/// `a` or `an`, for the TOML type of `value`.
fn article(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::Integer(_) | toml::Value::Array(_) => "an",
        _ => "a",
    }
}

/// The line of a parse error: 1 for the first line, from the start of the error's span.
pub(crate) fn line_of(text: &str, error: &toml::de::Error) -> usize {
    let at = error.span().map_or(0, |span| span.start);
    text.get(..at).unwrap_or(text).matches('\n').count() + 1
}

/// Parses `text` as a TOML document, or gives the line and message of why it is not one.
pub(crate) fn parse(text: &str) -> Result<toml::Table, (usize, String)> {
    text.parse::<toml::Table>()
        .map_err(|error| (line_of(text, &error), error.message().to_owned()))
}
