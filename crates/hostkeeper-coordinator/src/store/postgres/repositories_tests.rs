//! The registry as rows and back, without a database: the two tables' rows of a registry, and the
//! refusals of a load of rows a database could hold after a manual edit or a later build.

use super::{GENERATION, TABLE, of, rows};
use crate::repositories::{Kept, Registry};
use crate::repositories_support::komira;
use crate::store::postgres::rows::{KeyVal, Rows, Val};

fn message(why: crate::desired::StoreError) -> String {
    why.to_string()
}

/// Proves: a registry is one row per repository (keyed by its name, holding its record) and one
/// generation row at key 1, none for the registry at generation 0; the rows read back as the
/// registry, and a registry whose generation no `bigint` holds is refused as a store failure.
///
/// Catches: a generation of 0 stored as a row, which a load would then have to tell from a
/// registry that was never put.
#[test]
fn a_registry_is_its_rows_and_the_rows_read_back_as_it() {
    assert_eq!(
        (TABLE.name, GENERATION.name),
        ("repositories", "repository_generation")
    );
    let (none, zero) = rows(&Registry::default()).expect("rows");
    assert!(none.is_empty() && zero.is_empty());

    let held = Registry::default().put(komira(1)).expect("put");
    let (repositories, generation) = rows(&held).expect("rows");
    assert_eq!(
        repositories.get(&vec![KeyVal::Text("komira".to_owned())]),
        Some(&vec![Val::Json(
            held.get("komira").expect("kept").to_json()
        )])
    );
    assert_eq!(
        generation.get(&vec![KeyVal::Int(1)]),
        Some(&vec![Val::Int(1)])
    );
    assert_eq!(of(&repositories, &generation).expect("read"), held);

    let huge = Registry {
        generation: u64::MAX,
        ..Registry::default()
    };
    let refused = rows(&huge).expect_err("refused");
    assert!(message(refused).contains("above the largest counter"));
}

/// Proves: a load refuses a generation that is negative or not a number, a record that does not
/// read (naming its repository), and a registry that breaks a rule of the registry, each with the
/// words that say which.
///
/// Catches: a registry served from rows it cannot hold.
#[test]
fn rows_that_cannot_be_a_registry_are_refused_by_reason() {
    let held = Registry::default().put(komira(1)).expect("put");
    let (repositories, _) = rows(&held).expect("rows");
    let generation = |value: Val| Rows::from([(vec![KeyVal::Int(1)], vec![value])]);
    let prefix = "database:repositories: the desired state is malformed: ";
    for (bad, why) in [
        (Val::Int(-1), "a negative generation"),
        (
            Val::Text("1".to_owned()),
            "a generation that is not a number",
        ),
    ] {
        let refused = of(&repositories, &generation(bad)).expect_err("refused");
        assert_eq!(message(refused), format!("{prefix}{why}"));
    }
    let bad_record = Rows::from([(
        vec![KeyVal::Text("komira".to_owned())],
        vec![Val::Json(serde_json::json!({"generation": 1}))],
    )]);
    let refused = of(&bad_record, &generation(Val::Int(1))).expect_err("refused");
    assert_eq!(
        message(refused),
        format!("{prefix}the row komira: no repository")
    );

    let ahead = Rows::from([(
        vec![KeyVal::Text("komira".to_owned())],
        vec![Val::Json(
            Kept {
                repository: komira(1),
                generation: 3,
            }
            .to_json(),
        )],
    )]);
    let refused = of(&ahead, &generation(Val::Int(2))).expect_err("refused");
    assert_eq!(
        message(refused),
        format!("{prefix}komira is at generation 3, past the registry's 2")
    );
    let empty = of(&Rows::new(), &Rows::new()).expect("an empty registry");
    assert_eq!(empty, Registry::default());
}
