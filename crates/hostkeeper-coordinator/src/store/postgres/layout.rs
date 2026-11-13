//! The lab's layout ([`crate::layout`]) as the rows of the tables that migration `0015` adds:
//! `lab_cluster`, `lab_rows`, `lab_racks`, `lab_placements` and, from migration `0016`, `lab_devices`, a row for the name and one per row, rack and placement, keyed by id
//! (a placement by its subject's key), holding the record as the layout file holds it, without
//! the key.
//!
//! A database that holds no row holds an empty layout. An older release never reads any of the
//! tables (`0015` is `may-run`). A load reads every row through the file's reader, so it is as
//! strict as a start on the files, and refuses a layout that breaks a rule of [`Layout::new`],
//! naming the tables.

use super::rows::{KeyTy, KeyVal, Rows, Table, Tables, Ty, Val};
use super::state::{json_of, place};
use crate::layout::{Layout, LayoutError};

/// The table of the rows.
pub(crate) const LAB_ROWS: Table = Table {
    name: "lab_rows",
    keys: &[("id", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The table of the racks.
pub(crate) const LAB_RACKS: Table = Table {
    name: "lab_racks",
    keys: &[("id", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The table of the placements.
pub(crate) const LAB_PLACEMENTS: Table = Table {
    name: "lab_placements",
    keys: &[("subject", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The table of the cluster: one row, `name`, whose record is the cluster's name as a JSON text.
pub(crate) const LAB_CLUSTER: Table = Table {
    name: "lab_cluster",
    keys: &[("id", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The table of the network devices (migration `0016`).
pub(crate) const LAB_DEVICES: Table = Table {
    name: "lab_devices",
    keys: &[("id", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The layout's tables.
pub(crate) const TABLES: [Table; 5] = [
    LAB_CLUSTER,
    LAB_ROWS,
    LAB_RACKS,
    LAB_PLACEMENTS,
    LAB_DEVICES,
];

/// The rows of one table: a record for each key.
fn rows_of<'a, T: serde::Serialize + 'a>(items: impl Iterator<Item = (String, &'a T)>) -> Rows {
    let mut rows = Rows::new();
    for (key, item) in items {
        rows.insert(
            vec![KeyVal::Text(key)],
            vec![Val::Json(serde_json::to_value(item).unwrap_or_default())],
        );
    }
    rows
}

/// The rows of `layout`, under the tables' names.
pub(crate) fn rows(layout: &Layout) -> Tables {
    let mut cluster = Rows::new();
    if !layout.name().is_empty() {
        cluster.insert(
            vec![KeyVal::Text("name".to_owned())],
            vec![Val::Json(serde_json::Value::String(
                layout.name().to_owned(),
            ))],
        );
    }
    Tables::from([
        (LAB_CLUSTER.name, cluster),
        (
            LAB_ROWS.name,
            rows_of(layout.rows().into_iter().map(|r| (r.id.clone(), r))),
        ),
        (
            LAB_RACKS.name,
            rows_of(layout.racks().into_iter().map(|r| (r.id.clone(), r))),
        ),
        (
            LAB_PLACEMENTS.name,
            rows_of(
                layout
                    .placements()
                    .into_iter()
                    .map(|p| (p.subject.key(), p)),
            ),
        ),
        (
            LAB_DEVICES.name,
            rows_of(layout.devices().into_iter().map(|d| (d.id.clone(), d))),
        ),
    ])
}

/// The layout `tables` hold; a table they lack reads as empty.
///
/// # Errors
/// [`LayoutError::Stored`]: a row does not read, or the layout breaks a rule of [`Layout::new`].
pub(crate) fn of_tables(tables: &Tables) -> Result<Layout, LayoutError> {
    let empty = Rows::new();
    let records = |table: &Table| {
        tables
            .get(table.name)
            .unwrap_or(&empty)
            .iter()
            .map(|(key, values)| {
                let id = key.first().and_then(KeyVal::as_text).unwrap_or_default();
                let record = values.first().and_then(json_of).unwrap_or_default();
                (id.to_owned(), record)
            })
            .collect::<Vec<_>>()
    };
    let name = records(&LAB_CLUSTER)
        .into_iter()
        .find(|(key, _)| key == "name")
        .and_then(|(_, record)| record.as_str().map(str::to_owned))
        .unwrap_or_default();
    Layout::of_records(
        &place("lab_layout"),
        name,
        records(&LAB_ROWS),
        records(&LAB_RACKS),
        records(&LAB_PLACEMENTS),
        records(&LAB_DEVICES),
    )
}
