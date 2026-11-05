use super::{Table, Value, parse};

fn text(table: &Table, key: &str) -> String {
    match table.get(key) {
        Some(Value::Text(text)) => text.clone(),
        other => panic!("{key} is {other:?}"),
    }
}

fn refused(text: &str) -> String {
    parse(text).expect_err("the text is refused")
}

/// Catches a scalar of any kind being misread, a comment or blank line being taken for a key,
/// and quoted keys (the reserved `updater.*` ones) not being read.
#[test]
fn scalars_comments_and_quoted_keys_are_read() {
    let table = parse(
        "# written by the daemon\n\
         \n\
         units = \"systemd\"   # trailing comment\n\
         port=9310\n\
         flag = true\n\
         off = false\n\
         big = 1_000\n\
         neg = -5\n\
         pos = +7\n\
         ratio = 1.5\n\
         \t\"updater.target_version\" = \"1.2.3\"\r\n\
         'literal.key' = 'it'\n\
         bare-key_1 = \"x\"\n\
         \"\" = \"empty key\"\n",
    )
    .expect("parses");
    assert_eq!(text(&table, "units"), "systemd");
    assert_eq!(table.get("port"), Some(&Value::Integer(9310)));
    assert_eq!(table.get("flag"), Some(&Value::Bool(true)));
    assert_eq!(table.get("off"), Some(&Value::Bool(false)));
    assert_eq!(table.get("big"), Some(&Value::Integer(1000)));
    assert_eq!(table.get("neg"), Some(&Value::Integer(-5)));
    assert_eq!(table.get("pos"), Some(&Value::Integer(7)));
    assert_eq!(table.get("ratio"), Some(&Value::Other));
    assert_eq!(text(&table, "updater.target_version"), "1.2.3");
    assert_eq!(text(&table, "bare-key_1"), "x");
    assert_eq!(text(&table, ""), "empty key");
    assert_eq!(table.get("missing"), None);
}

/// Catches an empty file being refused: the daemon writes one for an empty table.
#[test]
fn an_empty_file_is_an_empty_table() {
    assert_eq!(parse(""), Ok(Table::default()));
    assert_eq!(parse("\n# only a comment"), Ok(Table::default()));
}

/// Catches a string's escapes being read wrongly.
#[test]
fn basic_string_escapes_are_decoded() {
    let table = parse(
        "a = \"q\\\"b\\\\s\\tn\\nr\\rf\\fb\\be\\e\"\n\
         u = \"\\u00e9\\U0001F600\\x41\"\n\
         e = \"\"\n",
    )
    .expect("parses");
    assert_eq!(text(&table, "a"), "q\"b\\s\tn\nr\rf\u{c}b\u{8}e\u{1b}");
    assert_eq!(text(&table, "u"), "\u{e9}\u{1F600}A");
    assert_eq!(text(&table, "e"), "");
}

/// Catches literal strings having their backslashes taken for escapes.
#[test]
fn literal_strings_keep_their_backslashes() {
    let table = parse("a = 'back\\slash \"q\"'\nm = '''\nl1\\n\nl2'''\n").expect("parses");
    assert_eq!(text(&table, "a"), "back\\slash \"q\"");
    assert_eq!(text(&table, "m"), "l1\\n\nl2");
}

/// Catches multi-line strings (what the contract crate writes for a value with a newline) being
/// read with the newline after the opening quotes, or with quotes lost before the closing ones.
#[test]
fn multi_line_strings_are_read_as_the_contract_crate_writes_them() {
    let table = parse(
        "a = \"\"\"\nline1\nline2\"\"\"\n\
         b = \"\"\"q\"uo'te\"\"\"\n\
         c = \"\"\"\r\ncrlf\"\"\"\n\
         d = \"\"\"ends with two quotes\"\"\"\"\"\n\
         e = \"\"\"a\\\"\"\"b\\u00e9\"\"\"\n\
         f = '''x''y\n'''\n\
         g = \"\"\"\"\"\"\n",
    )
    .expect("parses");
    assert_eq!(text(&table, "a"), "line1\nline2");
    assert_eq!(text(&table, "b"), "q\"uo'te");
    assert_eq!(text(&table, "c"), "crlf");
    assert_eq!(text(&table, "d"), "ends with two quotes\"\"");
    assert_eq!(text(&table, "e"), "a\"\"\"b\u{e9}");
    assert_eq!(text(&table, "f"), "x''y\n");
    assert_eq!(text(&table, "g"), "");
}

/// Catches a backslash at the end of a line in a multi-line string not joining the lines.
#[test]
fn a_line_ending_backslash_joins_the_lines() {
    let table = parse("a = \"\"\"one \\\n    two \\  \n\n  three\"\"\"\n").expect("parses");
    assert_eq!(text(&table, "a"), "one two three");
}

/// Catches text the reader would take for a value but that is not one.
#[test]
fn malformed_text_is_refused_with_its_line() {
    let cases = [
        ("port = \n", "line 1: \"\" is not a value"),
        ("port = nope\n", "line 1: \"nope\" is not a value"),
        ("\n\nport 9310\n", "line 3: expected `=` after the key \"port\""),
        ("= 1\n", "line 1: expected a key"),
        ("[table]\nx = 1\n", "line 1: expected a key"),
        ("a.b = 1\n", "line 1: expected `=` after the key \"a\""),
        ("a = 1 2\n", "line 1: unexpected text after the value"),
        ("a = \"x\" y\n", "line 1: unexpected text after the value"),
        ("a = \"open\n", "line 1: a string is not closed on its line"),
        ("a = \"open", "line 1: a string is not closed on its line"),
        ("a = 'open\n", "line 1: a string is not closed on its line"),
        ("a = \"\"\"open\n", "line 1: a multi-line string is not closed"),
        ("a = '''open", "line 1: a multi-line string is not closed"),
        ("a = \"\\q\"\n", "line 1: a string has an escape that TOML does not have"),
        ("a = \"\\", "line 1: a string has an escape that TOML does not have"),
        ("a = \"\\u12\"\n", "line 1: a unicode escape has too few hexadecimal digits"),
        ("a = \"\\u12zz\"\n", "line 1: a unicode escape has too few hexadecimal digits"),
        ("a = \"\\uD800\"\n", "line 1: U+D800 is not a character"),
        ("a = \"\\UFFFFFFFFF\"\n", "line 1: U+FFFFFFFF is not a character"),
        ("a = \"\"\"x\"\"\"\"\"\"\n", "line 1: too many quotes at the end of a string"),
        ("a = 1\na = 2\n", "line 2: key \"a\" is given twice"),
        ("\"a\" = 1\na = 2\n", "line 2: key \"a\" is given twice"),
    ];
    for (input, why) in cases {
        assert_eq!(refused(input), why, "{input:?}");
    }
}

/// Catches an integer too large for 64 bits being read as a smaller one.
#[test]
fn an_integer_over_64_bits_is_not_an_integer() {
    let table = parse("n = 99999999999999999999\n").expect("a float to the reader");
    assert_eq!(table.get("n"), Some(&Value::Other));
}
