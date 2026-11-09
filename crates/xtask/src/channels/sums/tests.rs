use super::*;
use crate::testdir::TestDir;

fn scratch(name: &str) -> TestDir {
    let beside = std::env::current_exe().expect("test binary");
    TestDir::under(
        &beside.parent().expect("deps").join("xtask-channels-unit"),
        name,
    )
}

fn hex(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

#[test]
fn parse_reads_lines_and_render_writes_them_back() {
    let text = format!("{}  a.deb\n{}  b.rpm\n", hex(1), hex(2));
    let lines = parse(&text).expect("lines");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].name, "a.deb");
    assert_eq!(lines[1].sha256.to_string(), hex(2));
    assert_eq!(render(&lines), text);
}

#[test]
fn parse_names_every_line_that_is_off_the_form() {
    let text = format!(
        "{}  a.deb\n{} b.rpm\n{}  dir/c.deb\n{}  \nnot hex  d\n{}  e",
        hex(1),
        hex(2),
        hex(3),
        hex(4),
        hex(5)
    );
    let problems = parse(&text).expect_err("problems");
    assert_eq!(problems.len(), 5);
    assert!(problems[0].starts_with("line 2: "), "{problems:?}");
    assert!(problems[1].starts_with("line 3: "), "{problems:?}");
    assert!(problems[2].starts_with("line 4: "), "{problems:?}");
    assert!(problems[3].starts_with("line 5: "), "{problems:?}");
    assert!(
        problems[4].contains("and a newline"),
        "a last line without its newline: {problems:?}"
    );
    // Upper case hex is not a digest as `sha256sum` writes it.
    assert!(parse(&format!("{}  a\n", hex(10).to_uppercase())).is_err());
}

#[test]
fn read_sums_reads_a_file_and_refuses_a_missing_malformed_or_empty_one() {
    let dir = scratch("read-sums");
    let ok = dir.join("ok");
    std::fs::write(&ok, format!("{}  a.deb\n", hex(7))).expect("write");
    assert_eq!(read_sums(&ok).expect("lines")[0].name, "a.deb");

    let missing = read_sums(&dir.join("missing")).unwrap_err();
    assert!(missing.to_string().starts_with("read "), "{missing}");

    let bad = dir.join("bad");
    std::fs::write(&bad, "garbage\n").expect("write");
    let refused = read_sums(&bad).unwrap_err();
    assert!(
        matches!(&refused, Error::Refused(p) if p.len() == 1),
        "{refused}"
    );
    assert!(refused.to_string().starts_with(&bad.display().to_string()));

    let empty = dir.join("empty");
    std::fs::write(&empty, "").expect("write");
    assert_eq!(
        read_sums(&empty).unwrap_err().to_string(),
        format!("{} lists no file", empty.display())
    );
}
