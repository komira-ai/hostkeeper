//! The hygiene lint: every tracked text file of this (public) repository is held to the rules in
//! [`rules`], and each rule is shown to refuse its defect by running it on a text that holds one.
//!
//! The files come from `git ls-files` at the repository root. A file whose bytes are not UTF-8 is
//! binary and is skipped; every other tracked file is read. A tracked link is read as git keeps
//! it, as the path it points at, and is not followed: a link may point at a path that exists only
//! on a host (the overlay's console issue link points into `/run`). The deny list of internal repository
//! names ([`DENIED_FILE`]) is not scanned, because its job is to hold the digests of those
//! names. The licence text ([`LICENCE_FILE`]) and the Sigstore trusted root ([`TRUSTED_ROOT_FILE`])
//! are scanned by every rule but the date rule, because each is verbatim third-party text with
//! its authors' dates; so are the pin lines of the pinned Debian packages (`.github/debs/*.toml`),
//! each a package's version and path as Canonical's signed index spells them. Test sources that need a text a
//! rule refuses build it from pieces at run time, so that this directory passes its own lint.

mod links;
mod retired;
mod rules;
mod rust_text;
#[path = "../../src/testdir.rs"]
mod testdir;
mod tlds;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use rules::{
    DEBS_DIR, EXACT, Finding, LICENCE_FILE, Lists, PIN_LINE, TRUSTED_ROOT_FILE, dates, emails,
    entries, home_paths, hostnames, ipv4s, lint, repositories,
};
use sha2::{Digest, Sha256};
use tlds::{FILE_SUFFIXES, IANA_TEXT, PRIVATE_TLDS, is_tld, parse_tlds};

/// The deny list, as a path from the repository root: a tracked file that is not scanned.
const DENIED_FILE: &str = "crates/xtask/tests/hygiene/denied_repositories.sha256";
const DENIED_TEXT: &str = include_str!("denied_repositories.sha256");
const PUBLIC_TEXT: &str = include_str!("public_domains.txt");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The tracked paths, from `git ls-files -z` at the repository root. A git failure panics.
fn tracked_files(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .expect("run git ls-files");
    assert!(
        out.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut paths = Vec::new();
    for raw in out.stdout.split(|&b| b == 0) {
        if !raw.is_empty() {
            paths.push(String::from_utf8(raw.to_vec()).expect("a tracked path is UTF-8"));
        }
    }
    paths
}

/// The paths the lint reads: all but the deny list. The licence text is read, by every rule but
/// the date rule.
fn scanned(paths: Vec<String>) -> Vec<String> {
    paths.into_iter().filter(|p| p != DENIED_FILE).collect()
}

/// `(path, text)` of every scanned file that is text. A directory (a submodule) and a file that
/// is not UTF-8 are skipped; a link's text is the path it points at, as git keeps it, whether
/// or not that path exists; a file that cannot be read panics.
fn texts(root: &Path, paths: Vec<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for path in paths {
        let full = root.join(&path);
        let meta = std::fs::symlink_metadata(&full).unwrap_or_else(|e| panic!("read {path}: {e}"));
        if meta.is_dir() {
            continue;
        }
        let bytes = if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&full).unwrap_or_else(|e| panic!("read {path}: {e}"));
            target.into_os_string().into_encoded_bytes()
        } else {
            std::fs::read(&full).unwrap_or_else(|e| panic!("read {path}: {e}"))
        };
        if let Ok(text) = String::from_utf8(bytes) {
            out.push((path, text));
        }
    }
    out
}

fn real_lists() -> Lists {
    Lists {
        public: entries(PUBLIC_TEXT),
        denied: entries(DENIED_TEXT).into_iter().collect(),
    }
}

fn reasons(found: Vec<Finding>) -> Vec<String> {
    found.iter().map(ToString::to_string).collect()
}

fn on(spelling: &str) -> String {
    format!("released on {spelling}.")
}

fn date_reason(spelling: &str) -> String {
    format!("notes.md:1: the date `{spelling}` is before 2026-09-01")
}

fn host_reason(host: &str) -> String {
    format!(
        "the host `{host}` is not an example domain and not on tests/hygiene/public_domains.txt"
    )
}

/// Asserts that a rule found nothing in `text`.
fn assert_clean(found: Vec<Finding>, text: &str) {
    assert!(found.is_empty(), "in {text:?}: {:?}", reasons(found));
}

fn public() -> Vec<String> {
    vec!["github.com".to_string()]
}

/// Catches a lint that stops at a tracked link whose target is not there, follows a link instead
/// of reading the path it holds, or reads a directory: a file is read as its text, a link that
/// points at nothing and a link to a file as the paths they hold, and a directory not at all.
#[test]
fn a_link_is_read_as_the_path_it_points_at_and_never_followed() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let dir = testdir::TestDir::under(base, "hygiene-links");
    std::fs::write(dir.join("notes.md"), "a note\n").expect("a file");
    std::os::unix::fs::symlink("/run/nowhere/enrollment.issue", dir.join("dangling"))
        .expect("a dangling link");
    std::os::unix::fs::symlink("notes.md", dir.join("to-notes")).expect("a link to the file");
    std::fs::create_dir(dir.join("submodule")).expect("a directory");
    let paths = ["notes.md", "dangling", "to-notes", "submodule"].map(str::to_owned);
    assert_eq!(
        texts(&dir, paths.to_vec()),
        [
            ("notes.md".to_owned(), "a note\n".to_owned()),
            (
                "dangling".to_owned(),
                "/run/nowhere/enrollment.issue".to_owned()
            ),
            ("to-notes".to_owned(), "notes.md".to_owned()),
        ]
    );
}

#[test]
fn hygiene_lint_passes_the_tree() {
    // Catches: a date before the cutoff, a non-documentation IPv4 address, a home-directory
    // path, an e-mail address or host outside the example domains and the allow list, or the
    // name of an internal repository, in any tracked text file of the repository.
    let root = repo_root();
    let paths = tracked_files(&root);
    assert!(
        paths.iter().any(|p| p == "AGENTS.md"),
        "git ls-files did not list AGENTS.md"
    );
    assert!(
        paths.iter().any(|p| p == DENIED_FILE),
        "the deny list is not tracked"
    );
    let lists = real_lists();
    let mut found = Vec::new();
    for (path, text) in texts(&root, scanned(paths)) {
        found.extend(reasons(lint(&path, &text, &lists)));
    }
    assert!(
        found.is_empty(),
        "{} finding(s):\n{}",
        found.len(),
        found.join("\n")
    );
}

#[test]
fn hygiene_lint_refuses_a_date_before_the_cutoff() {
    // Catches: a date that names a day before 2026-09-01 and goes unflagged because of how it
    // is spelled (numeric with each separator, compact, year last, month names whole or
    // abbreviated in any case, with or without a day, joined by hyphens or slashes, or the year
    // first, in any four-digit year from 0000 on), or a date on or after the cutoff, a version number or a
    // lone year that is flagged although it names no day before it.
    let refused = [
        ["2025", "01", "02"].join("-"),
        ["2025", "01", "02"].join("/"),
        ["2025", "1", "2"].join("."),
        ["2025", "0102"].concat(),
        ["1", "2", "2025"].join("/"),
        ["March", "3,", "2025"].join(" "),
        ["3rd", "March", "2025"].join(" "),
        ["Sept.", "3,", "2025"].join(" "),
        ["Mar", "2025"].join(" "),
        ["AUGUST", "2026"].join(" "),
        ["2026", "08", "31"].join("-"),
        ["1999", "12", "31"].join("-"),
        ["1066", "10", "14"].join("-"),
        ["3", "Jan", "2025"].join("-"),
        ["3", "SEPT", "2025"].join("/"),
        ["2025", "Jan", "03"].join("-"),
        ["Jan", "2025"].join("-"),
        ["2025", "January", "3"].join(" "),
        ["2025,", "March", "4"].join(" "),
        ["2025", "dec.", "31"].join(" "),
        ["0999", "12", "31"].join("-"),
        ["0001", "01", "01"].join("-"),
        ["0999", "1231"].concat(),
        ["Dec", "31,", "0999"].join(" "),
        ["December", "0999"].join(" "),
        ["31", "12", "0999"].join("/"),
    ];
    for spelling in refused {
        let found = reasons(dates("notes.md", &on(&spelling)));
        assert_eq!(found, [date_reason(&spelling)], "in {spelling:?}");
    }
    let two = format!(
        "a {} b {}",
        ["2025", "01", "02"].join("-"),
        ["Jan", "2024"].join(" ")
    );
    let want = [
        date_reason(&["2025", "01", "02"].join("-")).replace(":1:", ":2:"),
        date_reason(&["Jan", "2024"].join(" ")).replace(":1:", ":2:"),
    ];
    assert_eq!(reasons(dates("notes.md", &format!("clean\n{two}\n"))), want);

    let passing = [
        "2026-09-01",
        "nightly-2026-10-01",
        "Sept 3, 2026",
        "September 2026",
        "December 2030",
        "20261001",
        "edition = \"2024\"",
        "1.1.8+spec-1.1.0",
        "1.2.3",
        "0.2.190",
        "1.2.3.2025",
        "3-Jan-2027",
        "2026-Sep-01",
        "2027 March 4",
        "in 2025 may be wrong",
        "2025-13-01",
        "12345678",
        "it may happen in 2025",
        "1.95.0",
        "a3f5c9d0e1b2",
        "sha-0999a1231b",
        "0999 items",
        "00000000",
        "09991331",
    ];
    for text in passing {
        assert_clean(dates("notes.md", text), text);
    }
    let inside_a_word = ["ab", "2025", "0102", "cd"].concat();
    assert_clean(dates("notes.md", &inside_a_word), &inside_a_word);
}

#[test]
fn hygiene_lint_refuses_a_non_documentation_ipv4() {
    // Catches: a private or public IPv4 address that is not flagged, an address just outside a
    // documentation range (or beside the limited broadcast address) that is let through, or
    // loopback, unspecified, the limited broadcast and documentation addresses, numbers that are not four octets, and version-like names that are flagged.
    let refused = [
        ["10", "0", "0", "1"].join("."),
        ["192", "168", "1", "20"].join("."),
        ["8", "8", "4", "4"].join("."),
        ["192", "0", "3", "1"].join("."),
        ["198", "51", "101", "1"].join("."),
        ["203", "0", "112", "9"].join("."),
        ["255", "255", "255", "254"].join("."),
    ];
    let text = refused
        .iter()
        .map(|ip| format!("host {ip}."))
        .collect::<Vec<_>>()
        .join("\n");
    let want: Vec<String> = refused
        .iter()
        .enumerate()
        .map(|(i, ip)| {
            format!(
                "net.md:{}: the IPv4 address `{ip}` is not loopback, unspecified, the limited broadcast or a documentation address",
                i + 1
            )
        })
        .collect();
    assert_eq!(reasons(ipv4s("net.md", &text)), want);

    let passing = [
        "127.0.0.1",
        "127.255.255.254",
        "0.0.0.0",
        "255.255.255.255",
        "192.0.2.1",
        "198.51.100.77",
        "203.0.113.255",
        "256.1.1.1",
        "1.2.3",
        "1.2.3.4.5",
        "v1.2.3.4",
        "x.1.2.3.4",
    ];
    for text in passing {
        assert_clean(ipv4s("net.md", text), text);
    }
}

#[test]
fn hygiene_lint_refuses_a_home_path() {
    // Catches: a path under a person's or a host's home directory (on Linux or macOS) that is
    // not flagged, or a path that only starts like one (no trailing slash, a longer name).
    let linux = concat!("/ho", "me/alice/.cargo");
    let mac = concat!("/Us", "ers/bob/src");
    let text = format!("cd {linux}\nok\nopen {mac}\n");
    let want = [
        format!(
            "paths.txt:1: the path prefix `{}` names a home directory",
            concat!("/ho", "me/")
        ),
        format!(
            "paths.txt:3: the path prefix `{}` names a home directory",
            concat!("/Us", "ers/")
        ),
    ];
    assert_eq!(reasons(home_paths("paths.txt", &text)), want);
    for text in ["/home", "/Users", "/homes/x", "/var/home"] {
        assert_clean(home_paths("paths.txt", text), text);
    }
}

#[test]
fn hygiene_lint_refuses_an_email_address() {
    // Catches: an e-mail address outside the example domains that is not flagged (a plain one,
    // one with a tag and capitals, one whose domain starts with a digit, one with a quoted local
    // part), or an example address, a systemd unit instance name, an action pin, a version or a
    // bare `@name` that is flagged.
    let plain = ["alice", &["corp-mail", "com"].join(".")].join("@");
    let tagged = ["Bob.Smith+ops", &["Sub", "Corp-Mail", "IO"].join(".")].join("@");
    let digit_first = ["dana", &["42-mail", "com"].join(".")].join("@");
    let quoted = ["\"j doe\"", &["corp-mail", "com"].join(".")].join("@");
    let text = format!("mail {plain} now\nand ({tagged}).\n{digit_first}\nsent to {quoted} too\n");
    let want = [
        format!("m.md:1: the e-mail address `{plain}` is not at an example domain"),
        format!("m.md:2: the e-mail address `{tagged}` is not at an example domain"),
        format!("m.md:3: the e-mail address `{digit_first}` is not at an example domain"),
        format!("m.md:4: the e-mail address `{quoted}` is not at an example domain"),
    ];
    assert_eq!(reasons(emails("m.md", &text)), want);

    let passing = [
        "you@example.com",
        "a@mail.example.org",
        "x@host.test",
        "x@y.invalid",
        "x@box.localhost",
        "x@something.example",
        "tierstore@9301.service",
        "probe-polkit@a.service",
        "getty@tty1.timer",
        "uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1",
        "@AGENTS.md",
        "text\\n@AGENTS.md",
        "pkg@1.2.3",
        "you@7mail.example.com",
        "\"quoted local\"@example.com",
        "worker@42.timer",
        "say \"hi\", then @AGENTS.md",
    ];
    for text in passing {
        assert_clean(emails("m.md", text), text);
    }
}

#[test]
fn hygiene_lint_refuses_a_hostname_under_a_non_example_domain() {
    // Catches: the host of a URL (with user and port, in any case) or a bare dotted name, in any
    // case and under any top-level domain, under a non-example domain that is not flagged; a
    // domain that only ends like an allowed one; or a public site, an example domain, a method
    // call, a field access in Rust code outside a string, an underscore name, a file name and a
    // TOML table header that is flagged.
    let url = ["https", "://", "build", ".internal", "/x?y=1"].concat();
    let url_with_user = ["http", "://", "ci@", "Build", ".Corp", ".io", ":8080/p"].concat();
    let corp = ["build", "corp"].join(".");
    let bare = ["ssh to", corp.as_str(), "now"].join(" ");
    let suffix_only = ["notgithub", "com"].join(".");
    let text = format!("see {url}\nthen {url_with_user}\n{bare}\n{suffix_only}\n");
    let want = [
        format!("h.md:1: {}", host_reason(&["build", "internal"].join("."))),
        format!(
            "h.md:2: {}",
            host_reason(&["build", "corp", "io"].join("."))
        ),
        format!("h.md:3: {}", host_reason(&corp)),
        format!("h.md:4: {}", host_reason(&suffix_only)),
    ];
    assert_eq!(reasons(hostnames("h.md", &text, &public())), want);

    // The same bare name is a host in a string or a comment of a Rust file, not in code.
    let name = ["svc", "dev"].join(".");
    let in_string = format!("let h = \"{name}\";");
    let in_comment = format!("    // talks to {name}");
    let in_code = format!("let a = self.{name};");
    let rs = |text: &str| reasons(hostnames("src/a.rs", text, &public()));
    assert_eq!(
        rs(&in_string),
        [format!("src/a.rs:1: {}", host_reason(&name))]
    );
    assert_eq!(
        rs(&in_comment),
        [format!("src/a.rs:1: {}", host_reason(&name))]
    );
    assert!(rs(&in_code).is_empty(), "code is not read for hosts");

    // A table header is not a host in a TOML file, and is one in prose.
    let header = ["[profile", ".dev]"].concat();
    let toml = reasons(hostnames("Cargo.toml", &header, &public()));
    assert!(toml.is_empty(), "a table header is not a host: {toml:?}");
    let md = reasons(hostnames("notes.md", &header, &public()));
    assert_eq!(
        md,
        [format!(
            "notes.md:1: {}",
            host_reason(&["profile", "dev"].join("."))
        )]
    );

    let passing = [
        "https://github.com/a/b",
        "https://docs.github.com/x",
        "see github.com for more",
        "registry+https://github.com/rust-lang/crates.io-index",
        "https://raw.example.com/z",
        "https://api.example.net",
        "http://localhost/x",
        "http://127.0.0.1/x",
        "http://[::1]/x",
        "https://{host}/x",
        "a build.example.org host",
        "call meta.dev() now",
        "my_mod.dev is code",
        "main.rs and Cargo.toml",
    ];
    for text in passing {
        assert_clean(hostnames("h.md", text, &public()), text);
    }

    // A mixed-case name is a host, as is one under any top-level domain of the IANA list, whether
    // or not it is a common word or a file extension.
    let mixed = ["Build-01", "Corp", "Acme", "COM"].join(".");
    let others = [
        ["vpn", "acme", "de"].join("."),
        ["git", "acme", "co", "uk"].join("."),
        ["files", "acme", "studio"].join("."),
        ["mail", "acme", "localdomain"].join("."),
        ["Wiki", "ACME", "Fr"].join("."),
        ["ci", "acme", "build"].join("."),
        ["www", "acme", "solutions"].join("."),
        ["build-01", "corp", "acme", "rs"].join("."),
        ["fileserver", "acme", "zip"].join("."),
        ["gw", "acme", "xn--p1ai"].join("."),
    ];
    let mut text = format!("ssh to {mixed} now\n");
    for name in &others {
        text.push_str(&format!("see {name}\n"));
    }
    let mut want = vec![format!("h.md:1: {}", host_reason(&mixed.to_lowercase()))];
    for (i, name) in others.iter().enumerate() {
        want.push(format!(
            "h.md:{}: {}",
            i + 2,
            host_reason(&name.to_lowercase())
        ));
    }
    assert_eq!(reasons(hostnames("h.md", &text, &public())), want);
    // The allow list still applies to a name in capitals.
    let loud = ["GitHub", "COM"].join(".");
    assert_clean(hostnames("h.md", &loud, &public()), &loud);

    // A name whose last label is a file suffix is a file name when it has two labels or sits in
    // a path, in any case.
    for text in [
        "lib.rs",
        "Cargo.toml and Cargo.lock",
        "README.MD",
        "run.sh then tool.py",
        "ci.yml, config.yaml, data.json, notes.txt",
        "a.c b.h c.o libz.a libc.so",
        "tests/foo.rs",
        "crates/x/src/lib.rs",
        "src/main.rs",
        "see target/llvm-cov/lcov.info",
        "a.b.rs/more",
        "x/a.b.rs",
        "./a.b.sh",
        "bundle.zip and clip.mov",
        "worker.pid",
    ] {
        assert_clean(hostnames("h.md", text, &public()), text);
    }
    // With three or more labels and no path, such a name is a host.
    let three = ["build-01", "corp", "acme", "rs"].join(".");
    let md_host = ["wiki", "acme", "md"].join(".");
    let text = format!("{three}\n{md_host}.\n");
    assert_eq!(
        reasons(hostnames("h.md", &text, &public())),
        [
            format!("h.md:1: {}", host_reason(&three)),
            format!("h.md:2: {}", host_reason(&md_host)),
        ]
    );
    // An empty label makes no host.
    for text in ["a..com", ".com"] {
        assert_clean(hostnames("h.md", text, &public()), text);
    }
}

#[test]
fn hygiene_lint_reads_comments_and_strings_of_a_rust_file_lexically() {
    // Catches: a host in a trailing or doc comment, a block comment over several lines, a nested
    // block comment, a string with an escaped quote, a byte string, or an interior line of a
    // multi-line raw string (with any number of hashes) that is not flagged; and a dotted name in
    // code, after a character literal holding a quote, beside a lifetime, or after a comment
    // that holds a quote, that is flagged.
    let name = ["svc", "dev"].join(".");
    let code = ["self", "inner", "dev"].join(".");
    let rs = |text: &str| reasons(hostnames("src/a.rs", text, &public()));
    let at = |line: usize| format!("src/a.rs:{line}: {}", host_reason(&name));

    assert_eq!(rs(&format!("let a = 1; // talks to {name}")), [at(1)]);
    assert_eq!(rs(&format!("/// see {name}\nfn a() {{}}")), [at(1)]);
    assert_eq!(
        rs(&format!(
            "fn a() {{}}\n/* first\n   {name}\n*/\nlet b = {code};"
        )),
        [at(3)]
    );
    assert_eq!(
        rs(&format!("/* a /* b */ {name} */\nlet c = {code};")),
        [at(1)]
    );
    assert_eq!(
        rs(&format!(
            "let s = r#\"\nfirst\n{name}\n\"#;\nlet t = {code};"
        )),
        [at(3)]
    );
    assert_eq!(
        rs(&format!(
            "let s = r##\"\n\"# {name}\n\"##;\nlet t = {code};"
        )),
        [at(2)]
    );
    assert_eq!(rs(&format!("let s = \"q\\\"{name}\";")), [at(1)]);
    assert_eq!(rs(&format!("let s = b\"{name}\";")), [at(1)]);
    assert_eq!(rs(&format!("let s = br#\"{name}\"#;")), [at(1)]);

    let in_code = [
        format!("let a = {code};"),
        format!("let q = '\"'; let a = {code};"),
        format!("let q = '\\''; let a = {code};"),
        format!("fn f<'a>(x: &'a str) -> &'a str {{ {code} }}"),
        format!("// a \" quote\nlet a = {code};"),
        format!("let s = \"x\"; let a = {code};"),
        format!("let s = r#\"x\"#; let a = {code};"),
        format!("/* \" */ let a = {code};"),
    ];
    for text in &in_code {
        assert!(rs(text).is_empty(), "code is not read for hosts: {text:?}");
    }
}

#[test]
fn hygiene_lint_tld_list_is_the_pinned_iana_list() {
    // Catches: the committed top-level domain list being edited, truncated or replaced (its
    // SHA-256 is pinned), losing IANA's version header, or losing the generic, country,
    // new-generic and internationalised domains the lint relies on.
    let digest = hex::encode(Sha256::digest(IANA_TEXT.as_bytes()));
    assert_eq!(
        digest,
        "e874bbcdb65cdeaa632d510d69535e4466457e7ad198d73b9c546a10fc9b7fd1"
    );
    assert!(
        IANA_TEXT.starts_with("# Version "),
        "the list does not start with IANA's version line"
    );
    for domain in ["com", "build", "solutions", "de", "rs", "xn--p1ai"] {
        assert!(is_tld(domain), "{domain} is not a top-level domain");
    }
    for word in ["toml", "json", "yml", "e", "g", "localhost", "example"] {
        assert!(!is_tld(word), "{word} is a top-level domain");
    }
    assert_eq!(parse_tlds(IANA_TEXT).len(), 1436);
}

#[test]
fn hygiene_lint_top_level_domain_tables_are_consistent() {
    // Catches: an entry of the file extension or private-use tables that is not a lowercase
    // name, a private-use suffix that IANA delegates (so it belongs to the pinned list), a
    // parsed IANA entry that is not lowercase, and the loss of the extensions the lint relies on.
    for list in [FILE_SUFFIXES, PRIVATE_TLDS] {
        for entry in list {
            assert!(
                !entry.is_empty() && entry.bytes().all(|b| b.is_ascii_lowercase()),
                "not a lowercase name: {entry:?}"
            );
        }
    }
    let iana = parse_tlds(IANA_TEXT);
    for suffix in PRIVATE_TLDS {
        assert!(is_tld(suffix), "{suffix} is not read as a top-level domain");
        assert!(!iana.contains(*suffix), "{suffix} is delegated by IANA");
    }
    for tld in &iana {
        assert!(
            tld.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "not a lowercase top-level domain: {tld:?}"
        );
    }
    for suffix in [
        "rs", "md", "sh", "toml", "yml", "yaml", "json", "lock", "txt", "py", "c", "h", "o", "a",
        "so", "zip", "mov", "pid",
    ] {
        assert!(
            FILE_SUFFIXES.contains(&suffix),
            "{suffix} is not a file suffix"
        );
    }
}

#[test]
fn hygiene_lint_refuses_a_denied_repository_name() {
    // Catches: a name whose SHA-256 is on the deny list that is not flagged (in any case, next
    // to a slash or other punctuation), or a name that only contains a denied one, or a
    // separator-split spelling of it, that is flagged; and a message that prints the name.
    let digest = hex::encode(Sha256::digest(b"acme-private"));
    let denied: HashSet<String> = [digest.clone()].into_iter().collect();
    let text = "fine\nsee Acme-Private/tools and acme-private2 and acme_private here\n";
    let want = [format!(
        "d.md:2: a name on the deny list of internal repository names (sha-256 {digest})"
    )];
    assert_eq!(reasons(repositories("d.md", text, &denied)), want);
    assert_eq!(
        repositories("d.md", "acme private, acme-privat", &denied),
        []
    );
    assert_clean(repositories("d.md", text, &HashSet::new()), text);
}

#[test]
fn hygiene_lint_reports_every_finding_at_once() {
    // Catches: a lint that stops at the first finding or the first rule, so that a change has
    // to be fixed one finding at a time.
    let ip = ["10", "0", "0", "1"].join(".");
    let home = concat!("/ho", "me/alice");
    let text = format!("a {ip}\nb {home}\n");
    let lists = Lists {
        public: public(),
        denied: HashSet::new(),
    };
    let want = [
        format!(
            "x.md:1: the IPv4 address `{ip}` is not loopback, unspecified, the limited broadcast or a documentation address"
        ),
        format!(
            "x.md:2: the path prefix `{}` names a home directory",
            concat!("/ho", "me/")
        ),
    ];
    assert_eq!(reasons(lint("x.md", &text, &lists)), want);
}

#[test]
fn hygiene_lint_skips_dates_only_on_the_pin_lines_of_a_debs_pins_file() {
    // Catches: a package version that holds its snapshot's date (a pin line of
    // `.github/debs/<set>.toml`) refused; and the exemption reaching a comment or another line of
    // that file, a pin line of a file outside the directory or of another kind, or a rule other
    // than the date rule.
    let lists = Lists {
        public: public(),
        denied: HashSet::new(),
    };
    let old = ["2022", "01", "13"].join("");
    let pin = format!("{PIN_LINE}x\", version = \"1.0+git-{old}.fbbdc39-1\" }},");
    let pins = format!("{DEBS_DIR}set.toml");
    assert_clean(lint(&pins, &format!("{pin}\n"), &lists), "a pin line");
    let refused =
        |path: &str, line: usize| format!("{path}:{line}: the date `{old}` is before 2026-09-01");
    let text = format!("# taken {old}\n{pin}\nmirrors = [\"{old}\"]\n");
    assert_eq!(
        reasons(lint(&pins, &text, &lists)),
        [refused(&pins, 1), refused(&pins, 3)]
    );
    for other in ["notes/set.toml", &format!("{DEBS_DIR}set.md")] {
        assert_eq!(
            reasons(lint(other, &format!("{pin}\n"), &lists)),
            [refused(other, 1)]
        );
    }
    let ip = ["10", "0", "0", "1"].join(".");
    let with_ip = format!("{PIN_LINE}x\", version = \"{ip}\" }},\n");
    assert_eq!(lint(&pins, &with_ip, &lists).len(), 1);
}

#[test]
fn hygiene_lint_reads_the_licence_text_for_everything_but_dates() {
    // Catches: the licence text being skipped by the rules other than the date rule, so that an
    // e-mail address, an IPv4 address, a home path or a host added to it goes unflagged; and the
    // licence text's own date being refused, or the date exemption reaching another file.
    let lists = Lists {
        public: public(),
        denied: HashSet::new(),
    };
    let old = ["2014", "03", "09"].join("-");
    let line = |rest: &str| format!("Copyright {old} {rest}\n");
    assert_clean(
        lint(LICENCE_FILE, &line("by the authors"), &lists),
        "the date alone",
    );
    let elsewhere = reasons(lint("NOTICE", &line("by the authors"), &lists));
    assert_eq!(
        elsewhere,
        [format!("NOTICE:1: the date `{old}` is before 2026-09-01")]
    );

    let mail = ["alice", &["corp-mail", "com"].join(".")].join("@");
    let ip = ["10", "0", "0", "1"].join(".");
    let home = concat!("/ho", "me/alice");
    let host = ["build", "corp"].join(".");
    let text = format!(
        "{}{}{}{}",
        line(&format!("write to {mail}")),
        line(&format!("at {ip}")),
        line(&format!("in {home}")),
        line(&format!("see {host}")),
    );
    // The rules report in turn: addresses, then paths, then e-mail, then hosts.
    let want = [
        format!(
            "LICENSE:2: the IPv4 address `{ip}` is not loopback, unspecified, the limited broadcast or a documentation address"
        ),
        format!(
            "LICENSE:3: the path prefix `{}` names a home directory",
            concat!("/ho", "me/")
        ),
        format!("LICENSE:1: the e-mail address `{mail}` is not at an example domain"),
        format!("LICENSE:4: {}", host_reason(&host)),
    ];
    assert_eq!(reasons(lint(LICENCE_FILE, &text, &lists)), want);
}

#[test]
fn hygiene_lint_skips_the_deny_list_file_and_it_holds_only_digests() {
    // Catches: the deny list being scanned (its digests are not names, but a plaintext name
    // added there by mistake would then be flagged in the one place it must not be named), a
    // line of it that is neither a comment nor a lowercase 64-digit SHA-256, and an allow list
    // entry that is not a plain lowercase name.
    let paths = ["Cargo.toml", DENIED_FILE, LICENCE_FILE, "docs/a.md"]
        .map(String::from)
        .to_vec();
    assert_eq!(scanned(paths), ["Cargo.toml", LICENCE_FILE, "docs/a.md"]);
    assert!(
        repo_root().join(DENIED_FILE).is_file(),
        "{DENIED_FILE} does not exist"
    );
    assert!(
        DENIED_TEXT.contains("NOT scanned"),
        "the header does not say the file is not scanned"
    );

    let mut digests = 0;
    for (i, line) in DENIED_TEXT.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let hex_digits = line.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        assert!(
            line.len() == 64 && hex_digits,
            "line {}: not a lowercase SHA-256",
            i + 1
        );
        digests += 1;
    }
    assert!(digests >= 1, "the deny list holds no digest");

    for entry in entries(PUBLIC_TEXT) {
        let domain = entry.strip_prefix(EXACT).unwrap_or(&entry);
        let plain = domain
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-'));
        assert!(
            plain && !domain.starts_with('.'),
            "not a plain domain: {domain:?}"
        );
    }
}

#[test]
fn hygiene_lint_refuses_a_protocol_relative_url_host() {
    // Catches: a host written as the authority of a protocol-relative URL (`//host/path`)
    // escaping the lint because a `/` next to a bare name marks a path component.
    let authority = ["cdn", ".acme", ".io"].concat();
    for text in [
        format!("<script src=\"//{authority}/lib.js\">"),
        format!("load //{authority} first"),
        format!("//{authority}/x"),
    ] {
        assert_eq!(
            reasons(hostnames("page.md", &text, &public())),
            [format!("page.md:1: {}", host_reason(&authority))],
            "{text}"
        );
    }
    let in_rust = format!("let url = \"//{authority}/lib.js\";");
    assert_eq!(
        reasons(hostnames("src/a.rs", &in_rust, &public())),
        [format!("src/a.rs:1: {}", host_reason(&authority))]
    );
    // An allowed host, workspace paths, comments and doc comments are no URLs to refuse.
    for text in [
        "see //github.com/a/b",
        "crates/x//y/lib.rs",
        "a//b.rs and x//y",
        "// a comment",
        "/// a doc comment",
        "file:///x/y",
        "https://github.com/a",
    ] {
        assert_clean(hostnames("page.md", text, &public()), text);
    }
}

#[test]
fn hygiene_lint_exact_entries_allow_only_the_name() {
    // Catches: an allow-list entry meant for one exact name (a file or field name that ends in a
    // word delegated as a top-level domain) letting every host under it through.
    let public = vec![
        format!("{EXACT}hook.group"),
        format!("{EXACT}tool.download"),
        "github.com".to_string(),
    ];
    for name in ["hook.group", "tool.download"] {
        let text = format!("the field `{name}` is read");
        assert_clean(hostnames("notes.md", &text, &public), &text);
        let under = format!("ci.{name}");
        let text = format!("talks to {under} now");
        assert_eq!(
            reasons(hostnames("notes.md", &text, &public)),
            [format!("notes.md:1: {}", host_reason(&under))],
            "{text}"
        );
    }
    // A domain entry still allows the names under it.
    assert_clean(
        hostnames("notes.md", "see docs.github.com", &public),
        "docs",
    );
    // Every exact entry of the real list allows its name and, when it is a dotted name, refuses
    // it with one more label in front (`alpine` is no dotted name, so nothing under it is a host).
    let real = entries(PUBLIC_TEXT);
    let exact: Vec<&str> = real.iter().filter_map(|e| e.strip_prefix(EXACT)).collect();
    assert!(exact.contains(&"hook.group"), "{exact:?}");
    for name in exact.into_iter().filter(|name| name.contains('.')) {
        let text = format!("x {name} y");
        assert_clean(hostnames("notes.md", &text, &real), &text);
        let under = format!("ci.{name}");
        let text = format!("x {under} y");
        assert_eq!(
            reasons(hostnames("notes.md", &text, &real)),
            [format!("notes.md:1: {}", host_reason(&under))],
            "{text}"
        );
    }
}

#[test]
fn hygiene_lint_reads_the_trusted_root_for_everything_but_dates() {
    // Catches: the trusted root's validity dates refused (it is Sigstore's text, compared byte
    // for byte with the live root), the date exemption reaching a file beside it, and the trusted
    // root skipped by the other rules, so that an address, a home path or an unlisted host added
    // to it goes unflagged.
    let lists = Lists {
        public: public(),
        denied: HashSet::new(),
    };
    let old = ["2021", "03", "07"].join("-");
    let valid = format!("{{\"validFor\":{{\"start\":\"{old}T03:20:29Z\"}}}}\n");
    assert_clean(lint(TRUSTED_ROOT_FILE, &valid, &lists), "the date alone");
    let beside = "packaging/sigstore/other.json";
    assert_eq!(
        reasons(lint(beside, &valid, &lists)),
        [format!("{beside}:1: the date `{old}` is before 2026-09-01")]
    );
    let ip = ["10", "0", "0", "1"].join(".");
    let host = ["log", "corp"].join(".");
    let text = format!("{{\"baseUrl\":\"https://{ip}\"}}\n{{\"uri\":\"{host}\"}}\n");
    assert_eq!(
        reasons(lint(TRUSTED_ROOT_FILE, &text, &lists)),
        [
            format!(
                "{TRUSTED_ROOT_FILE}:1: the IPv4 address `{ip}` is not loopback, unspecified, the limited broadcast or a documentation address"
            ),
            format!("{TRUSTED_ROOT_FILE}:2: {}", host_reason(&host)),
        ]
    );
}
