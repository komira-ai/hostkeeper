//! The cells of the report and the rules each kind is held to.
//!
//! A cell is an element (a `<span>`) with `data-cell="<id>"` and exactly one source:
//!
//! - `data-result="<name>.tsv"`: a measurement. Every number printed in the cell is the value
//!   of a row of `docs/report/results/<name>.tsv` that names the cell (the row format is in
//!   [`result_rows`]);
//! - `data-source="<path>"` with `data-item="<NAME>"`: a constant of the code. `<path>` is a
//!   file of the repository and `<NAME>` a `const` or `static` declared in it; the cell prints one
//!   number, and that number in the unit word after it (`45 s`, `16 KiB`, `7 days`) is the value
//!   of the declaration ([`crate::rust_value`]);
//! - `data-open="<what fills it>"`: a measurement not taken yet. The cell prints `open` and no
//!   digit of any script at all.
//!
//! A filled cell prints at least one number. Every attribute of every tag is written in double
//! quotes, so no cell attribute can hide from these rules by being written another way.

use std::collections::BTreeMap;
use std::path::Path;

use crate::html::{
    Tag, Value, attr, attributes, element_end, line_of, numbers, plain, quantities, tags, tokens,
};

/// The attributes that make or describe a cell.
pub const CELL_ATTRS: &[&str] = &[
    "data-cell",
    "data-result",
    "data-source",
    "data-item",
    "data-open",
];

/// Where a cell's value comes from.
#[derive(Debug, PartialEq)]
pub enum Kind {
    Result(String),
    Source { path: String, item: String },
    Open(String),
}

/// One cell: its id, its source, the line of its start tag, its text, and the byte range of the
/// whole element (start tag to the end of its end tag).
#[derive(Debug)]
pub struct Cell {
    pub id: String,
    pub kind: Kind,
    pub line: usize,
    pub text: String,
    pub span: (usize, usize),
}

/// The cells of `text` and one line per problem in how they are written: an attribute not in
/// double quotes or with no value, a cell attribute on an element with no `data-cell`, a cell
/// with no source or more than one, a `data-source` without `data-item` or the reverse, a cell
/// that is never closed, and an id used twice.
pub fn cells(text: &str) -> (Vec<Cell>, Vec<String>) {
    let all = tags(text);
    let mut out = Vec::new();
    let mut problems = quote_problems(text, &all);
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (k, tag) in all.iter().enumerate() {
        if tag.closing {
            continue;
        }
        let line = line_of(text, tag.start);
        let get = |name: &str| attr(tag.attrs, name);
        let Some(id) = get("data-cell") else {
            for name in &CELL_ATTRS[1..] {
                if get(name).is_some() {
                    problems.push(format!(
                        "line {line}: {name} on an element with no data-cell"
                    ));
                }
            }
            continue;
        };
        let kind = match (
            get("data-result"),
            get("data-source"),
            get("data-item"),
            get("data-open"),
        ) {
            (Some(name), None, None, None) => Kind::Result(name.to_owned()),
            (None, Some(path), Some(item), None) => Kind::Source {
                path: path.to_owned(),
                item: item.to_owned(),
            },
            (None, None, None, Some(why)) => Kind::Open(why.to_owned()),
            (None, Some(_), None, None) => {
                problems.push(format!(
                    "line {line}: cell {id} has data-source but no data-item"
                ));
                continue;
            }
            (None, None, Some(_), None) => {
                problems.push(format!(
                    "line {line}: cell {id} has data-item but no data-source"
                ));
                continue;
            }
            (None, None, None, None) => {
                problems.push(format!(
                    "line {line}: cell {id} has no data-result, data-source or data-open"
                ));
                continue;
            }
            _ => {
                problems.push(format!("line {line}: cell {id} has more than one source"));
                continue;
            }
        };
        let Some(end) = element_end(&all, k) else {
            problems.push(format!("line {line}: cell {id} is never closed"));
            continue;
        };
        if let Some(first) = seen.insert(id.to_owned(), line) {
            problems.push(format!(
                "line {line}: cell {id} is used again (first on line {first})"
            ));
        }
        let close = text[end..].find('>').map_or(text.len(), |at| end + at + 1);
        out.push(Cell {
            id: id.to_owned(),
            kind,
            line,
            text: plain(&text[tag.end..end]),
            span: (tag.start, close),
        });
    }
    (out, problems)
}

/// One line per attribute of a start tag not written in double quotes, and per cell attribute
/// with no value.
fn quote_problems(text: &str, all: &[Tag<'_>]) -> Vec<String> {
    let mut found = Vec::new();
    for tag in all.iter().filter(|t| !t.closing) {
        let line = line_of(text, tag.start);
        for a in attributes(tag.attrs) {
            match a.value {
                Value::Double(_) => {}
                Value::Single | Value::Bare => {
                    found.push(format!(
                        "line {line}: attribute {} is not double-quoted",
                        a.name
                    ));
                }
                Value::Missing if CELL_ATTRS.iter().any(|c| a.name.eq_ignore_ascii_case(c)) => {
                    found.push(format!("line {line}: attribute {} has no value", a.name));
                }
                Value::Missing => {}
            }
        }
    }
    found
}

/// One row of a result file.
pub struct Row {
    pub id: String,
    pub value: f64,
    /// The unit the value is in (`s`, `MiB`); empty when the row names none.
    pub unit: String,
    /// The cell ids that print the value.
    pub cells: Vec<String>,
}

/// The rows of a result file: tab-separated columns kind, id, value, unit, how and cells, where
/// `cells` lists, separated by commas, every cell id that prints the row's value. Lines starting
/// with `#` and blank lines are skipped.
pub fn result_rows(text: &str) -> Result<Vec<Row>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 3 {
            return Err(format!(
                "line {}: fewer than 3 tab-separated columns",
                n + 1
            ));
        }
        let value = columns[2]
            .parse::<f64>()
            .map_err(|_| format!("line {}: value {:?} is not a number", n + 1, columns[2]))?;
        let cells = columns
            .get(5)
            .map(|c| c.split(',').map(|id| id.trim().to_owned()).collect())
            .unwrap_or_default();
        out.push(Row {
            id: columns[1].to_owned(),
            value,
            unit: columns.get(3).map_or("", |u| u.trim()).to_owned(),
            cells,
        });
    }
    Ok(out)
}

/// Whether `name` is a plain file name of a result: lower-case letters, digits, `.`, `-` and
/// `_`, ending in `.tsv`, no `..`.
fn result_name_ok(name: &str) -> bool {
    !name.contains("..")
        && name.ends_with(".tsv")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

/// Whether `path` is a plain relative path inside the repository: no leading `/`, no `\`, no
/// empty, `.` or `..` component.
fn source_path_ok(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// One line per dotted version (`1.2.3`) in a filled cell, and per filled cell with no number.
fn shape_problems(cell: &Cell) -> Vec<String> {
    let mut found: Vec<String> = tokens(&cell.text)
        .into_iter()
        .filter(|t| crate::html::is_version(t))
        .map(|t| {
            format!(
                "line {}: {}: {t} is a dotted version, not a number",
                cell.line, cell.id
            )
        })
        .collect();
    if numbers(&cell.text).is_empty() {
        found.push(format!(
            "line {}: {}: a filled cell prints no number",
            cell.line, cell.id
        ));
    }
    found
}

/// One line per number in a `data-result` cell that no row of its file naming the cell holds,
/// per number printed in another unit than its row's (`unit` column; the word after the number
/// must be it), per unusable result name or file, and per shape problem.
pub fn result_problems(cells: &[Cell], results: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for cell in cells {
        let Kind::Result(name) = &cell.kind else {
            continue;
        };
        let (line, id) = (cell.line, &cell.id);
        if !result_name_ok(name) {
            problems.push(format!(
                "line {line}: {id}: data-result {name:?} is not a plain .tsv name"
            ));
            continue;
        }
        let rows = match std::fs::read_to_string(results.join(name)) {
            Ok(text) => match result_rows(&text) {
                Ok(rows) => rows,
                Err(why) => {
                    problems.push(format!("line {line}: {id}: {name}: {why}"));
                    continue;
                }
            },
            Err(_) => {
                problems.push(format!(
                    "line {line}: {id}: result file {name} does not exist"
                ));
                continue;
            }
        };
        problems.extend(shape_problems(cell));
        for (number, printed_unit) in quantities(&cell.text) {
            let holders: Vec<&Row> = rows
                .iter()
                .filter(|r| (r.value - number).abs() < 1e-9)
                .collect();
            let unit_ok =
                |r: &&Row| r.unit.is_empty() || printed_unit.as_deref() == Some(r.unit.as_str());
            if let Some(row) = holders.iter().find(|r| r.cells.iter().any(|c| c == id))
                && !unit_ok(row)
            {
                problems.push(format!(
                    "line {line}: {id}: the number {number} is printed in {}, its row {} is in {}",
                    printed_unit.as_deref().unwrap_or("no unit"),
                    row.id,
                    row.unit
                ));
                continue;
            }
            if holders.is_empty() {
                problems.push(format!(
                    "line {line}: {id}: the number {number} is not a value of {name}"
                ));
            } else if !holders.iter().any(|r| r.cells.iter().any(|c| c == id)) {
                let ids: Vec<&str> = holders.iter().map(|r| r.id.as_str()).collect();
                problems.push(format!(
                    "line {line}: {id}: the number {number} is not a value of {name} for this cell; the rows holding it ({}) do not name {id}",
                    ids.join(", ")
                ));
            }
        }
    }
    problems
}

/// The size of a unit word printed after a number, in the base unit of its kind (seconds,
/// bytes); `None` for a word that is no unit, which leaves the number as it is.
pub fn unit_factor(word: &str) -> Option<f64> {
    Some(match word {
        "ms" => 1e-3,
        "s" | "second" | "seconds" => 1.0,
        "minute" | "minutes" => 60.0,
        "hour" | "hours" => 3600.0,
        "day" | "days" => 86_400.0,
        "year" | "years" => 365.0 * 86_400.0,
        "byte" | "bytes" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    })
}

/// One line per `data-source` cell that does not print exactly one number, or whose number,
/// times the unit word after it ([`unit_factor`]), is not the value of its declaration
/// ([`crate::rust_value`]); and per source that is not a plain path, cannot be read, declares
/// no such item or cannot be evaluated.
pub fn source_problems(cells: &[Cell], root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for cell in cells {
        let Kind::Source { path, item } = &cell.kind else {
            continue;
        };
        let (line, id) = (cell.line, &cell.id);
        if !source_path_ok(path) {
            problems.push(format!(
                "line {line}: {id}: data-source {path:?} is not a plain path in the repository"
            ));
            continue;
        }
        let Ok(source) = std::fs::read_to_string(root.join(path)) else {
            problems.push(format!("line {line}: {id}: source {path} cannot be read"));
            continue;
        };
        let Some(text) = crate::rust_value::declared_text(&source, item) else {
            problems.push(format!(
                "line {line}: {id}: {path} declares no const or static {item}"
            ));
            continue;
        };
        let value = match crate::rust_value::evaluate(text) {
            Ok(value) => value,
            Err(why) => {
                problems.push(format!("line {line}: {id}: {item} in {path}: {why}"));
                continue;
            }
        };
        problems.extend(shape_problems(cell));
        let printed = quantities(&cell.text);
        if printed.len() > 1 {
            problems.push(format!(
                "line {line}: {id}: a cell of the code prints one number, not {}",
                printed.len()
            ));
            continue;
        }
        for (number, unit) in printed {
            let factor = unit.as_deref().and_then(unit_factor).unwrap_or(1.0);
            if ((number * factor) - value).abs() > 1e-9 * value.abs().max(1.0) {
                problems.push(format!(
                    "line {line}: {id}: {number}{} is not the value of {item} in {path}, {value}",
                    unit.map(|u| format!(" {u}")).unwrap_or_default()
                ));
            }
        }
    }
    problems
}

/// One line per open cell that prints a digit or does not say `open`.
pub fn open_problems(cells: &[Cell]) -> Vec<String> {
    let mut problems = Vec::new();
    for cell in cells {
        if !matches!(cell.kind, Kind::Open(_)) {
            continue;
        }
        if cell.text.chars().any(char::is_numeric) {
            problems.push(format!(
                "line {}: {}: an open cell prints a digit: {:?}",
                cell.line, cell.id, cell.text
            ));
        }
        if !cell.text.to_lowercase().contains("open") {
            problems.push(format!(
                "line {}: {}: an open cell does not say open: {:?}",
                cell.line, cell.id, cell.text
            ));
        }
    }
    problems
}

/// One line per id of `required` that is not a cell, or is a cell whose value comes from the
/// code: a measurement is measured or open, never a constant.
pub fn required_problems(cells: &[Cell], required: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    for id in required {
        match cells.iter().find(|c| c.id == *id) {
            None => problems.push(format!("the measurement cell {id} is missing")),
            Some(cell) if matches!(cell.kind, Kind::Source { .. }) => problems.push(format!(
                "line {}: the measurement cell {id} takes its value from the code, not from a measurement",
                cell.line
            )),
            Some(_) => {}
        }
    }
    problems
}

/// The open cells, one line each: the id and what fills it.
pub fn open_list(cells: &[Cell]) -> Vec<String> {
    cells
        .iter()
        .filter_map(|c| match &c.kind {
            Kind::Open(why) => Some(format!("{} (line {}): {why}", c.id, c.line)),
            _ => None,
        })
        .collect()
}

/// One line per open cell whose id is not one of `measurements`: only a measurement the farm
/// run has not produced is ever left open.
pub fn stray_open_problems(cells: &[Cell], measurements: &[&str]) -> Vec<String> {
    cells
        .iter()
        .filter(|c| matches!(c.kind, Kind::Open(_)) && !measurements.contains(&c.id.as_str()))
        .map(|c| format!("line {}: {} is open but is not a measurement", c.line, c.id))
        .collect()
}

/// One line when no cell reads its value from the code, so the rule of code cells would check
/// nothing.
pub fn source_rule_problems(cells: &[Cell]) -> Vec<String> {
    if cells.iter().any(|c| matches!(c.kind, Kind::Source { .. })) {
        Vec::new()
    } else {
        vec!["no cell reads its value from the code".to_owned()]
    }
}
