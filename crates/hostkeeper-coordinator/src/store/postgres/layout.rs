//! The lab's layout ([`crate::layout`]) as the rows of the tables that migrations `0015`, `0016`
//! and `0017` add: `lab_cluster`, `lab_rows` and `lab_racks` (`0015`), `lab_devices` (`0016`),
//! `lab_sleds` and `lab_sled_items` (`0017`): a row for the name and one per row, rack, device,
//! sled and subject on a sled, keyed by id (a subject by its key), holding the record as the
//! layout file holds it, without the key.
//!
//! `lab_placements` (`0015`), where the release before kept what stands in a rack, is not read:
//! migration `0017` turned its rows into sleds, once, and left it as it was for a rollback.
//!
//! A database that holds no row holds an empty layout. An older release never reads the tables
//! of `0016` and `0017` (both are `may-run`). A load reads every row through the file's reader,
//! so it is as strict as a start on the files, and refuses a layout that breaks a rule of
//! [`Layout::new`], naming the tables.

use super::rows::{KeyTy, KeyVal, Rows, Table, Tables, Ty, Val};
use super::state::{json_of, place};
use crate::layout::{Layout, LayoutError, Records};

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

/// The table of the sleds (migration `0017`).
pub(crate) const LAB_SLEDS: Table = Table {
    name: "lab_sleds",
    keys: &[("id", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The table of the subjects on sleds, by subject key (migration `0017`).
pub(crate) const LAB_SLED_ITEMS: Table = Table {
    name: "lab_sled_items",
    keys: &[("subject", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The layout's tables.
pub(crate) const TABLES: [Table; 6] = [
    LAB_CLUSTER,
    LAB_ROWS,
    LAB_RACKS,
    LAB_DEVICES,
    LAB_SLEDS,
    LAB_SLED_ITEMS,
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
            LAB_DEVICES.name,
            rows_of(layout.devices().into_iter().map(|d| (d.id.clone(), d))),
        ),
        (
            LAB_SLEDS.name,
            rows_of(layout.sleds().into_iter().map(|s| (s.id.clone(), s))),
        ),
        (
            LAB_SLED_ITEMS.name,
            rows_of(
                layout
                    .sled_items()
                    .into_iter()
                    .map(|i| (i.subject.key(), i)),
            ),
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
        Vec::new(),
        Records {
            devices: records(&LAB_DEVICES),
            sleds: records(&LAB_SLEDS),
            items: records(&LAB_SLED_ITEMS),
        },
    )
}
