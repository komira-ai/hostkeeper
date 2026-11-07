use super::*;

const PEOPLE: Table = Table {
    name: "people",
    keys: &[("team", KeyTy::Text), ("seat", KeyTy::Int)],
    values: &[("who", Ty::Text), ("age", Ty::Int)],
};

const TEAMS: Table = Table {
    name: "teams",
    keys: &[("team", KeyTy::Text)],
    values: &[],
};

fn team(name: &str) -> Vec<KeyVal> {
    vec![KeyVal::Text(name.to_owned())]
}

fn person(team: &str, seat: i64) -> Vec<KeyVal> {
    vec![KeyVal::Text(team.to_owned()), KeyVal::Int(seat)]
}

fn values(name: &str, age: i64) -> Vec<Val> {
    vec![Val::Text(name.to_owned()), Val::Int(age)]
}

fn rows(entries: &[(Vec<KeyVal>, Vec<Val>)]) -> Rows {
    entries.iter().cloned().collect()
}

fn sql(statements: &[Stmt]) -> Vec<&str> {
    statements.iter().map(|s| s.sql.as_str()).collect()
}

/// Proves: the statements a table makes are exactly the select of every column in key order, the
/// upsert of one row over any row of its key (naming each value column in the update), the
/// insert-or-ignore of a table with no value columns, and the delete of one key, with the key
/// and the values as parameters in column order.
///
/// Catches: a statement that names a column out of order, updates the key, or deletes by part of
/// the key.
#[test]
fn a_table_writes_one_row_by_its_whole_key() {
    assert_eq!(
        PEOPLE.select(),
        "SELECT team, seat, who, age FROM people ORDER BY team, seat"
    );
    let upsert = PEOPLE.upsert(&person("a", 2), &values("ann", 30));
    assert_eq!(
        upsert.sql,
        "INSERT INTO people (team, seat, who, age) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (team, seat) DO UPDATE SET who = EXCLUDED.who, age = EXCLUDED.age"
    );
    assert_eq!(
        upsert.params,
        vec![
            Val::Text("a".to_owned()),
            Val::Int(2),
            Val::Text("ann".to_owned()),
            Val::Int(30)
        ]
    );
    assert_eq!(upsert.refs().len(), 4);
    let ignore = TEAMS.upsert(&team("a"), &[]);
    assert_eq!(
        ignore.sql,
        "INSERT INTO teams (team) VALUES ($1) ON CONFLICT (team) DO NOTHING"
    );
    let delete = PEOPLE.delete(&person("a", 2));
    assert_eq!(
        delete.sql,
        "DELETE FROM people WHERE team = $1 AND seat = $2"
    );
    assert_eq!(delete.params, vec![Val::Text("a".to_owned()), Val::Int(2)]);
}

/// Proves: a commit's statements delete every gone row of the tables in the reverse of their
/// order first, and then write every new or changed row in their order; a row that did not change
/// writes nothing, and a part with no rows in `before` or `after` is read as empty.
///
/// Catches: a row deleted after the table that refers to it is still written, a unique value
/// taken before the row that held it is deleted, and a rewrite of a row that did not change.
#[test]
fn changes_delete_in_reverse_order_then_write_in_order() {
    let before = Tables::from([
        ("teams", rows(&[(team("a"), vec![]), (team("b"), vec![])])),
        (
            "people",
            rows(&[
                (person("a", 1), values("ann", 30)),
                (person("a", 2), values("bob", 40)),
                (person("b", 1), values("cy", 50)),
            ]),
        ),
    ]);
    let after = Tables::from([
        ("teams", rows(&[(team("a"), vec![]), (team("c"), vec![])])),
        (
            "people",
            rows(&[
                (person("a", 1), values("ann", 31)),
                (person("a", 2), values("bob", 40)),
            ]),
        ),
    ]);
    let tables = [TEAMS, PEOPLE];
    let statements = changes(&tables, &before, &after);
    assert_eq!(
        statements
            .iter()
            .map(|s| (
                s.sql
                    .split_whitespace()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" "),
                s.params.clone()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "DELETE FROM people".to_owned(),
                vec![Val::Text("b".to_owned()), Val::Int(1)]
            ),
            (
                "DELETE FROM teams".to_owned(),
                vec![Val::Text("b".to_owned())]
            ),
            (
                "INSERT INTO teams".to_owned(),
                vec![Val::Text("c".to_owned())]
            ),
            (
                "INSERT INTO people".to_owned(),
                vec![
                    Val::Text("a".to_owned()),
                    Val::Int(1),
                    Val::Text("ann".to_owned()),
                    Val::Int(31)
                ]
            ),
        ]
    );
    assert_eq!(sql(&changes(&tables, &after, &after)), Vec::<&str>::new());
    let from_nothing = changes(&tables, &Tables::new(), &after);
    assert_eq!(from_nothing.len(), 4);
    let to_nothing = changes(&tables, &after, &Tables::new());
    assert_eq!(
        sql(&to_nothing),
        vec![
            "DELETE FROM people WHERE team = $1 AND seat = $2",
            "DELETE FROM people WHERE team = $1 AND seat = $2",
            "DELETE FROM teams WHERE team = $1",
            "DELETE FROM teams WHERE team = $1",
        ]
    );
}

fn memory_of(entries: &[(&str, i64)]) -> Tables {
    Tables::from([(
        "people",
        rows(
            &entries
                .iter()
                .map(|(name, age)| (person("a", 1), values(name, *age)))
                .collect::<Vec<_>>(),
        ),
    )])
}

/// Proves: laying a change over memory gives a row the change made its new value, removes a row
/// the change deleted, and leaves a row alone that memory changed since the change's `before`,
/// whatever the change did to it; a table only memory has is kept.
///
/// Catches: a change that landed after its commit's outcome was lost overwriting what memory did
/// since, or being left out of memory.
#[test]
fn a_change_is_laid_over_memory_except_where_memory_changed_since() {
    let key = person("a", 1);
    let other = person("a", 2);
    let third = person("a", 3);
    let before = Tables::from([(
        "people",
        rows(&[
            (key.clone(), values("ann", 30)),
            (other.clone(), values("bob", 40)),
            (third.clone(), values("dee", 20)),
        ]),
    )]);
    let after = Tables::from([(
        "people",
        rows(&[
            (key.clone(), values("ann", 31)),
            (third.clone(), values("dee", 20)),
            (person("a", 4), values("eve", 22)),
        ]),
    )]);
    // Memory is what the commit started from, but for the row of `third`.
    let mut memory = before.clone();
    memory
        .get_mut("people")
        .expect("people")
        .insert(third.clone(), values("dee", 21));
    memory
        .entry("elsewhere")
        .or_default()
        .insert(team("z"), vec![]);
    let over = laid_over(&memory, &before, &after);
    let people = over.get("people").expect("people");
    assert_eq!(
        people.get(&key),
        Some(&values("ann", 31)),
        "a changed row is written"
    );
    assert_eq!(people.get(&other), None, "a deleted row is removed");
    assert_eq!(
        people.get(&third),
        Some(&values("dee", 21)),
        "a row memory changed since keeps its value"
    );
    assert_eq!(
        people.get(&person("a", 4)),
        Some(&values("eve", 22)),
        "a new row is added"
    );
    assert!(over.contains_key("elsewhere"));

    // Memory changed the deleted row: it stays; memory deleted the changed row: it stays gone.
    let mut moved = before.clone();
    let rows_moved = moved.get_mut("people").expect("people");
    rows_moved.insert(other.clone(), values("bob", 41));
    rows_moved.remove(&key);
    let over = laid_over(&moved, &before, &after);
    let people = over.get("people").expect("people");
    assert_eq!(people.get(&other), Some(&values("bob", 41)));
    assert_eq!(people.get(&key), None);

    // A table the change did not have in `before` or in `after` is read as empty.
    let none = laid_over(&Tables::new(), &Tables::new(), &after);
    assert_eq!(none.get("people").map(BTreeMap::len), Some(3));
    let gone = laid_over(
        &memory_of(&[("ann", 30)]),
        &memory_of(&[("ann", 30)]),
        &Tables::new(),
    );
    assert_eq!(gone.get("people").map(BTreeMap::len), Some(0));
}

/// Proves: the accessors of a value answer for their own type and `None` for any other, text and
/// null convert as the callers expect, and a key value becomes the same value as a parameter.
///
/// Catches: a value read as the wrong type, and a key written as another type than it was read.
#[test]
fn values_and_keys_answer_for_their_own_type_only() {
    assert_eq!(Val::text_or_null(Some("x")), Val::Text("x".to_owned()));
    assert_eq!(Val::text_or_null(None), Val::Null);
    assert_eq!(Val::Int(7).as_int(), Some(7));
    assert_eq!(Val::Null.as_int(), None);
    assert_eq!(KeyVal::Text("k".to_owned()).as_text(), Some("k"));
    assert_eq!(KeyVal::Int(1).as_text(), None);
    assert_eq!(
        Val::from(&KeyVal::Text("k".to_owned())),
        Val::Text("k".to_owned())
    );
    assert_eq!(Val::from(&KeyVal::Int(5)), Val::Int(5));
}

/// Proves: every kind of value encodes for the column type it is written to, and null encodes
/// as null.
///
/// Catches: a value kind whose encoding is missing or stored as null.
#[test]
fn every_value_encodes_for_the_column_type_it_is_written_to() {
    use postgres::types::Type;
    let encode = |val: &Val, ty: &Type| -> Result<IsNull, String> {
        let mut out = BytesMut::new();
        val.to_sql(ty, &mut out).map_err(|e| e.to_string())
    };
    assert!(matches!(encode(&Val::Null, &Type::TEXT), Ok(IsNull::Yes)));
    for (val, ty) in [
        (Val::Text("t".to_owned()), Type::TEXT),
        (Val::Int(1), Type::INT8),
        (Val::Small(1), Type::INT2),
        (Val::Bool(true), Type::BOOL),
        (Val::Json(serde_json::json!([1])), Type::JSONB),
        (Val::Bytes(vec![1]), Type::BYTEA),
        (Val::Texts(vec!["a".to_owned()]), Type::TEXT_ARRAY),
    ] {
        assert!(matches!(encode(&val, &ty), Ok(IsNull::No)), "{val:?}");
    }
    assert!(Val::accepts(&Type::TEXT));
    let mut out = BytesMut::new();
    let checked = ToSql::to_sql_checked(&Val::Bool(true), &Type::BOOL, &mut out);
    assert!(matches!(checked, Ok(IsNull::No)));
}
