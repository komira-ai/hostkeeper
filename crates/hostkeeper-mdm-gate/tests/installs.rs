//! The named installs (docs/design/coordinator-mdm.md section 4.7): every rule
//! `Installs::parse` checks, the text the setting keeps and reads back, what changes an entry's
//! digest, and the one-time seed from the old pkg allowlist.

use hostkeeper_mdm_gate::{HOSTKEEPER_INSTALL, Installs, PkgAllowlist};

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn at(sha256: &str) -> String {
    format!("/var/lib/hostkeeper-mdm-pkg/{sha256}.pkg")
}

/// One install as the JSON the setting holds.
fn entry(name: &str, sha256: &str, version: &str, extra: &str) -> String {
    format!(
        r#"{{"name":"{name}","bundle_id":"com.example.{name}","sha256":"{sha256}","version":"{version}","path":"{}"{extra}}}"#,
        at(sha256)
    )
}

fn refusal(text: &str) -> String {
    Installs::parse(text).expect_err("refused").to_string()
}

/// Catches: an installs text accepted with a broken entry: a name that is not 1 to 32
/// lower-case letters, digits and `-`, a bundle identifier of other characters, a malformed
/// digest or version, a path that is relative or not named for its digest (the URL would name
/// a digest the file is not), an origin that is not `https://`, a name or a digest listed twice,
/// or text that is not an array; each refused with a text naming the rule.
#[test]
fn the_installs_refuse_each_broken_rule() {
    let wrap = |entry: &str| format!("[{entry}]");
    let cases = [
        ("{}".to_owned(), "installs: not a JSON array of installs"),
        (
            "[{\"name\":\"x\"}]".to_owned(),
            "installs: not a JSON array of installs",
        ),
        (
            wrap(&entry("Tools", A, "1.0", "")),
            "installs: the name \"Tools\" is not 1 to 32 lower-case letters, digits and '-'",
        ),
        (
            wrap(&entry("-tools", A, "1.0", "")),
            "installs: the name \"-tools\" is not 1 to 32 lower-case letters, digits and '-'",
        ),
        (
            wrap(&entry(&"t".repeat(33), A, "1.0", "")),
            &format!(
                "installs: the name {:?} is not 1 to 32 lower-case letters, digits and '-'",
                "t".repeat(33)
            ),
        ),
        (
            wrap(&entry("tools", A, "1.0", "").replace("com.example.tools", "a b")),
            "installs: tools: the bundle_id is not 1 to 255 letters, digits, '.', '-' and '_'",
        ),
        (
            wrap(&entry("tools", &A.to_uppercase(), "1.0", "")),
            "installs: tools: sha256 is not 64 lower-case hex digits",
        ),
        (
            wrap(&entry("tools", A, "", "")),
            "installs: tools: version \"\": the version is empty",
        ),
        (
            wrap(&entry("tools", A, "1.0", "").replace(&at(A), &format!("{A}.pkg"))),
            &format!("installs: tools: the path is not an absolute path to {A}.pkg"),
        ),
        (
            wrap(&entry("tools", A, "1.0", "").replace(&at(A), &at(B))),
            &format!("installs: tools: the path is not an absolute path to {A}.pkg"),
        ),
        (
            wrap(&entry(
                "tools",
                A,
                "1.0",
                r#","url":"http://example.com/a.pkg""#,
            )),
            "installs: tools: the url is not an https:// URL",
        ),
        (
            wrap(&entry("tools", A, "1.0", r#","url":"https://""#)),
            "installs: tools: the url is not an https:// URL",
        ),
        (
            format!(
                "[{},{}]",
                entry("tools", A, "1.0", ""),
                entry("tools", B, "1.0", "")
            ),
            "installs: the name tools is listed twice",
        ),
        (
            format!(
                "[{},{}]",
                entry("tools", A, "1.0", ""),
                entry("other", A, "1.0", "")
            ),
            &format!("installs: other: sha256 {A} is listed twice"),
        ),
    ];
    for (text, why) in cases {
        assert_eq!(refusal(&text), why, "{text}");
    }
}

/// Catches: the setting's text not read back as it was written (a field lost, the order
/// changed, an optional `url` kept when absent or dropped when present), a key a later release
/// adds refused (B8), and an empty list refused: the default is no install.
#[test]
fn the_installs_round_trip_and_pass_over_a_key_they_do_not_know() {
    let text = format!(
        "[{},{}]",
        entry("hostkeeper", A, "0.1.66", ""),
        entry(
            "tools",
            B,
            "2.1",
            r#","url":"https://example.com/tools.pkg","signed_by":"a later release""#
        ),
    );
    let installs = Installs::parse(&text).expect("parses");
    let names: Vec<&str> = installs
        .entries()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["hostkeeper", "tools"]);
    assert_eq!(
        installs.get("tools").and_then(|entry| entry.url.as_deref()),
        Some("https://example.com/tools.pkg")
    );
    assert_eq!(
        installs.hostkeeper().map(|entry| entry.sha256.as_str()),
        Some(A)
    );
    assert_eq!(
        installs
            .baseline()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["tools"],
        "the baseline sends every install but the hostkeeper pkg"
    );
    assert!(installs.get("nothing").is_none());

    let written = installs.to_json();
    assert_eq!(
        written,
        format!(
            "[{},{}]",
            entry("hostkeeper", A, "0.1.66", ""),
            entry(
                "tools",
                B,
                "2.1",
                r#","url":"https://example.com/tools.pkg""#
            )
        ),
        "an unknown key is not kept, and an absent url is not written"
    );
    assert_eq!(
        Installs::parse(&written).expect("reads back").to_json(),
        written
    );
    // Digits and `-` in a name, and `-`, `_` and digits in a bundle identifier, are taken.
    let odd = entry("tools-2", B, "2.1", "").replace("com.example.tools-2", "com.ex-ample.tool_9");
    assert!(Installs::parse(&format!("[{odd}]")).is_ok(), "{odd}");
    assert_eq!(Installs::parse("[]").expect("empty"), Installs::empty());
    assert_eq!(Installs::empty().to_json(), "[]");
    assert!(Installs::empty().hostkeeper().is_none());
}

/// Catches: a digest that does not change with what a Mac is sent (the file's digest, the
/// version, the bundle identifier, the name), or that changes with where the file is found
/// (path, origin url), which would send a Mac the same package again.
#[test]
fn an_entry_digest_changes_with_what_is_installed_and_not_with_where_it_is_found() {
    let digest = |text: &str| {
        Installs::parse(&format!("[{text}]"))
            .expect("parses")
            .entries()[0]
            .digest()
    };
    let base = digest(&entry("tools", A, "1.0", ""));
    assert_eq!(base.len(), 64);
    assert_eq!(
        base,
        digest(&entry(
            "tools",
            A,
            "1.0",
            r#","url":"https://example.com/a.pkg""#
        )),
        "an origin is where the file is pulled from"
    );
    assert_ne!(base, digest(&entry("tools", B, "1.0", "")), "the file");
    assert_ne!(base, digest(&entry("tools", A, "1.1", "")), "the version");
    assert_ne!(
        base,
        digest(&entry("tools", A, "1.0", "").replace("com.example.tools", "com.example.x")),
        "the bundle identifier"
    );
    assert_ne!(base, digest(&entry("tool", A, "1.0", "")), "the name");
}

/// Catches: the old allowlist seeding something other than its newest pkg as the install named
/// `hostkeeper` with the hostkeeper bundle identifier and no origin.
#[test]
fn the_old_allowlist_seeds_its_newest_pkg_as_the_hostkeeper_install() {
    let list = format!(
        "[[pkg]]\nsha256 = \"{A}\"\nversion = \"0.1.9\"\npath = \"{}\"\n[[pkg]]\nsha256 = \"{B}\"\nversion = \"0.1.66\"\npath = \"{}\"\n",
        at(A),
        at(B)
    );
    let seeded = PkgAllowlist::parse(&list).expect("parses").installs();
    assert_eq!(seeded.entries().len(), 1);
    let entry = seeded.get(HOSTKEEPER_INSTALL).expect("hostkeeper");
    assert_eq!(entry.sha256.as_str(), B);
    assert_eq!(entry.version.as_str(), "0.1.66");
    assert_eq!(entry.bundle_id, "dev.hostkeeper.hostkeeper");
    assert_eq!(entry.url, None);
    assert_eq!(
        seeded.to_json(),
        format!(
            r#"[{{"name":"hostkeeper","bundle_id":"dev.hostkeeper.hostkeeper","sha256":"{B}","version":"0.1.66","path":"{}"}}]"#,
            at(B)
        )
    );
}
