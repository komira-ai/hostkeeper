//! The params reader against the writer it reads: whatever `Params::to_toml` of the hook contract
//! crate writes, the program's own reader reads back to the same values.

use hello::params::{Value, parse};
use hostkeeper_hook::{Params, Scalar};

fn text(table: &hello::params::Table, key: &str) -> String {
    table
        .get(key)
        .and_then(Value::as_text)
        .unwrap_or("<missing, or not text>")
        .to_owned()
}

fn tricky() -> Vec<String> {
    let mut found: Vec<String> = [
        "",
        " ",
        "plain",
        "with \"double\" quotes",
        "with 'single' quotes",
        "\"",
        "'",
        "ends with a quote\"",
        "ends with two\"\"",
        "three \"\"\" in the middle",
        "''' and \"\"\"",
        "back\\slash",
        "\\",
        "\\n is not a newline",
        "windows\\path\\\"quoted\"",
        "line1\nline2",
        "\nleading newline",
        "trailing newline\n",
        "\n",
        "\r\n",
        "tab\there",
        "bell\u{7}",
        "escape\u{1b}[0m",
        "nul-free control \u{1}\u{1f}\u{7f}",
        "caf\u{e9}",
        "\u{1F600} and \u{10FFFF}",
        "\u{2028} \u{85} \u{a0}",
        "# not a comment",
        "key = \"value\"",
        "a = 1\nb = 2",
        "[table]",
        "/var/lib/updater/hooks/hello",
        "sha256:abababababababababababababababababababababababababababababababab",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    // Every character of ASCII alone, between letters, and at the ends of the string.
    for code in 0u32..0x80 {
        let c = char::from_u32(code).expect("ASCII");
        found.push(format!("a{c}b"));
        found.push(format!("{c}{c}"));
        found.push(format!("x{c}"));
        found.push(format!("{c}x"));
    }
    found
}

/// Catches a string the writer can write, in whatever form it writes it (basic, literal, multi
/// line), being read back as another string, and a key the writer quotes being lost.
#[test]
fn every_string_the_contract_crate_writes_is_read_back() {
    for (index, value) in tricky().into_iter().enumerate() {
        let mut params = Params::default();
        params.insert("record_dir", Scalar::Text(value.clone()));
        params.insert("updater.target_version", Scalar::Text(value.clone()));
        params.insert("updater.previous_version", Scalar::Text("1.0.0".to_owned()));
        params.insert("after", Scalar::Text("next".to_owned()));
        let written = params.to_toml();
        let table =
            parse(&written).unwrap_or_else(|why| panic!("#{index} {value:?}: {why}\n{written}"));
        assert_eq!(text(&table, "record_dir"), value, "#{index}\n{written}");
        assert_eq!(
            text(&table, "updater.target_version"),
            value,
            "#{index}\n{written}"
        );
        assert_eq!(text(&table, "updater.previous_version"), "1.0.0");
        assert_eq!(
            text(&table, "after"),
            "next",
            "a key after the value is not swallowed: #{index}"
        );
    }
}

/// Catches an integer, a boolean or a float the writer writes being read as another kind or
/// value, including the ends of 64 bits.
#[test]
fn integers_booleans_and_floats_are_read_back() {
    let mut params = Params::default();
    for (key, value) in [
        ("zero", 0i64),
        ("one", 1),
        ("port", 9310),
        ("neg", -42),
        ("max", i64::MAX),
        ("min", i64::MIN),
    ] {
        params.insert(key, Scalar::Integer(value));
    }
    params.insert("yes", Scalar::Bool(true));
    params.insert("no", Scalar::Bool(false));
    for (key, value) in [
        ("half", 1.5),
        ("big", 1e300),
        ("tiny", -2.5e-9),
        ("whole", 3.0),
        ("inf", f64::INFINITY),
        ("ninf", f64::NEG_INFINITY),
        ("nan", f64::NAN),
    ] {
        params.insert(key, Scalar::Float(value));
    }
    let written = params.to_toml();
    let table = parse(&written).unwrap_or_else(|why| panic!("{why}\n{written}"));
    for (key, value) in [
        ("zero", 0),
        ("one", 1),
        ("port", 9310),
        ("neg", -42),
        ("max", i64::MAX),
        ("min", i64::MIN),
    ] {
        assert_eq!(
            table.get(key),
            Some(&Value::Integer(value)),
            "{key}\n{written}"
        );
    }
    assert_eq!(table.get("yes"), Some(&Value::Bool(true)));
    assert_eq!(table.get("no"), Some(&Value::Bool(false)));
    for key in ["half", "big", "tiny", "whole", "inf", "ninf", "nan"] {
        assert_eq!(table.get(key), Some(&Value::Other), "{key}\n{written}");
    }
}

/// Catches an empty table, and the file the daemon writes for a hook (the four reserved keys the
/// hook never reads beside its own), being refused.
#[test]
fn the_files_the_daemon_writes_are_read() {
    assert_eq!(
        parse(&Params::default().to_toml()),
        Ok(hello::params::Table::default())
    );

    let mut params = Params::default();
    params.insert("units", Scalar::Text("systemd".to_owned()));
    params.insert("port", Scalar::Integer(9310));
    params.insert(
        "record_dir",
        Scalar::Text("/var/lib/updater/hooks/hello".to_owned()),
    );
    params.insert(
        "updater.staged_release",
        Scalar::Text("/var/lib/hostkeeper/packages/hello/staged".to_owned()),
    );
    params.insert(
        "updater.previous_release",
        Scalar::Text("/var/lib/hostkeeper/packages/hello/previous".to_owned()),
    );
    params.insert("updater.target_version", Scalar::Text("1.2.3".to_owned()));
    params.insert("updater.previous_version", Scalar::Text("1.0.0".to_owned()));
    params.insert(
        "updater.artifact_digest",
        Scalar::Text(format!("sha256:{}", "ab".repeat(32))),
    );
    let params = params.with_promotion_ceiling_secs(3600);
    let table = parse(&params.to_toml()).expect("the daemon's file");
    assert_eq!(table.get("port"), Some(&Value::Integer(9310)));
    assert_eq!(
        table.get("updater.promotion_ceiling_secs"),
        Some(&Value::Integer(3600))
    );
    assert_eq!(text(&table, "updater.target_version"), "1.2.3");
}
