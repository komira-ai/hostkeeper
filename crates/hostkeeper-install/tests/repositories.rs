//! The host's repository configuration (packages-repositories.md section 3.1; packages-tests.md,
//! "The repository configuration"): `load` reads `<name>.sources` files and their keyrings from a
//! scratch directory, and refuses the start on each defect, naming the path.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_install::repository::config::{
    FileFault, MAX_SOURCES_BYTES, Repositories, RepositoriesError, Repository, Rule, load, parse,
};
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// The running user, the owner every file is expected to have.
fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Writes `bytes` at `dir/name` with `mode`.
fn put(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("the mode");
    path
}

/// A repository directory (mode 0755) in a new scratch directory, holding the keyring `k.gpg`.
fn repo_dir(name: &str) -> (Scratch, PathBuf) {
    let scratch = scratch(name);
    let dir = scratch.path().join("repositories");
    fs::create_dir(&dir).expect("the directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("the mode");
    put(&dir, "k.gpg", b"keyring", 0o644);
    (scratch, dir)
}

/// A valid stanza whose keyring is `dir/k.gpg`.
fn good(dir: &Path) -> String {
    format!(
        "Types: deb\nURIs: https://repo.example.com/debian\nSuites: stable\n\
         Components: main\nSigned-By: {}/k.gpg\n",
        dir.display()
    )
}

/// `good(dir)` with `from` replaced by `to`, which must occur in it.
fn changed(dir: &Path, from: &str, to: &str) -> String {
    let text = good(dir);
    assert!(text.contains(from), "{from:?} is in the valid stanza");
    text.replace(from, to)
}

/// The refusal `load` gives for a directory holding `text` as `a.sources` and the keyring.
fn refused_stanza(name: &str, build: impl FnOnce(&Path) -> String) -> (RepositoriesError, PathBuf) {
    let (_scratch, dir) = repo_dir(name);
    let text = build(&dir);
    put(&dir, "a.sources", text.as_bytes(), 0o644);
    let error = load(&dir, me()).expect_err("the start is refused");
    (error, dir.join("a.sources"))
}

/// The rule a stanza is refused for, with the file named and the Display text.
fn rule_of(name: &str, build: impl FnOnce(&Path) -> String) -> (Rule, String) {
    let (error, path) = refused_stanza(name, build);
    let text = error.to_string();
    match error {
        RepositoriesError::Stanza { path: found, rule } => {
            assert_eq!(found, path);
            assert_eq!(text, format!("{}: {rule}", path.display()));
            (rule, text)
        }
        other => panic!("a stanza refusal was expected, not {other:?}"),
    }
}

/// The directory's own refusal.
fn refused_dir(dir: &Path) -> (PathBuf, FileFault, String) {
    let error = load(dir, me()).expect_err("the start is refused");
    let text = error.to_string();
    match error {
        RepositoriesError::File { path, fault } => (path, fault, text),
        other => panic!("a file refusal was expected, not {other:?}"),
    }
}

/// Proves a stanza with comments and an https URI loads as an entry with its name, URI, suite, components, keyring and text, and that the repositories answer get, names and is_empty.
#[test]
fn a_valid_https_entry_loads_with_its_fields() {
    let (_scratch, dir) = repo_dir("https");
    let text = format!(
        "# the vendor's repository\nTypes: deb\nURIs: https://repo.example.com/debian\n\
         # a comment between fields\nSuites: stable\nComponents: main contrib\n\
         Signed-By: {}/k.gpg\n\n# trailing comment\n",
        dir.display()
    );
    put(&dir, "vendor-1.sources", text.as_bytes(), 0o644);
    let repositories = load(&dir, me()).expect("the entry loads");
    assert_eq!(repositories.names(), vec!["vendor-1"]);
    assert!(!repositories.is_empty());
    assert_eq!(
        repositories.get("vendor-1"),
        Some(&Repository {
            name: "vendor-1".to_owned(),
            uri: "https://repo.example.com/debian".to_owned(),
            suite: "stable".to_owned(),
            components: vec!["main".to_owned(), "contrib".to_owned()],
            keyring: dir.join("k.gpg"),
            text,
        })
    );
    assert_eq!(repositories.get("other"), None);
}

/// Proves a `file:/` URI loads, and that continuation lines (tab and spaces) extend the Components field.
#[test]
fn a_valid_file_uri_entry_with_a_continued_field_loads() {
    let (_scratch, dir) = repo_dir("file-uri");
    let text = format!(
        "Types: deb\nURIs: file:/srv/repo\nSuites: testing\nComponents: main\n\tcontrib\n  extra\n\
         Signed-By: {}/k.gpg\nCheck-Valid-Until: yes\n",
        dir.display()
    );
    put(&dir, "local.sources", text.as_bytes(), 0o644);
    let repositories = load(&dir, me()).expect("the entry loads");
    let found = repositories.get("local").expect("the entry");
    assert_eq!(found.uri, "file:/srv/repo");
    assert_eq!(found.suite, "testing");
    assert_eq!(found.components, ["main", "contrib", "extra"]);
}

/// Proves keys are read without regard to case and a 32-byte name is inside the grammar.
#[test]
fn a_key_is_read_in_any_case_and_a_name_of_32_bytes_is_accepted() {
    let (_scratch, dir) = repo_dir("key-case");
    let text = format!(
        "TYPES: deb\nuris: https://r.example/x\nSuItEs: s\ncomponents: main\nSIGNED-BY: {}/k.gpg\n",
        dir.display()
    );
    let name = "a".repeat(32);
    put(&dir, &format!("{name}.sources"), text.as_bytes(), 0o644);
    let repositories = load(&dir, me()).expect("the entry loads");
    assert_eq!(repositories.names(), vec![name.as_str()]);
}

/// Proves two entries load in name order whatever the file modes (0644 or 0600), sharing one keyring, and that `Repositories::new` keys by name.
#[test]
fn several_entries_load_by_name_and_may_share_a_keyring() {
    let (_scratch, dir) = repo_dir("several");
    put(&dir, "b.sources", good(&dir).as_bytes(), 0o644);
    put(&dir, "a.sources", good(&dir).as_bytes(), 0o600);
    let repositories = load(&dir, me()).expect("both load");
    assert_eq!(repositories.names(), vec!["a", "b"]);
    assert_eq!(
        Repositories::new(vec![
            repositories.get("b").expect("b").clone(),
            repositories.get("a").expect("a").clone()
        ]),
        repositories
    );
}

/// Proves an empty directory and a missing one are no repository, not a refusal.
#[test]
fn an_empty_directory_and_a_missing_one_are_no_repository() {
    let scratch = scratch("none");
    let empty = scratch.path().join("empty");
    fs::create_dir(&empty).expect("the directory");
    fs::set_permissions(&empty, fs::Permissions::from_mode(0o755)).expect("the mode");
    for dir in [empty, scratch.path().join("missing")] {
        let repositories = load(&dir, me()).expect("no entry is not a refusal");
        assert!(repositories.is_empty());
        assert!(repositories.names().is_empty());
        assert_eq!(repositories, Repositories::default());
    }
}

/// Proves an upper-case name, an empty stem, an underscore and a 33-byte name are refused with the file's path and the name in the text.
/// Catches: a start that accepts a name no order can name.
#[test]
fn a_name_outside_the_grammar_is_refused_naming_the_file() {
    let long = "a".repeat(33);
    for (file, stem) in [
        ("A.sources", "A".to_owned()),
        (".sources", String::new()),
        ("a_b.sources", "a_b".to_owned()),
        (&format!("{long}.sources"), long.clone()),
    ] {
        let (_scratch, dir) = repo_dir("name");
        let path = put(&dir, file, good(&dir).as_bytes(), 0o644);
        let error = load(&dir, me()).expect_err("the start is refused");
        assert_eq!(
            error,
            RepositoriesError::Name {
                path: path.clone(),
                name: stem.clone()
            },
            "{file}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "{}: the name {stem:?} is not 1 to 32 bytes of [a-z0-9-]",
                path.display()
            )
        );
    }
}

/// Proves a directory owned by a user other than the expected one is refused with both uids in the text.
#[test]
fn a_directory_not_owned_by_the_expected_user_is_refused() {
    let (_scratch, dir) = repo_dir("owner");
    put(&dir, "a.sources", good(&dir).as_bytes(), 0o644);
    assert!(load(&dir, me()).is_ok(), "its owner is the expected user");
    let other = me() + 1;
    let error = load(&dir, other).expect_err("another expected owner");
    assert_eq!(
        error,
        RepositoriesError::File {
            path: dir.clone(),
            fault: FileFault::WrongOwner {
                found: me(),
                expected: other
            }
        }
    );
    assert_eq!(
        error.to_string(),
        format!("{}: is owned by uid {}, not {other}", dir.display(), me())
    );
}

/// Proves a directory group-writable (0775) is refused with its mode.
#[test]
fn a_directory_writable_by_group_is_refused() {
    let (_scratch, dir) = repo_dir("dir-writable");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).expect("the mode");
    let (path, fault, text) = refused_dir(&dir);
    assert_eq!(path, dir);
    assert_eq!(fault, FileFault::Writable { found: 0o775 });
    assert_eq!(
        text,
        format!(
            "{}: has mode 0775: group or others can write it",
            dir.display()
        )
    );
}

/// Proves the directory given as a link, or as a regular file, is refused.
#[test]
fn a_directory_that_is_a_link_or_a_file_is_refused() {
    let (scratch, dir) = repo_dir("dir-link");
    let link = scratch.path().join("link");
    symlink(&dir, &link).expect("the link");
    let (path, fault, text) = refused_dir(&link);
    assert_eq!((path, fault), (link.clone(), FileFault::Link));
    assert_eq!(text, format!("{}: is a symbolic link", link.display()));
    let file = put(scratch.path(), "plain", b"x", 0o644);
    let (path, fault, text) = refused_dir(&file);
    assert_eq!((path, fault), (file.clone(), FileFault::NotDirectory));
    assert_eq!(text, format!("{}: is not a directory", file.display()));
}

/// Proves a directory that cannot be listed, and one whose parent cannot be searched, are refused with the OS error.
#[test]
fn a_directory_that_cannot_be_read_or_reached_is_refused_with_the_os_error() {
    let (scratch, dir) = repo_dir("dir-unreadable");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).expect("the mode");
    let unreadable = load(&dir, me());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("the mode");
    let reached = scratch.path().join("closed");
    fs::create_dir(&reached).expect("the directory");
    fs::create_dir(reached.join("repositories")).expect("the inner directory");
    fs::set_permissions(&reached, fs::Permissions::from_mode(0o000)).expect("the mode");
    let unreached = load(&reached.join("repositories"), me());
    fs::set_permissions(&reached, fs::Permissions::from_mode(0o755)).expect("the mode");
    assert_eq!(
        unreadable.expect_err("listing refused").to_string(),
        format!(
            "{}: cannot be read: Permission denied (os error 13)",
            dir.display()
        )
    );
    assert_eq!(
        unreached.expect_err("stat refused").to_string(),
        format!(
            "{}: cannot be read: Permission denied (os error 13)",
            reached.join("repositories").display()
        )
    );
}

/// Proves a `.sources` file writable by group, by others or by both is refused with its mode.
#[test]
fn a_sources_file_writable_by_group_or_others_is_refused() {
    for (mode, name) in [(0o664, "group"), (0o646, "others"), (0o666, "both")] {
        let (_scratch, dir) = repo_dir(name);
        let path = put(&dir, "a.sources", good(&dir).as_bytes(), mode);
        let error = load(&dir, me()).expect_err("the start is refused");
        assert_eq!(
            error,
            RepositoriesError::File {
                path: path.clone(),
                fault: FileFault::Writable { found: mode }
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "{}: has mode {mode:04o}: group or others can write it",
                path.display()
            )
        );
    }
}

/// Proves a keyring writable by group or by others is refused, naming the entry that names it.
#[test]
fn a_keyring_writable_by_group_or_others_is_refused_naming_the_entry() {
    for (mode, name) in [(0o664, "kgroup"), (0o646, "kothers")] {
        let (_scratch, dir) = repo_dir(name);
        let path = put(&dir, "a.sources", good(&dir).as_bytes(), 0o644);
        put(&dir, "k.gpg", b"keyring", mode);
        let error = load(&dir, me()).expect_err("the start is refused");
        assert_eq!(
            error,
            RepositoriesError::Keyring {
                path: path.clone(),
                fault: FileFault::Writable { found: mode }
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "{}: the keyring it names has mode {mode:04o}: group or others can write it",
                path.display()
            )
        );
    }
}

/// Proves a keyring that is a link, is missing, or is a directory is refused, naming the entry.
#[test]
fn a_keyring_that_is_a_link_or_missing_or_not_a_file_is_refused() {
    let (_scratch, dir) = repo_dir("keyring-link");
    let path = put(&dir, "a.sources", good(&dir).as_bytes(), 0o644);
    fs::remove_file(dir.join("k.gpg")).expect("the keyring");
    let outside = dir.parent().expect("a parent").join("outside");
    fs::write(&outside, b"keyring").expect("the target");
    symlink(&outside, dir.join("k.gpg")).expect("the link");
    let error = load(&dir, me()).expect_err("the link is refused");
    assert_eq!(
        error,
        RepositoriesError::Keyring {
            path: path.clone(),
            fault: FileFault::Link
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the keyring it names is a symbolic link",
            path.display()
        )
    );
    fs::remove_file(dir.join("k.gpg")).expect("the link");
    let error = load(&dir, me()).expect_err("a missing keyring is refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the keyring it names cannot be read: No such file or directory (os error 2)",
            path.display()
        )
    );
    fs::create_dir(dir.join("k.gpg")).expect("a directory as keyring");
    let error = load(&dir, me()).expect_err("a directory is not a keyring");
    assert_eq!(
        error,
        RepositoriesError::Keyring {
            path: path.clone(),
            fault: FileFault::NotRegular
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the keyring it names is not a regular file",
            path.display()
        )
    );
}

/// Proves a `.sources` entry that is a link (never followed), a directory, or unreadable is refused.
#[test]
fn a_sources_file_that_is_a_link_a_directory_or_unreadable_is_refused() {
    let (_scratch, dir) = repo_dir("sources-kinds");
    let target = put(&dir, "k2.gpg", b"x", 0o644);
    let path = dir.join("a.sources");
    symlink(&target, &path).expect("the link");
    let error = load(&dir, me()).expect_err("a link is refused");
    assert_eq!(
        error,
        RepositoriesError::File {
            path: path.clone(),
            fault: FileFault::Link
        }
    );
    assert_eq!(
        error.to_string(),
        format!("{}: is a symbolic link", path.display())
    );
    fs::remove_file(&path).expect("the link");
    fs::create_dir(&path).expect("a directory");
    let error = load(&dir, me()).expect_err("a directory is refused");
    assert_eq!(
        error.to_string(),
        format!("{}: is not a regular file", path.display())
    );
    fs::remove_dir(&path).expect("the directory");
    put(&dir, "a.sources", good(&dir).as_bytes(), 0o000);
    let error = load(&dir, me()).expect_err("an unreadable file is refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: cannot be read: Permission denied (os error 13)",
            path.display()
        )
    );
}

/// Proves a file no stanza's Signed-By names, and a keyring no entry names, are refused as stray.
#[test]
fn a_file_no_signed_by_names_is_refused_as_stray() {
    let (_scratch, dir) = repo_dir("stray");
    put(&dir, "a.sources", good(&dir).as_bytes(), 0o644);
    let stray = put(&dir, "notes.txt", b"x", 0o644);
    let error = load(&dir, me()).expect_err("the start is refused");
    assert_eq!(
        error,
        RepositoriesError::Stray {
            path: stray.clone()
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "{}: neither a .sources file nor a keyring one names",
            stray.display()
        )
    );
    // A keyring and no entry that names it is stray too.
    fs::remove_file(dir.join("a.sources")).expect("the entry");
    assert_eq!(
        load(&dir, me()).expect_err("no entry names k.gpg"),
        RepositoriesError::Stray {
            path: dir.join("k.gpg")
        }
    );
}

/// Proves a subdirectory of the configuration directory is refused.
#[test]
fn a_subdirectory_is_refused_as_stray() {
    let (_scratch, dir) = repo_dir("subdir");
    put(&dir, "a.sources", good(&dir).as_bytes(), 0o644);
    let sub = dir.join("sub");
    fs::create_dir(&sub).expect("the subdirectory");
    assert_eq!(
        load(&dir, me()).expect_err("the start is refused"),
        RepositoriesError::Stray { path: sub }
    );
}

/// Proves a file of exactly MAX_SOURCES_BYTES loads and one byte more is refused.
#[test]
fn a_file_over_the_size_bound_is_refused_and_one_at_it_is_accepted() {
    let (_scratch, dir) = repo_dir("size");
    let base = good(&dir);
    let want = usize::try_from(MAX_SOURCES_BYTES).expect("fits");
    let pad = want - base.len();
    let at = format!("{base}#{}\n", "x".repeat(pad - 2));
    assert_eq!(at.len(), want);
    put(&dir, "a.sources", at.as_bytes(), 0o644);
    assert!(load(&dir, me()).is_ok(), "exactly the bound is accepted");
    let over = format!("{base}#{}\n", "x".repeat(pad - 1));
    assert_eq!(over.len(), want + 1);
    let path = put(&dir, "a.sources", over.as_bytes(), 0o644);
    let error = load(&dir, me()).expect_err("one byte over is refused");
    assert_eq!(
        error,
        RepositoriesError::File {
            path: path.clone(),
            fault: FileFault::TooLarge
        }
    );
    assert_eq!(
        error.to_string(),
        format!("{}: is over 16384 bytes", path.display())
    );
}

/// Proves a file with bytes that are not UTF-8 is refused.
#[test]
fn a_file_that_is_not_utf8_is_refused() {
    let (_scratch, dir) = repo_dir("utf8");
    let mut bytes = good(&dir).into_bytes();
    bytes.extend_from_slice(b"# \xff\xfe\n");
    let path = put(&dir, "a.sources", &bytes, 0o644);
    let error = load(&dir, me()).expect_err("the start is refused");
    assert_eq!(
        error,
        RepositoriesError::File {
            path: path.clone(),
            fault: FileFault::NotUtf8
        }
    );
    assert_eq!(
        error.to_string(),
        format!("{}: is not UTF-8", path.display())
    );
}

/// Proves two stanzas, and a file with comments, blank lines or nothing at all, are refused.
#[test]
fn two_stanzas_or_none_are_refused() {
    let (rule, text) = rule_of("two", |dir| format!("{}\n{}", good(dir), good(dir)));
    assert_eq!(rule, Rule::Stanzas(2));
    assert!(text.ends_with(": holds 2 stanzas, not one"), "{text}");
    for (name, body) in [
        ("comments", "# only a comment\n# and another\n"),
        ("empty", ""),
        ("blank", "\n  \n\n"),
    ] {
        let (rule, text) = rule_of(name, |_| body.to_owned());
        assert_eq!(rule, Rule::NoStanza, "{name}");
        assert!(text.ends_with(": holds no stanza"), "{text}");
    }
}

/// Proves a line with no colon, a continuation as the first line or after a blank line, a key with a space and an empty key are refused with the line's number.
#[test]
fn a_syntax_error_is_refused_with_its_line_number() {
    let cases: [(&str, String); 5] = [
        (
            "nocolon",
            "Types: deb\nURIs https://x.example/y\n".to_owned(),
        ),
        ("first", " continued: deb\nTypes: deb\n".to_owned()),
        ("spacekey", "Types: deb\nBad Key: x\n".to_owned()),
        ("nokey", "# c\n: deb\n".to_owned()),
        // A continuation after a blank line belongs to no field.
        ("afterblank", "Types: deb\n\n  deb\n".to_owned()),
    ];
    let wants = [2, 1, 2, 2, 3];
    for ((name, body), line) in cases.into_iter().zip(wants) {
        let (rule, text) = rule_of(name, |_| body);
        assert_eq!(rule, Rule::Syntax(line), "{name}");
        assert!(
            text.ends_with(&format!(
                ": line {line} is neither a field, a continuation nor a comment"
            )),
            "{text}"
        );
    }
}

/// Proves `deb deb-src`, `deb-src` and an empty Types are refused.
#[test]
fn a_type_other_than_deb_is_refused() {
    for (name, types) in [("both", "deb deb-src"), ("src", "deb-src"), ("empty", "")] {
        let (rule, text) = rule_of(name, |dir| {
            changed(dir, "Types: deb", &format!("Types: {types}"))
        });
        assert_eq!(rule, Rule::Types(types.to_owned()));
        assert!(
            text.ends_with(&format!(": has Types {types:?}, not exactly \"deb\"")),
            "{text}"
        );
    }
}

/// Proves two URIs, no URI, `http://`, a bare `file:`, `file:/`, `https://`, a relative file URI and another scheme are refused.
#[test]
fn a_uri_must_be_one_https_or_file_uri() {
    let (rule, text) = rule_of("two-uris", |dir| {
        changed(
            dir,
            "https://repo.example.com/debian",
            "https://a.example/x file:/srv/y",
        )
    });
    assert_eq!(rule, Rule::Uris(2));
    assert!(text.ends_with(": has 2 URIs, not one"), "{text}");
    let (rule, _) = rule_of("no-uris", |dir| {
        changed(dir, "URIs: https://repo.example.com/debian", "URIs:")
    });
    assert_eq!(rule, Rule::Uris(0));
    for (name, uri) in [
        ("http", "http://repo.example.com/debian"),
        ("file-empty", "file:"),
        ("file-slash", "file:/"),
        ("https-empty", "https://"),
        ("relative", "file:srv/repo"),
        ("ftp", "ftp://repo.example.com/"),
    ] {
        let (rule, text) = rule_of(name, |dir| {
            changed(dir, "https://repo.example.com/debian", uri)
        });
        assert_eq!(rule, Rule::Scheme(uri.to_owned()), "{uri}");
        assert!(
            text.ends_with(&format!(
                ": has the URI {uri:?}, neither https:// nor file:/"
            )),
            "{text}"
        );
    }
}

/// Proves two suites, no suite and no component are refused.
#[test]
fn a_suite_must_be_one_and_a_component_at_least_one() {
    let (rule, text) = rule_of("two-suites", |dir| {
        changed(dir, "Suites: stable", "Suites: stable testing")
    });
    assert_eq!(rule, Rule::Suites(2));
    assert!(text.ends_with(": has 2 suites, not one"), "{text}");
    let (rule, _) = rule_of("no-suite", |dir| changed(dir, "Suites: stable", "Suites:"));
    assert_eq!(rule, Rule::Suites(0));
    let (rule, text) = rule_of("no-component", |dir| {
        changed(dir, "Components: main", "Components:")
    });
    assert_eq!(rule, Rule::NoComponent);
    assert!(text.ends_with(": has no component"), "{text}");
}

/// Proves a relative Signed-By, one outside the directory, a `.sources` file, a hidden name, a deeper path and the directory itself are refused.
#[test]
fn a_signed_by_that_is_not_a_keyring_in_the_directory_is_refused() {
    type SignedBy = Box<dyn Fn(&Path) -> String>;
    let cases: [(&str, SignedBy); 6] = [
        ("relative", Box::new(|_| "k.gpg".to_owned())),
        ("outside", Box::new(|_| "/etc/other/k.gpg".to_owned())),
        (
            "sources",
            Box::new(|d| format!("{}/b.sources", d.display())),
        ),
        ("hidden", Box::new(|d| format!("{}/.k.gpg", d.display()))),
        ("deeper", Box::new(|d| format!("{}/sub/k.gpg", d.display()))),
        ("the-directory", Box::new(|d| d.display().to_string())),
    ];
    for (name, signed_by) in cases {
        let mut shown = String::new();
        let (rule, text) = rule_of(name, |dir| {
            shown = signed_by(dir);
            changed(
                dir,
                &format!("Signed-By: {}/k.gpg", dir.display()),
                &format!("Signed-By: {shown}"),
            )
        });
        assert_eq!(rule, Rule::SignedBy(shown.clone()), "{name}");
        assert!(
            text.ends_with(&format!(
                ": has the Signed-By {shown:?}, not an absolute path of a keyring in its directory"
            )),
            "{text}"
        );
    }
}

/// Proves an inline key, as continuation lines or as `-----BEGIN` on the line, is refused.
#[test]
fn an_inline_key_in_signed_by_is_refused() {
    for (name, signed_by) in [
        (
            "continued",
            "Signed-By:\n -----BEGIN PGP PUBLIC KEY BLOCK-----\n .\n mQINBF\n -----END PGP PUBLIC KEY BLOCK-----",
        ),
        ("begin", "Signed-By: -----BEGIN PGP PUBLIC KEY BLOCK-----"),
    ] {
        let (rule, text) = rule_of(name, |dir| {
            changed(
                dir,
                &format!("Signed-By: {}/k.gpg", dir.display()),
                signed_by,
            )
        });
        assert_eq!(rule, Rule::InlineKey, "{name}");
        assert!(
            text.ends_with(": has a Signed-By that is an inline key, not a path"),
            "{text}"
        );
    }
}

/// Proves Trusted, Allow-Insecure, Allow-Weak and Allow-Downgrade-To-Insecure are refused in any case of the key and with any value.
#[test]
fn each_key_that_turns_off_a_check_is_refused_in_any_case() {
    for (name, key) in [
        ("trusted", "Trusted"),
        ("trusted-upper", "TRUSTED"),
        ("insecure", "Allow-Insecure"),
        ("insecure-lower", "allow-insecure"),
        ("weak", "Allow-Weak"),
        ("weak-mixed", "aLLow-wEAK"),
        ("downgrade", "Allow-Downgrade-To-Insecure"),
    ] {
        let (rule, text) = rule_of(name, |dir| format!("{}{key}: yes\n", good(dir)));
        let lower = key.to_ascii_lowercase();
        assert_eq!(rule, Rule::Weakening(lower.clone()), "{key}");
        assert!(
            text.ends_with(&format!(
                ": has the key {lower:?}, which turns off a check of apt's"
            )),
            "{text}"
        );
    }
    // Whatever its value: `no` does not make it harmless.
    let (rule, _) = rule_of("trusted-no", |dir| format!("{}Trusted: no\n", good(dir)));
    assert_eq!(rule, Rule::Weakening("trusted".to_owned()));
}

/// Proves `Check-Valid-Until` with `no`, empty or another value is refused and `yes` is accepted.
#[test]
fn check_valid_until_is_accepted_only_as_yes() {
    for (name, value) in [("no", "no"), ("empty", ""), ("other", "false")] {
        let (rule, text) = rule_of(name, |dir| {
            format!("{}Check-Valid-Until: {value}\n", good(dir))
        });
        assert_eq!(rule, Rule::ValidUntil(value.to_owned()), "{value}");
        assert!(
            text.ends_with(&format!(
                ": has `Check-Valid-Until: {value}`; only `yes` is accepted"
            )),
            "{text}"
        );
    }
    let (_scratch, dir) = repo_dir("valid-until-yes");
    put(
        &dir,
        "a.sources",
        format!("{}check-valid-until: yes\n", good(&dir)).as_bytes(),
        0o644,
    );
    assert!(load(&dir, me()).is_ok());
}

/// Proves a key outside the allowed ones and a key given twice (also in another case) are refused.
#[test]
fn an_unknown_key_and_a_duplicate_key_are_refused() {
    let (rule, text) = rule_of("unknown", |dir| {
        format!("{}Architectures: amd64\n", good(dir))
    });
    assert_eq!(rule, Rule::UnknownKey("architectures".to_owned()));
    assert!(
        text.ends_with(": has the unknown key \"architectures\""),
        "{text}"
    );
    let (rule, text) = rule_of("duplicate", |dir| format!("{}Suites: stable\n", good(dir)));
    assert_eq!(rule, Rule::Duplicate("suites".to_owned()));
    assert!(text.ends_with(": has the key \"suites\" twice"), "{text}");
    // Keys differing only in case are the same key.
    let (rule, _) = rule_of("duplicate-case", |dir| {
        format!("{}SUITES: testing\n", good(dir))
    });
    assert_eq!(rule, Rule::Duplicate("suites".to_owned()));
}

/// Proves each of the five required keys, left out, is refused naming it.
#[test]
fn each_required_key_missing_is_refused() {
    let cases: [(&str, &'static str, &str); 5] = [
        ("types", "types", "Types: deb\n"),
        ("uris", "uris", "URIs: https://repo.example.com/debian\n"),
        ("suites", "suites", "Suites: stable\n"),
        ("components", "components", "Components: main\n"),
        ("signed-by", "signed-by", ""),
    ];
    for (name, key, line) in cases {
        let (rule, text) = rule_of(name, |dir| {
            let line = if line.is_empty() {
                format!("Signed-By: {}/k.gpg\n", dir.display())
            } else {
                line.to_owned()
            };
            changed(dir, &line, "")
        });
        assert_eq!(rule, Rule::Missing(key));
        assert!(text.ends_with(&format!(": has no {key:?}")), "{text}");
    }
}

/// Proves `parse` reads a stanza text for a directory with no file system, and refuses a missing key.
#[test]
fn parse_reads_a_stanza_without_the_file_checks() {
    let dir = Path::new("/etc/hostkeeper/repositories");
    let text = format!(
        "Types: deb\nURIs: https://r.example/x\nSuites: s\nComponents: a b\nSigned-By: {}/k.gpg\n",
        dir.display()
    );
    let found = parse("r", &text, dir).expect("the stanza parses");
    assert_eq!(found.keyring, dir.join("k.gpg"));
    assert_eq!(found.components, ["a", "b"]);
    assert_eq!(found.text, text);
    assert_eq!(parse("r", "Types: deb\n", dir), Err(Rule::Missing("uris")));
}
