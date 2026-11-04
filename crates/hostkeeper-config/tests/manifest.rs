//! The manifest of a package and the load rules of design section 4.7: the format, the keys,
//! the hook, the versions and the class of each refusal. The rules on paths, units, health and
//! params are in `manifest_paths.rs`. Each case builds a version directory from the design's
//! tierstore example and breaks one thing.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;

use hostkeeper_config::manifest::{
    BadHostkeeperVersion, Class, FileEntry, HostkeeperVersion, MAX_MANIFEST_BYTES, Manifest,
    ManifestRefusal, Timeouts,
};
use hostkeeper_config::names::UnitError;
use support::fifo;
use support::package::{
    EXAMPLE, Package, RUNNING, expected, invalid_key, secs, unknown, with, with_in,
};

/// Catches a renamed key, a dropped value or a wrong default: the example, with its units,
/// files and binary, loads and each field is the example's.
#[test]
fn valid_tierstore_manifest_loads() {
    let package = Package::new("valid");
    let manifest = package.load().expect("the design's example loads");
    assert_eq!(manifest.name.as_str(), "tierstore");
    assert_eq!(manifest.version.as_str(), "1.5.0");
    assert_eq!(manifest.min_hostkeeper, RUNNING);
    assert_eq!(manifest.hook.command, ["bin/tierstore", "update-hook"]);
    assert_eq!(manifest.hook.boot.as_deref(), Some("restore"));
    assert_eq!(manifest.hook.verify_deadline_secs, 1600);
    assert_eq!(manifest.hook.promotion_ceiling_secs, Some(secs(3600)));
    assert_eq!(manifest.hook.cleanup_retries, 3);
    assert_eq!(
        manifest.hook.timeout_secs,
        Timeouts {
            prepare: None,
            cutover: secs(300),
            verify: secs(1000),
            rollback: secs(600),
            cleanup: secs(1300),
            uninstall: Some(secs(300)),
        }
    );
    assert_eq!(
        manifest.unit_files,
        BTreeSet::from(["tierstore@.service".to_owned()])
    );
    assert_eq!(
        manifest.run,
        ["tierstore@9301.service", "tierstore@9302.service"]
    );
    assert_eq!(
        manifest.health_url.as_deref(),
        Some("http://127.0.0.1:{port}/healthz")
    );
    assert_eq!(
        manifest.files,
        [FileEntry {
            source: "files/tierstore.conf".to_owned(),
            path: "/etc/tierstore/tierstore.conf".to_owned(),
        }]
    );
    assert_eq!(
        manifest.params.get("promotion_bound_secs"),
        Some(&toml::Value::Integer(900))
    );
}

/// Catches an optional table or key that is required, or a default that is not the documented
/// one: with no boot, ceiling, retries, uninstall, units, health, files or params, the manifest
/// loads with none of each and 3 retries.
#[test]
fn the_optional_tables_and_keys_may_be_left_out() {
    let package = Package::new("minimal");
    fs::remove_dir_all(package.dir().join("units")).unwrap();
    let text = r#"schema = 1
name = "tierstore"
version = "1.5.0"
min_hostkeeper = "0.1.0"
[hook]
command = ["bin/tierstore"]
verify_deadline_secs = 2
[hook.timeout_secs]
prepare = 5
cutover = 1
verify = 1
rollback = 1
cleanup = 1
"#;
    let manifest = package.load_text(text).expect("a minimal manifest loads");
    assert_eq!(manifest.hook.boot, None);
    assert_eq!(manifest.hook.promotion_ceiling_secs, None);
    assert_eq!(manifest.hook.cleanup_retries, 3);
    assert_eq!(manifest.hook.timeout_secs.prepare, Some(secs(5)));
    assert_eq!(manifest.hook.timeout_secs.uninstall, None);
    assert!(manifest.unit_files.is_empty() && manifest.run.is_empty());
    assert_eq!(manifest.health_url, None);
    assert!(manifest.files.is_empty() && manifest.params.is_empty());
    for (from, key) in [
        ("prepare = 5\n", "hook.timeout_secs.prepare"),
        ("cutover = 1\n", "hook.timeout_secs.cutover"),
        ("verify = 1\n", "hook.timeout_secs.verify"),
        ("rollback = 1\n", "hook.timeout_secs.rollback"),
        ("cleanup = 1\n", "hook.timeout_secs.cleanup"),
        ("verify_deadline_secs = 2\n", "hook.verify_deadline_secs"),
        ("command = [\"bin/tierstore\"]\n", "hook.command"),
        ("min_hostkeeper = \"0.1.0\"\n", "min_hostkeeper"),
        ("schema = 1\n", "schema"),
        ("name = \"tierstore\"\n", "name"),
        ("version = \"1.5.0\"\n", "version"),
    ] {
        let refusal = package.refusal(&with_in(text, from, ""));
        assert!(refusal.to_string().contains("is missing"), "{refusal}");
        assert_eq!(invalid_key(refusal), key, "{from:?}");
    }
    for (header, key) in [
        ("[hook]", "hook"),
        ("[hook.timeout_secs]", "hook.timeout_secs"),
    ] {
        let cut = &text[..text.find(header).unwrap()];
        assert_eq!(invalid_key(package.refusal(cut)), key, "{header}");
    }
}

/// Catches a manifest of another service or another version being taken for the order's; each
/// is its own refusal.
#[test]
fn name_mismatch_and_version_mismatch_are_refused_separately() {
    let package = Package::new("mismatch");
    let other = package.refusal(&with("name           = \"tierstore\"", "name = \"tier\""));
    assert_eq!(
        other,
        ManifestRefusal::NameMismatch {
            expected: "tierstore".to_owned(),
            found: "tier".to_owned(),
        }
    );
    let older = package.refusal(&with("version        = \"1.5.0\"", "version = \"1.4.2\""));
    assert_eq!(
        older,
        ManifestRefusal::VersionMismatch {
            expected: "1.5.0".to_owned(),
            found: "1.4.2".to_owned(),
        }
    );
    package.manifest(EXAMPLE);
    let order = Manifest::load(&package.dir(), &expected("tierstore", "1.6.0"), &RUNNING);
    assert!(matches!(
        order,
        Err(ManifestRefusal::VersionMismatch { .. })
    ));
    let order = Manifest::load(&package.dir(), &expected("web", "1.5.0"), &RUNNING);
    assert!(matches!(order, Err(ManifestRefusal::NameMismatch { .. })));
    assert!(older.to_string().contains("1.4.2"));
    let both = package.refusal(&with(
        "name           = \"tierstore\"\nversion        = \"1.5.0\"",
        "name = \"x\"\nversion = \"2\"",
    ));
    assert!(matches!(both, ManifestRefusal::NameMismatch { .. }));
}

/// Catches a table that ignores a key it does not know: an extra key in each table is
/// `UnknownKey` naming the key and the table, `[params]` excepted.
#[test]
fn unknown_key_is_refused_in_every_table() {
    let package = Package::new("unknown");
    for (from, to, table, key) in [
        (
            "schema         = 1\n",
            "schema = 1\nextra = 1\n",
            "",
            "extra",
        ),
        (
            "boot    = \"restore\"",
            "boot = \"restore\"\nextra = 1",
            "hook",
            "extra",
        ),
        (
            "uninstall = 300",
            "uninstall = 300\nreload = 5",
            "hook.timeout_secs",
            "reload",
        ),
        (
            "run = [\"tierstore@9301.service\", \"tierstore@9302.service\"]",
            "run = []\nenable = []",
            "units",
            "enable",
        ),
        (
            "url = \"http://127.0.0.1:{port}/healthz\"",
            "url = \"http://127.0.0.1/healthz\"\ntimeout = 3",
            "health",
            "timeout",
        ),
        (
            "path   = \"/etc/tierstore/tierstore.conf\"",
            "path = \"/etc/tierstore/tierstore.conf\"\nmode = 420",
            "files[0]",
            "mode",
        ),
        ("[params]\n", "[extras]\n[params]\n", "", "extras"),
    ] {
        assert_eq!(
            package.refusal(&with(from, to)),
            unknown(table, key),
            "{to}"
        );
    }
    let second = format!(
        "{EXAMPLE}\n[[files]]\nsource = \"files/tierstore.conf\"\npath = \"/opt/x\"\nowner = \"root\"\n"
    );
    assert_eq!(package.refusal(&second), unknown("files[1]", "owner"));
    let open = with("promotion_bound_secs = 900", "anything = 1\nelse = true");
    assert!(package.load_text(&open).is_ok());
    let error = package.refusal(&with(
        "boot    = \"restore\"",
        "boot = \"restore\"\nextra = 1",
    ));
    assert!(error.to_string().contains("\"extra\""), "{error}");
}

/// Catches the declaration format surviving in the manifest: `user`, `capabilities`, a write
/// path, a signer, a release URL and `min_version` are unknown keys where the declaration had
/// them.
#[test]
fn user_capabilities_signer_url_and_min_version_keys_are_unknown_keys() {
    let package = Package::new("declaration");
    for (from, to, table, key) in [
        ("[hook]\n", "[hook]\nuser = \"root\"\n", "hook", "user"),
        (
            "[hook]\n",
            "[hook]\ncapabilities = []\n",
            "hook",
            "capabilities",
        ),
        (
            "[hook]\n",
            "[hook]\nwrite_paths = [\"/opt/x\"]\n",
            "hook",
            "write_paths",
        ),
        (
            "schema         = 1\n",
            "schema = 1\nsigner = \"x\"\n",
            "",
            "signer",
        ),
        (
            "schema         = 1\n",
            "schema = 1\nrelease_url = \"https://x\"\n",
            "",
            "release_url",
        ),
        (
            "schema         = 1\n",
            "schema = 1\nmin_version = \"1.0\"\n",
            "",
            "min_version",
        ),
        (
            "schema         = 1\n",
            "schema = 1\nuser = \"root\"\n",
            "",
            "user",
        ),
    ] {
        assert_eq!(
            package.refusal(&with(from, to)),
            unknown(table, key),
            "{to}"
        );
    }
}

/// Catches `prepare = 0` refused: it is the unbounded form, and loads as no bound.
#[test]
fn unbounded_prepare_is_accepted() {
    let package = Package::new("prepare");
    let manifest = package.load().expect("prepare = 0 loads");
    assert_eq!(manifest.hook.timeout_secs.prepare, None);
    let bounded = package
        .load_text(&with("prepare = 0", "prepare = 1"))
        .expect("prepare = 1 loads");
    assert_eq!(bounded.hook.timeout_secs.prepare, Some(secs(1)));
}

/// Catches a 0 accepted for a step other than `prepare`: one case per step.
#[test]
fn zero_cutover_verify_rollback_cleanup_uninstall_are_each_refused() {
    let package = Package::new("zero");
    for (line, step) in [
        ("cutover = 300", "cutover"),
        ("verify  = 1000", "verify"),
        ("rollback = 600", "rollback"),
        ("cleanup = 1300", "cleanup"),
        ("uninstall = 300", "uninstall"),
    ] {
        let refusal = package.refusal(&with(line, &format!("{step} = 0")));
        assert_eq!(refusal, ManifestRefusal::ZeroBound { step }, "{step}");
        assert!(refusal.to_string().contains(step));
    }
    let negative = package.refusal(&with("cutover = 300", "cutover = -1"));
    assert_eq!(invalid_key(negative), "hook.timeout_secs.cutover");
}

/// The boundary of the verify deadline: exactly the verify and rollback bounds together is
/// accepted, with the largest bounds a manifest can hold too.
#[test]
fn verify_deadline_equal_to_verify_plus_rollback_is_accepted() {
    let package = Package::new("deadline-ok");
    assert!(package.load().is_ok(), "1600 = 1000 + 600");
    let max = i64::MAX;
    let text = with("verify  = 1000", &format!("verify = {max}"))
        .replace("rollback = 600", &format!("rollback = {max}"))
        .replace(
            "verify_deadline_secs   = 1600",
            &format!("verify_deadline_secs = {max}"),
        );
    assert_eq!(
        package.refusal(&text),
        ManifestRefusal::DeadlineTooShort {
            deadline: max as u64,
            verify: max as u64,
            rollback: max as u64,
        }
    );
}

/// The other side: one second less is refused, with the three numbers.
#[test]
fn one_less_is_refused() {
    let package = Package::new("deadline-short");
    let refusal = package.refusal(&with(
        "verify_deadline_secs   = 1600",
        "verify_deadline_secs = 1599",
    ));
    assert_eq!(
        refusal,
        ManifestRefusal::DeadlineTooShort {
            deadline: 1599,
            verify: 1000,
            rollback: 600,
        }
    );
    assert!(refusal.to_string().contains("1599"));
}

/// Catches a command that is not a program in the package, and a `boot` that is not an
/// additive step's name.
#[test]
fn the_hook_command_and_the_boot_step_are_checked() {
    let package = Package::new("command");
    let command = "command = [\"bin/tierstore\", \"update-hook\"]";
    for bad in [
        "command = []",
        "command = [\"/usr/bin/tierstore\"]",
        "command = [\"../tierstore\"]",
        "command = [\"bin/../x\"]",
        "command = \"bin/tierstore\"",
        "command = [1]",
    ] {
        assert_eq!(
            invalid_key(package.refusal(&with(command, bad))),
            "hook.command",
            "{bad}"
        );
    }
    for bad in [
        "\"\"",
        "\"Restore\"",
        "\"re-store\"",
        "\"prepare\"",
        "\"cutover\"",
        "\"verify\"",
        "\"rollback\"",
        "\"cleanup\"",
        "\"uninstall\"",
        "5",
    ] {
        let text = with("boot    = \"restore\"", &format!("boot = {bad}"));
        assert_eq!(invalid_key(package.refusal(&text)), "hook.boot", "{bad}");
    }
    let long = with(
        "boot    = \"restore\"",
        &format!("boot = \"{}\"", "a".repeat(65)),
    );
    assert_eq!(invalid_key(package.refusal(&long)), "hook.boot");
    let longest = with(
        "boot    = \"restore\"",
        &format!("boot = \"{}\"", "a".repeat(64)),
    );
    assert!(package.load_text(&longest).is_ok());
    for (from, to, key) in [
        (
            "promotion_ceiling_secs = 3600",
            "promotion_ceiling_secs = 0",
            "hook.promotion_ceiling_secs",
        ),
        (
            "promotion_ceiling_secs = 3600",
            "promotion_ceiling_secs = \"1h\"",
            "hook.promotion_ceiling_secs",
        ),
        (
            "cleanup_retries        = 3",
            "cleanup_retries = 4294967296",
            "hook.cleanup_retries",
        ),
        (
            "cleanup_retries        = 3",
            "cleanup_retries = -1",
            "hook.cleanup_retries",
        ),
    ] {
        assert_eq!(invalid_key(package.refusal(&with(from, to))), key, "{to}");
    }
    for (to, value) in [("cleanup_retries = 0", 0), ("cleanup_retries = 7", 7)] {
        let text = with("cleanup_retries        = 3", to);
        assert_eq!(
            package.load_text(&text).unwrap().hook.cleanup_retries,
            value
        );
    }
    let one = with(
        "promotion_ceiling_secs = 3600",
        "promotion_ceiling_secs = 1",
    );
    assert_eq!(
        package.load_text(&one).unwrap().hook.promotion_ceiling_secs,
        Some(secs(1))
    );
    let most = with("cleanup_retries        = 3", "cleanup_retries = 4294967295");
    assert_eq!(
        package.load_text(&most).unwrap().hook.cleanup_retries,
        u32::MAX
    );
}

/// Catches a daemon too old being reported as a bad package: a `min_hostkeeper` above the
/// running daemon is `DaemonTooOld`, class `Environment`, reason `daemon_too_old`.
#[test]
fn min_hostkeeper_above_running_is_daemon_too_old_not_parse_error() {
    let package = Package::new("too-old");
    for needs in ["0.4.1", "0.5.0", "1.0.0", "0.10.0"] {
        let text = with(
            "min_hostkeeper = \"0.4.0\"",
            &format!("min_hostkeeper = \"{needs}\""),
        );
        let refusal = package.refusal(&text);
        assert_eq!(
            refusal,
            ManifestRefusal::DaemonTooOld {
                needs: HostkeeperVersion::parse(needs).unwrap(),
                running: RUNNING,
            },
            "{needs}"
        );
        assert_eq!(refusal.class(), Class::Environment);
        assert_eq!(refusal.reason(), "daemon_too_old");
        assert!(refusal.to_string().contains(needs), "{refusal}");
    }
}

/// The other side: a floor equal to or below the running daemon loads.
#[test]
fn min_hostkeeper_equal_or_below_running_loads() {
    let package = Package::new("floor-ok");
    for floor in ["0.4.0", "0.3.9", "0.0.0", "0.3.99"] {
        let text = with(
            "min_hostkeeper = \"0.4.0\"",
            &format!("min_hostkeeper = \"{floor}\""),
        );
        let manifest = package
            .load_text(&text)
            .unwrap_or_else(|e| panic!("{floor}: {e}"));
        assert_eq!(manifest.min_hostkeeper.to_string(), floor);
    }
}

/// Catches a malformed floor read as "too old": a pre-release, a build suffix, two or four
/// numbers, a leading zero or a non-string is a release refusal.
#[test]
fn malformed_min_hostkeeper_is_a_release_refusal_not_too_old() {
    let package = Package::new("floor-bad");
    for floor in [
        "\"9.0.0-rc.1\"",
        "\"9.0.0+build\"",
        "\"9.0\"",
        "\"192.0.2.1\"",
        "\"09.0.0\"",
        "\"v9.0.0\"",
        "\"\"",
        "\"9..0\"",
        "\"99999999999999999999.0.0\"",
        "9",
    ] {
        let text = with(
            "min_hostkeeper = \"0.4.0\"",
            &format!("min_hostkeeper = {floor}"),
        );
        let refusal = package.refusal(&text);
        assert_eq!(refusal.class(), Class::Release, "{floor}");
        assert_eq!(refusal.reason(), "manifest_mismatch", "{floor}");
        assert_eq!(invalid_key(refusal), "min_hostkeeper", "{floor}");
    }
    assert_eq!(
        HostkeeperVersion::parse("1.2"),
        Err(BadHostkeeperVersion("1.2".to_owned()))
    );
    assert!(
        BadHostkeeperVersion("x".into())
            .to_string()
            .contains("major.minor.patch")
    );
}

/// Catches the floor being judged before the rest: a manifest that breaks another rule and
/// also needs a newer daemon is refused for the other rule.
#[test]
fn too_old_is_not_reported_when_another_rule_also_fails() {
    let package = Package::new("order");
    let too_old = with("min_hostkeeper = \"0.4.0\"", "min_hostkeeper = \"9.0.0\"");
    for (from, to, expected) in [
        (
            "cutover = 300",
            "cutover = 0",
            ManifestRefusal::ZeroBound { step: "cutover" },
        ),
        (
            "promotion_bound_secs = 900",
            "updater.x = 1",
            ManifestRefusal::ReservedParam {
                key: "updater".to_owned(),
            },
        ),
        (
            "\"/etc/tierstore/tierstore.conf\"",
            "\"/usr/x\"",
            ManifestRefusal::ReservedPath {
                path: "/usr/x".to_owned(),
                prefix: "/usr",
            },
        ),
        (
            "version        = \"1.5.0\"",
            "version = \"1.4.0\"",
            ManifestRefusal::VersionMismatch {
                expected: "1.5.0".to_owned(),
                found: "1.4.0".to_owned(),
            },
        ),
        (
            "schema         = 1\n",
            "schema = 1\nuser = \"root\"\n",
            unknown("", "user"),
        ),
    ] {
        let refusal = package.refusal(&with_in(&too_old, from, to));
        assert_eq!(refusal, expected, "{to}");
        assert_eq!(refusal.class(), Class::Release, "{to}");
    }
    assert!(matches!(
        package.refusal(&too_old),
        ManifestRefusal::DaemonTooOld { .. }
    ));
}

/// Catches a manifest read through a link, or a large one read whole: a link is refused, a
/// manifest of exactly the cap is read and one byte more is `TooLarge`; a directory, a pipe, a
/// missing file and bytes that are not text are unreadable.
#[test]
fn manifest_symlink_or_oversized_file_is_refused() {
    let package = Package::new("file");
    let real = package.scratch.file("real.toml", EXAMPLE, 0o444);
    let manifest = package.dir().join("manifest.toml");
    fs::remove_file(&manifest).unwrap();
    symlink(&real, &manifest).unwrap();
    assert!(matches!(
        package.load(),
        Err(ManifestRefusal::Unreadable { .. })
    ));
    fs::remove_file(&manifest).unwrap();
    let pad = |total: u64| {
        let comment = "#".repeat(total as usize - EXAMPLE.len() - 1);
        format!("{EXAMPLE}{comment}\n")
    };
    assert!(
        package.load_text(&pad(MAX_MANIFEST_BYTES)).is_ok(),
        "exactly the cap"
    );
    assert_eq!(
        package.refusal(&pad(MAX_MANIFEST_BYTES + 1)),
        ManifestRefusal::TooLarge
    );
    fs::remove_file(&manifest).unwrap();
    assert!(matches!(
        package.load(),
        Err(ManifestRefusal::Unreadable { .. })
    ));
    fs::create_dir(&manifest).unwrap();
    let directory = package.load().expect_err("refused");
    assert!(
        directory.to_string().contains("not a regular file"),
        "{directory}"
    );
    fs::remove_dir(&manifest).unwrap();
    fifo(&manifest);
    assert!(matches!(
        package.load(),
        Err(ManifestRefusal::Unreadable { .. })
    ));
    fs::remove_file(&manifest).unwrap();
    fs::write(&manifest, b"schema = 1\nname = \"\xff\"\n").unwrap();
    let bytes = package.load().expect_err("refused");
    assert!(bytes.to_string().contains("UTF-8"), "{bytes}");
}

/// Catches a parse error without its line, and a schema that is not an integer.
#[test]
fn a_syntax_error_names_its_line() {
    let package = Package::new("syntax");
    let text = with("cutover = 300", "cutover = = 300");
    let line = EXAMPLE
        .lines()
        .position(|line| line.starts_with("cutover"))
        .unwrap()
        + 1;
    assert!(matches!(
        package.refusal(&text),
        ManifestRefusal::Syntax { line: found, .. } if found == line
    ));
    let schema = with("schema         = 1", "schema = \"1\"");
    assert_eq!(invalid_key(package.refusal(&schema)), "schema");
}

/// B8: a later format is an explicit step. A schema other than 1 is refused naming it, before
/// any key of the new format is judged, so `schema = 2` with keys this loader does not know is
/// a schema refusal, not a silent load.
#[test]
fn manifest_schema_other_than_1_is_refused_naming_the_schema() {
    let package = Package::new("schema");
    for found in [0, 2, -1] {
        let text = with("schema         = 1", &format!("schema = {found}"));
        let refusal = package.refusal(&text);
        assert_eq!(refusal, ManifestRefusal::Schema { found });
        assert!(
            refusal.to_string().contains(&format!("schema {found}")),
            "{refusal}"
        );
    }
    let future = with("schema         = 1\n", "schema = 2\nnew_table_key = 1\n");
    assert_eq!(
        package.refusal(&future),
        ManifestRefusal::Schema { found: 2 }
    );
    let today = with("schema         = 1\n", "schema = 1\nnew_table_key = 1\n");
    assert_eq!(package.refusal(&today), unknown("", "new_table_key"));
}

/// The index of each refusal in [`every_refusal`]. The match has no wildcard, so a refusal added
/// to the crate and not here does not compile.
fn index(refusal: &ManifestRefusal) -> usize {
    match refusal {
        ManifestRefusal::Unreadable { .. } => 0,
        ManifestRefusal::TooLarge => 1,
        ManifestRefusal::Syntax { .. } => 2,
        ManifestRefusal::Schema { .. } => 3,
        ManifestRefusal::UnknownKey { .. } => 4,
        ManifestRefusal::Invalid { .. } => 5,
        ManifestRefusal::NameMismatch { .. } => 6,
        ManifestRefusal::VersionMismatch { .. } => 7,
        ManifestRefusal::ZeroBound { .. } => 8,
        ManifestRefusal::DeadlineTooShort { .. } => 9,
        ManifestRefusal::FileSource { .. } => 10,
        ManifestRefusal::FilePath { .. } => 11,
        ManifestRefusal::ReservedPath { .. } => 12,
        ManifestRefusal::DuplicatePath { .. } => 13,
        ManifestRefusal::UnitFile { .. } => 14,
        ManifestRefusal::RunUnit { .. } => 15,
        ManifestRefusal::PortWithoutInstance { .. } => 16,
        ManifestRefusal::ReservedParam { .. } => 17,
        ManifestRefusal::NonScalarParam { .. } => 18,
        ManifestRefusal::DaemonTooOld { .. } => 19,
        ManifestRefusal::UnitNotAFile { .. } => 20,
    }
}

fn every_refusal() -> Vec<ManifestRefusal> {
    let s = |text: &str| text.to_owned();
    vec![
        ManifestRefusal::Unreadable { why: s("x") },
        ManifestRefusal::TooLarge,
        ManifestRefusal::Syntax {
            line: 1,
            message: s("x"),
        },
        ManifestRefusal::Schema { found: 2 },
        unknown("", "x"),
        ManifestRefusal::Invalid {
            key: s("x"),
            why: s("y"),
        },
        ManifestRefusal::NameMismatch {
            expected: s("a"),
            found: s("b"),
        },
        ManifestRefusal::VersionMismatch {
            expected: s("1"),
            found: s("2"),
        },
        ManifestRefusal::ZeroBound { step: "verify" },
        ManifestRefusal::DeadlineTooShort {
            deadline: 1,
            verify: 1,
            rollback: 1,
        },
        ManifestRefusal::FileSource {
            entry: s("x"),
            why: s("y"),
        },
        ManifestRefusal::FilePath {
            path: s("x"),
            why: s("y"),
        },
        ManifestRefusal::ReservedPath {
            path: s("/usr/x"),
            prefix: "/usr",
        },
        ManifestRefusal::DuplicatePath { path: s("/x") },
        ManifestRefusal::UnitFile {
            file: s("x"),
            why: UnitError::NotAUnit,
        },
        ManifestRefusal::RunUnit {
            unit: s("x"),
            why: s("y"),
        },
        ManifestRefusal::PortWithoutInstance { unit: s("x") },
        ManifestRefusal::ReservedParam { key: s("updater") },
        ManifestRefusal::NonScalarParam { key: s("x") },
        ManifestRefusal::DaemonTooOld {
            needs: RUNNING,
            running: RUNNING,
        },
        ManifestRefusal::UnitNotAFile { file: s("x") },
    ]
}

/// Catches a refusal with no class or reason, a reason that is not a token, or a class other
/// than the design's: every refusal is listed once, `DaemonTooOld` alone is `Environment`
/// with `daemon_too_old`, and every other is `Release` with `manifest_mismatch` (design 5.5).
#[test]
fn every_refusal_has_a_class_and_a_closed_reason_token() {
    let all = every_refusal();
    let seen: BTreeSet<usize> = all.iter().map(index).collect();
    assert_eq!(seen, (0..all.len()).collect::<BTreeSet<_>>());
    for refusal in &all {
        let reason = refusal.reason();
        assert!(
            !reason.is_empty() && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{reason}"
        );
        assert!(!refusal.to_string().is_empty());
        let expected = match refusal {
            ManifestRefusal::DaemonTooOld { .. } => (Class::Environment, "daemon_too_old"),
            _ => (Class::Release, "manifest_mismatch"),
        };
        assert_eq!((refusal.class(), reason), expected, "{refusal:?}");
    }
}

/// Catches a public type whose value cannot be copied, compared or shown: a loaded manifest, its
/// order and its parts clone to equal values, and each prints.
#[test]
fn a_loaded_manifest_clones_compares_and_prints() {
    let package = Package::new("values");
    let manifest = package.load().expect("loads");
    assert_eq!(manifest.clone(), manifest);
    assert_eq!(manifest.hook.clone(), manifest.hook);
    let order = expected("tierstore", "1.5.0");
    assert_eq!(order.clone(), order);
    for shown in [
        format!("{manifest:?}"),
        format!("{order:?}"),
        format!("{:?}", Class::Release),
        format!("{:?}", manifest.files[0].clone()),
        format!("{:?}", BadHostkeeperVersion("x".into())),
    ] {
        assert!(!shown.is_empty());
    }
}

/// Catches a table given a value of another type being reported as anything but that table:
/// `hook`, `hook.timeout_secs`, `units` and `health`, each a number with nothing else wrong, are
/// `Invalid` naming the table.
#[test]
fn a_table_given_another_type_is_refused_naming_it() {
    let package = Package::new("table-types");
    let timeouts =
        &EXAMPLE[EXAMPLE.find("[hook.timeout_secs]").unwrap()..EXAMPLE.find("[units]").unwrap()];
    let cases = [
        (with(timeouts, "timeout_secs = 5\n\n"), "hook.timeout_secs"),
        (
            with_in(
                &with(timeouts, ""),
                &EXAMPLE
                    [EXAMPLE.find("[hook]").unwrap()..EXAMPLE.find("[hook.timeout_secs]").unwrap()],
                "",
            )
            .replace("schema         = 1\n", "schema = 1\nhook = 1\n"),
            "hook",
        ),
        (
            with(
                "[units]\nrun = [\"tierstore@9301.service\", \"tierstore@9302.service\"]\n",
                "",
            )
            .replace("schema         = 1\n", "schema = 1\nunits = 1\n"),
            "units",
        ),
        (
            with("[health]\nurl = \"http://127.0.0.1:{port}/healthz\"\n", "")
                .replace("schema         = 1\n", "schema = 1\nhealth = 1\n"),
            "health",
        ),
    ];
    for (text, key) in cases {
        let refusal = package.refusal(&text);
        assert!(
            refusal.to_string().contains("not a table"),
            "{key}: {refusal}"
        );
        assert_eq!(invalid_key(refusal), key);
    }
}

/// Catches a required key of a `[units]` or `[[files]]` table that may be left out: with the
/// table kept, a missing `run`, `source` or `path` is `Invalid` naming it.
#[test]
fn a_missing_key_of_units_or_files_names_it() {
    let package = Package::new("table-keys");
    for (from, key) in [
        (
            "run = [\"tierstore@9301.service\", \"tierstore@9302.service\"]\n",
            "units|run",
        ),
        ("source = \"files/tierstore.conf\"\n", "files[0].source"),
        (
            "path   = \"/etc/tierstore/tierstore.conf\"\n",
            "files[0].path",
        ),
    ] {
        let refusal = package.refusal(&with(from, ""));
        assert!(refusal.to_string().contains("is missing"), "{refusal}");
        // `units|run` stands for the dotted key, which written whole reads as a host name.
        assert_eq!(invalid_key(refusal), key.replace('|', "."), "{from:?}");
    }
}

/// Catches an unknown key reported after the order or a value is judged: with an unknown key, a
/// name mismatch and a bad bound are not what is reported.
#[test]
fn an_unknown_key_is_reported_before_the_order_and_the_values() {
    let package = Package::new("unknown-first");
    let extra = with("schema         = 1\n", "schema = 1\nextra = 1\n");
    for (from, to) in [
        ("name           = \"tierstore\"", "name = \"other\""),
        ("cutover = 300", "cutover = 0"),
        ("\"/etc/tierstore/tierstore.conf\"", "\"relative\""),
    ] {
        let text = with_in(&extra, from, to);
        assert_eq!(package.refusal(&text), unknown("", "extra"), "{to}");
    }
}

/// The accepted side of the deadline: more than the verify and rollback bounds together loads.
#[test]
fn a_deadline_above_verify_plus_rollback_is_accepted() {
    let package = Package::new("deadline-above");
    let text = with(
        "verify_deadline_secs   = 1600",
        "verify_deadline_secs = 1601",
    );
    assert_eq!(
        package.load_text(&text).unwrap().hook.verify_deadline_secs,
        1601
    );
}
