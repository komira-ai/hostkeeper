//! `target.json` and the manifest read from it: the entry written beside a repository version,
//! read back as it was, and turned into the manifest the daemon reads, or refused with the key
//! that is wrong.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, ManifestRefusal};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{Hook, Manifest, Param, StepBounds, Target, param};
use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

const OWN: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 70,
};

fn expected() -> Expected {
    Expected {
        name: ServiceName::new("web").expect("a name"),
        version: Version::new("1.0.0").expect("a version"),
        platform: Platform::current(),
    }
}

fn bounds() -> StepBounds {
    StepBounds {
        prepare: 60,
        cutover: 61,
        verify: 62,
        rollback: 63,
        cleanup: 64,
        uninstall: Some(65),
    }
}

/// An apt entry whose manifest every rule accepts.
fn entry() -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: "ab".repeat(32),
        manifest: Some(Manifest {
            package: Some("web".to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(bounds()),
                verify_deadline_secs: 300,
                ..Hook::default()
            }),
            run: vec!["web.service".to_owned()],
            health_url: Some("http://127.0.0.1:8080/healthz".to_owned()),
            min_hostkeeper: "0.1.0".to_owned(),
            ..Manifest::default()
        }),
        repository: Some("main".to_owned()),
        package_version: Some("1.0.0-1".to_owned()),
        ..Target::default()
    }
}

fn with_manifest(change: impl FnOnce(&mut Manifest)) -> Target {
    let mut entry = entry();
    change(entry.manifest.as_mut().expect("a manifest"));
    entry
}

fn with_bounds(change: impl FnOnce(&mut StepBounds)) -> Target {
    with_manifest(|manifest| {
        change(
            manifest
                .hook
                .as_mut()
                .and_then(|hook| hook.timeout_secs.as_mut())
                .expect("bounds"),
        );
    })
}

fn invalid(key: &str, why: &str) -> ManifestRefusal {
    ManifestRefusal::Invalid {
        key: key.to_owned(),
        why: why.to_owned(),
    }
}

fn of(
    dir: &Path,
    entry: &Target,
) -> Result<hostkeeper_config::manifest::Manifest, ManifestRefusal> {
    of_entry(dir, entry, &expected(), &OWN)
}

/// Proves an entry written as `target.json` reads back equal, field for field, and that the file
/// is the read-only canonical JSON. Catches an encoding that drops or renames an entry field.
#[test]
fn an_entry_written_reads_back_equal_and_the_file_is_read_only() {
    let dir = scratch("target-roundtrip");
    let mut written = entry();
    written.bytes = 12;
    written
        .manifest
        .as_mut()
        .expect("a manifest")
        .params
        .insert(
            "level".to_owned(),
            Param {
                value: Some(param::Value::Integer(3)),
            },
        );
    write(dir.path(), &written).expect("written");
    let file = dir.path().join(TARGET_FILE);
    assert_eq!(
        fs::metadata(&file).expect("the file").permissions().mode() & 0o777,
        0o444
    );
    assert_eq!(
        fs::read(&file).expect("its bytes"),
        to_json(&written).expect("json")
    );
    assert_eq!(read(dir.path()).expect("read"), Some(written));
}

/// Proves a second write into the same staging directory fails with an I/O error and leaves the
/// first copy as it was. Catches a write that replaces a sealed entry.
#[test]
fn a_second_write_is_refused_and_keeps_the_first_copy() {
    let dir = scratch("target-twice");
    write(dir.path(), &entry()).expect("written");
    let mut other = entry();
    other.digest = "cd".repeat(32);
    let error = write(dir.path(), &other).expect_err("exists");
    assert!(matches!(error, TargetError::Io(_)), "{error}");
    assert_eq!(read(dir.path()).expect("read"), Some(entry()));
}

/// Proves writing into a directory that does not exist is an I/O error naming the file.
#[test]
fn a_write_into_a_missing_directory_is_an_io_error() {
    let dir = scratch("target-missing-dir");
    let error = write(&dir.path().join("none"), &entry()).expect_err("no directory");
    assert!(matches!(error, TargetError::Io(_)));
    assert!(error.to_string().starts_with("target.json: "), "{error}");
}

/// Proves a version directory without `target.json` has no entry. Catches an archive version
/// read as an error.
#[test]
fn a_directory_without_target_json_has_no_entry() {
    let dir = scratch("target-none");
    assert_eq!(read(dir.path()).expect("read"), None);
}

/// Proves a `target.json` that is not JSON of an entry is `Invalid`, with the decoder's words.
#[test]
fn a_target_json_that_is_not_an_entry_is_invalid() {
    let dir = scratch("target-garbage");
    fs::write(dir.path().join(TARGET_FILE), b"not json").expect("write");
    let error = read(dir.path()).expect_err("garbage");
    assert!(matches!(error, TargetError::Invalid(_)), "{error}");
    assert!(
        error
            .to_string()
            .starts_with("target.json is not an entry: ")
    );
    assert!(matches!(
        from_json(b"{\"nope\": 1}"),
        Err(TargetError::Invalid(_))
    ));
    assert!(matches!(from_json(b""), Err(TargetError::Invalid(_))));
}

/// Proves a `target.json` of more than [`MAX_TARGET_BYTES`] is refused by size, and one of exactly
/// that size is read (and then refused as no entry, not as too large).
#[test]
fn a_target_json_over_the_size_limit_is_refused_by_size() {
    let dir = scratch("target-big");
    let file = dir.path().join(TARGET_FILE);
    fs::write(&file, vec![b' '; MAX_TARGET_BYTES as usize + 1]).expect("write");
    let error = read(dir.path()).expect_err("too big");
    assert_eq!(
        error.to_string(),
        format!("target.json is not an entry: it is larger than {MAX_TARGET_BYTES} bytes")
    );
    fs::write(&file, vec![b' '; MAX_TARGET_BYTES as usize]).expect("write");
    let error = read(dir.path()).expect_err("blank");
    assert!(!error.to_string().contains("larger than"), "{error}");
}

/// Proves a `target.json` that cannot be read (here a directory) is an I/O error.
#[test]
fn a_target_json_that_cannot_be_read_is_an_io_error() {
    let dir = scratch("target-dir");
    fs::create_dir(dir.path().join(TARGET_FILE)).expect("a directory");
    assert!(matches!(read(dir.path()), Err(TargetError::Io(_))));
}

/// Proves the manifest of a directory with `target.json` is the entry's, carrying the expected
/// name, version and platform, with the entry's bounds, run units and health URL, and no unit file
/// or placed file.
#[test]
fn the_manifest_of_a_version_with_target_json_is_the_entrys() {
    let dir = scratch("target-manifest");
    write(dir.path(), &entry()).expect("written");
    let manifest = manifest(dir.path(), &expected(), &OWN).expect("a manifest");
    assert_eq!(manifest.name.as_str(), "web");
    assert_eq!(manifest.version.as_str(), "1.0.0");
    assert_eq!(manifest.platform, Platform::current());
    assert_eq!(manifest.run, vec!["web.service".to_owned()]);
    assert_eq!(
        manifest.health_url.as_deref(),
        Some("http://127.0.0.1:8080/healthz")
    );
    assert!(manifest.unit_files.is_empty());
    assert!(manifest.files.is_empty());
    assert_eq!(manifest.hook.verify_deadline_secs, 300);
    assert_eq!(manifest.hook.timeout_secs.cutover.get(), 61);
}

/// Proves a directory with only `manifest.toml` is read by the manifest loader, as an archive
/// version always was.
#[test]
fn the_manifest_of_a_version_without_target_json_is_manifest_toml() {
    let dir = scratch("target-toml");
    fs::write(
        dir.path().join("manifest.toml"),
        crate::testing::manifest("web", "1.0.0"),
    )
    .expect("write");
    fs::set_permissions(
        dir.path().join("manifest.toml"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("mode");
    let manifest = manifest(dir.path(), &expected(), &OWN).expect("a manifest");
    assert_eq!(manifest.hook.command, vec!["bin/hook", "update-hook"]);
}

/// Proves a `target.json` that does not read is `Unreadable` with its text, and is not replaced
/// by a `manifest.toml` beside it.
#[test]
fn an_unreadable_target_json_is_an_unreadable_manifest() {
    let dir = scratch("target-unreadable");
    fs::write(dir.path().join(TARGET_FILE), b"{").expect("write");
    fs::write(
        dir.path().join("manifest.toml"),
        crate::testing::manifest("web", "1.0.0"),
    )
    .expect("write");
    let refusal = manifest(dir.path(), &expected(), &OWN).expect_err("unreadable");
    let ManifestRefusal::Unreadable { why } = refusal else {
        panic!("not unreadable: {refusal:?}");
    };
    assert!(why.starts_with("target.json is not an entry: "), "{why}");
}

/// Proves an entry with no manifest is refused naming `manifest`.
#[test]
fn an_entry_without_a_manifest_is_refused() {
    let entry = Target {
        manifest: None,
        ..entry()
    };
    assert_eq!(
        of(Path::new("/nonexistent"), &entry),
        Err(invalid("manifest", "is missing from the entry"))
    );
}

/// Proves a bound of 0 is refused for each step but `prepare` (unbounded) and `uninstall` (no
/// step), naming the dotted key.
#[test]
fn a_zero_bound_is_refused_except_for_prepare_and_uninstall() {
    let dir = Path::new("/nonexistent");
    for (key, entry) in [
        ("cutover", with_bounds(|b| b.cutover = 0)),
        ("verify", with_bounds(|b| b.verify = 0)),
        ("rollback", with_bounds(|b| b.rollback = 0)),
        ("cleanup", with_bounds(|b| b.cleanup = 0)),
    ] {
        assert_eq!(
            of(dir, &entry),
            Err(invalid(&format!("hook.timeout_secs.{key}"), "is 0"))
        );
    }
    let open = with_bounds(|b| {
        b.prepare = 0;
        b.uninstall = Some(0);
    });
    let timeouts = of(dir, &open).expect("accepted").hook.timeout_secs;
    assert_eq!(timeouts.prepare, None);
    assert_eq!(timeouts.uninstall, None);
    let none = with_bounds(|b| b.uninstall = None);
    assert_eq!(
        of(dir, &none)
            .expect("accepted")
            .hook
            .timeout_secs
            .uninstall,
        None
    );
    let kept = of(dir, &entry()).expect("accepted").hook.timeout_secs;
    assert_eq!(kept.prepare.map(|n| n.get()), Some(60));
    assert_eq!(kept.uninstall.map(|n| n.get()), Some(65));
}

/// Proves an entry whose manifest has no hook at all has zero bounds, so its cutover bound is
/// refused.
#[test]
fn an_entry_without_a_hook_has_no_bounds_and_is_refused() {
    let entry = with_manifest(|manifest| manifest.hook = None);
    assert_eq!(
        of(Path::new("/nonexistent"), &entry),
        Err(invalid("hook.timeout_secs.cutover", "is 0"))
    );
    let no_bounds = with_manifest(|manifest| {
        manifest.hook.as_mut().expect("a hook").timeout_secs = None;
    });
    assert_eq!(
        of(Path::new("/nonexistent"), &no_bounds),
        Err(invalid("hook.timeout_secs.cutover", "is 0"))
    );
}

/// Proves the hook is the package's program at `tree/usr/lib/<package>/hostkeeper-hook` when that
/// is a regular file, and no command (the built-in steps) when it is absent, a link or a
/// directory, or when the entry names no package.
#[test]
fn the_hook_is_the_packages_program_only_when_it_is_a_regular_file() {
    let dir = scratch("target-hook");
    let hook = dir.path().join("tree/usr/lib/web");
    fs::create_dir_all(&hook).expect("tree");
    let command = |entry: &Target| of(dir.path(), entry).expect("accepted").hook.command;
    assert!(command(&entry()).is_empty(), "no program yet");
    fs::write(hook.join("hostkeeper-hook"), b"#!/bin/sh\n").expect("hook");
    assert_eq!(
        command(&entry()),
        vec!["tree/usr/lib/web/hostkeeper-hook".to_owned()]
    );
    let nameless = with_manifest(|manifest| manifest.package = None);
    assert!(command(&nameless).is_empty());
    let other = with_manifest(|manifest| manifest.package = Some("other".to_owned()));
    assert!(command(&other).is_empty());
    fs::remove_file(hook.join("hostkeeper-hook")).expect("remove");
    symlink("/bin/true", hook.join("hostkeeper-hook")).expect("link");
    assert!(command(&entry()).is_empty(), "a link is no program");
    fs::remove_file(hook.join("hostkeeper-hook")).expect("remove");
    fs::create_dir(hook.join("hostkeeper-hook")).expect("directory");
    assert!(command(&entry()).is_empty(), "a directory is no program");
}

/// Proves each kind of param becomes the TOML scalar of the same type, and a param with no value
/// refuses the entry naming `params`.
#[test]
fn params_become_toml_scalars_and_one_without_a_value_is_refused() {
    let param = |value| Param { value: Some(value) };
    let entry = with_manifest(|manifest| {
        manifest.params.insert(
            "text".to_owned(),
            param(param::Value::Text("hi".to_owned())),
        );
        manifest
            .params
            .insert("integer".to_owned(), param(param::Value::Integer(-7)));
        manifest
            .params
            .insert("float".to_owned(), param(param::Value::Float(1.5)));
        manifest
            .params
            .insert("flag".to_owned(), param(param::Value::Flag(true)));
    });
    let params = of(Path::new("/nonexistent"), &entry)
        .expect("accepted")
        .params;
    assert_eq!(params["text"], toml::Value::String("hi".to_owned()));
    assert_eq!(params["integer"], toml::Value::Integer(-7));
    assert_eq!(params["float"], toml::Value::Float(1.5));
    assert_eq!(params["flag"], toml::Value::Boolean(true));
    assert_eq!(params.len(), 4);
    let empty = with_manifest(|manifest| {
        manifest
            .params
            .insert("none".to_owned(), Param { value: None });
    });
    assert_eq!(
        of(Path::new("/nonexistent"), &empty),
        Err(invalid("params", "holds a param with no value"))
    );
}

/// Proves `min_hostkeeper` that does not parse is refused naming the key, one above the daemon's
/// version is `DaemonTooOld`, and one equal to it is accepted (the boundary).
#[test]
fn min_hostkeeper_is_judged_against_the_daemons_version() {
    let dir = Path::new("/nonexistent");
    let min = |text: &str| with_manifest(|manifest| manifest.min_hostkeeper = text.to_owned());
    let refusal = of(dir, &min("1.x")).expect_err("not a version");
    assert!(
        matches!(&refusal, ManifestRefusal::Invalid { key, .. } if key == "min_hostkeeper"),
        "{refusal:?}"
    );
    assert_eq!(
        of(dir, &min("0.1.71")),
        Err(ManifestRefusal::DaemonTooOld {
            needs: HostkeeperVersion {
                major: 0,
                minor: 1,
                patch: 71
            },
            running: OWN,
        })
    );
    assert_eq!(of(dir, &min("0.1.70")).expect("equal").min_hostkeeper, OWN);
}

/// Proves the hook's own settings carry over: the boot step, the verify deadline, the promotion
/// ceiling (0 is none), and the cleanup retries (3 when the entry does not say).
#[test]
fn the_hooks_boot_deadline_ceiling_and_retries_carry_over() {
    let dir = Path::new("/nonexistent");
    let plain = of(dir, &entry()).expect("accepted").hook;
    assert_eq!(plain.boot, None);
    assert_eq!(plain.promotion_ceiling_secs, None);
    assert_eq!(plain.cleanup_retries, DEFAULT_CLEANUP_RETRIES);
    let set = with_manifest(|manifest| {
        let hook = manifest.hook.as_mut().expect("a hook");
        hook.boot = Some("boot".to_owned());
        hook.promotion_ceiling_secs = Some(900);
        hook.cleanup_retries = Some(0);
    });
    let hook = of(dir, &set).expect("accepted").hook;
    assert_eq!(hook.boot.as_deref(), Some("boot"));
    assert_eq!(hook.promotion_ceiling_secs.map(|n| n.get()), Some(900));
    assert_eq!(hook.cleanup_retries, 0);
    let zero = with_manifest(|manifest| {
        manifest
            .hook
            .as_mut()
            .expect("a hook")
            .promotion_ceiling_secs = Some(0);
    });
    assert_eq!(
        of(dir, &zero)
            .expect("accepted")
            .hook
            .promotion_ceiling_secs,
        None
    );
}

/// Proves a `target.json` that cannot be opened for a reason other than being absent (here its
/// directory is a file) is an I/O error, not "no entry".
#[test]
fn a_target_json_under_a_file_is_an_io_error_not_no_entry() {
    let dir = scratch("target-under-file");
    let file = dir.path().join("plain");
    fs::write(&file, b"x").expect("write");
    assert!(matches!(read(&file), Err(TargetError::Io(_))));
}
