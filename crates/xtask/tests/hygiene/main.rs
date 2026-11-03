//! The hygiene lint: every tracked text file of this (public) repository is held to the rules in
//! [`rules`], and each rule is shown to refuse its defect by running it on a text that holds one.
//!
//! The files come from `git ls-files` at the repository root. A file whose bytes are not UTF-8 is
//! binary and is skipped; every other tracked file is read. The deny list of internal repository
//! names ([`DENIED_FILE`]) is not scanned, because its job is to hold the digests of those
//! names, and neither is the licence text ([`LICENCE_FILE`]), which carries its authors' date. Test sources that need a text a rule refuses build it from pieces at
//! run time, so that this directory passes its own lint.

mod rules;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use rules::{
    Finding, Lists, dates, emails, entries, home_paths, hostnames, ipv4s, lint, repositories,
};
use sha2::{Digest, Sha256};

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

/// The licence text, verbatim from its authors, dated as they dated it: not scanned.
const LICENCE_FILE: &str = "LICENSE";

/// The paths the lint reads: all but the deny list and the licence text.
fn scanned(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|p| p != DENIED_FILE && p != LICENCE_FILE)
        .collect()
}

/// `(path, text)` of every scanned file that is text. A directory (a submodule) and a file that
/// is not UTF-8 are skipped; a file that cannot be read panics.
fn texts(root: &Path, paths: Vec<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for path in paths {
        let full = root.join(&path);
        if full.is_dir() {
            continue;
        }
        let bytes = std::fs::read(&full).unwrap_or_else(|e| panic!("read {path}: {e}"));
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
    // abbreviated in any case, with or without a day), or a date on or after the cutoff, a
    // version number or a lone year that is flagged although it names no day before it.
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
        "1.2.3.2025",
        "2025-13-01",
        "12345678",
        "it may happen in 2025",
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
    // documentation range that is let through, or loopback, unspecified and documentation
    // addresses, numbers that are not four octets, and version-like names that are flagged.
    let refused = [
        ["10", "0", "0", "1"].join("."),
        ["192", "168", "1", "20"].join("."),
        ["8", "8", "4", "4"].join("."),
        ["192", "0", "3", "1"].join("."),
        ["198", "51", "101", "1"].join("."),
        ["203", "0", "112", "9"].join("."),
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
                "net.md:{}: the IPv4 address `{ip}` is not loopback, unspecified or a documentation address",
                i + 1
            )
        })
        .collect();
    assert_eq!(reasons(ipv4s("net.md", &text)), want);

    let passing = [
        "127.0.0.1",
        "127.255.255.254",
        "0.0.0.0",
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
    // one with a tag and capitals), or an example address, a systemd unit instance name, an
    // action pin, a version or a bare `@name` that is flagged.
    let plain = ["alice", &["corp-mail", "com"].join(".")].join("@");
    let tagged = ["Bob.Smith+ops", "Sub.Corp-Mail.IO"].join("@");
    let text = format!("mail {plain} now\nand ({tagged}).\n");
    let want = [
        format!("m.md:1: the e-mail address `{plain}` is not at an example domain"),
        format!("m.md:2: the e-mail address `{tagged}` is not at an example domain"),
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
    ];
    for text in passing {
        assert_clean(emails("m.md", text), text);
    }
}

#[test]
fn hygiene_lint_refuses_a_hostname_under_a_non_example_domain() {
    // Catches: the host of a URL (with user and port, in any case) or a bare dotted name under a
    // non-example domain that is not flagged; a domain that only ends like an allowed one; or a
    // public site, an example domain, a method call, a field access in Rust code outside a
    // string, an underscore or capitalised name, and a TOML table header that is flagged.
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
    let capitalised = ["Build", "corp"].join(".");
    assert_clean(hostnames("h.md", &capitalised, &public()), &capitalised);
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
            "x.md:1: the IPv4 address `{ip}` is not loopback, unspecified or a documentation address"
        ),
        format!(
            "x.md:2: the path prefix `{}` names a home directory",
            concat!("/ho", "me/")
        ),
    ];
    assert_eq!(reasons(lint("x.md", &text, &lists)), want);
}

#[test]
fn hygiene_lint_skips_the_deny_list_file_and_it_holds_only_digests() {
    // Catches: the licence text, with its authors' date, being scanned and failing the tree;
    // the deny list being scanned (its digests are not names, but a plaintext name
    // added there by mistake would then be flagged in the one place it must not be named), a
    // line of it that is neither a comment nor a lowercase 64-digit SHA-256, and an allow list
    // entry that is not a plain lowercase name.
    let paths = ["Cargo.toml", DENIED_FILE, LICENCE_FILE, "docs/a.md"]
        .map(String::from)
        .to_vec();
    assert_eq!(scanned(paths), ["Cargo.toml", "docs/a.md"]);
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

    for domain in entries(PUBLIC_TEXT) {
        let plain = domain
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-'));
        assert!(
            plain && !domain.starts_with('.'),
            "not a plain domain: {domain:?}"
        );
    }
}
