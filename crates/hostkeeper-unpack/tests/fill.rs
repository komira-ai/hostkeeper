//! `stage_filled`: a stage the caller fills, published sealed as an unpacked version is.

mod support;

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_steps::{Layout, Seal, read_seal, tree_digest};
use hostkeeper_unpack::{FillError, FillOrder, Filled, PackageFs, UnpackError, stage_filled};
use support::{Call, EACCES, Faulty, Matcher, Rig, SERVICE, VERSION, sha256_hex};

const EIO: i32 = 5;

fn digest() -> Sha256Hex {
    Sha256Hex::parse(&sha256_hex(b"the .deb")).unwrap()
}

fn other_digest() -> Sha256Hex {
    Sha256Hex::parse(&sha256_hex(b"another .deb")).unwrap()
}

/// What an installer writes into a stage: `tree/` and `apt/` of plain files and directories,
/// and `target.json`.
fn install_into(stage: &Path) -> Result<(), String> {
    let write = |e: std::io::Error| e.to_string();
    fs::create_dir_all(stage.join("tree/usr/bin")).map_err(write)?;
    fs::create_dir(stage.join("apt")).map_err(write)?;
    fs::write(stage.join("tree/usr/bin/tool"), "#!tool\n").map_err(write)?;
    fs::set_permissions(
        stage.join("tree/usr/bin/tool"),
        fs::Permissions::from_mode(0o755),
    )
    .map_err(write)?;
    fs::write(stage.join("apt/control"), "Package: tool\n").map_err(write)?;
    fs::write(stage.join("target.json"), "{}\n").map_err(write)
}

/// What a call did: how often `same` and `fill` ran.
struct Run {
    result: Result<Filled, FillError<String>>,
    same: usize,
    fill: usize,
}

fn call_with(
    fake: &dyn PackageFs,
    version: &str,
    package: &Sha256Hex,
    same_says: Option<&str>,
    fill_with: &dyn Fn(&Path) -> Result<(), String>,
) -> Run {
    let service = ServiceName::new(SERVICE).unwrap();
    let version = Version::new(version).unwrap();
    let order = FillOrder {
        service: &service,
        version: &version,
        package_digest: package,
    };
    let (same_calls, fill_calls) = (Cell::new(0), Cell::new(0));
    let mut same = |_: &Path| {
        same_calls.set(same_calls.get() + 1);
        same_says.map_or(Ok(()), |why| Err(why.to_owned()))
    };
    let mut fill = |stage: &Path| {
        fill_calls.set(fill_calls.get() + 1);
        fill_with(stage)
    };
    let result = stage_filled(&order, fake, &mut same, &mut fill);
    Run {
        result,
        same: same_calls.get(),
        fill: fill_calls.get(),
    }
}

fn call(fake: &dyn PackageFs, version: &str, package: &Sha256Hex) -> Run {
    call_with(fake, version, package, None, &install_into)
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn unpack_error(run: Run) -> UnpackError {
    match run.result {
        Err(FillError::Unpack(e)) => e,
        other => panic!("not a core failure: {other:?}"),
    }
}

fn published(_rig: &Rig) -> [String; 2] {
    [VERSION.to_owned(), format!("{VERSION}.seal")]
}

/// Catches a fill published without the seal an unpack makes, or not read-only: the version is
/// `fresh`, its directory is the real path of `packages/<service>/<version>` at mode 0555, the
/// seal records the given package digest and the digest of the tree as walked, the step runner's
/// check of the version passes, and the stage is gone.
#[test]
fn a_fill_is_published_sealed_with_the_given_package_digest_and_the_walked_tree_digest() {
    let rig = Rig::new("fill-publish");
    let run = call(&rig.host(), VERSION, &digest());
    assert_eq!(run.fill, 1);
    assert_eq!(run.same, 0);
    let filled = run.result.unwrap();
    assert!(filled.fresh);
    let dir = rig.service().join(VERSION);
    assert_eq!(filled.dir, dir);
    assert_eq!(mode(&dir), 0o555);
    assert_eq!(rig.listing(), published(&rig));
    let layout = Layout::package(&rig.service());
    let seal = Seal::parse(&read_seal(&layout.seal_path(VERSION)).unwrap()).unwrap();
    assert_eq!(seal.package_digest(), digest().as_str());
    assert_eq!(seal.tree_digest(), tree_digest(&dir).unwrap());
    assert_eq!(mode(&rig.service().join(format!("{VERSION}.seal"))), 0o444);
    layout.check_tree(VERSION).unwrap();
    assert!(dir.join("tree/usr/bin/tool").is_file() && dir.join("target.json").is_file());
}

/// Catches a second call that fills again or unpacks a manifest: with the same digest it calls
/// `same` with the version directory, not `fill`, writes nothing through the port, and returns
/// `fresh: false`.
#[test]
fn a_second_call_with_the_same_digest_checks_the_version_and_does_not_fill() {
    let rig = Rig::new("fill-again");
    let first = call(&rig.host(), VERSION, &digest()).result.unwrap();
    let fake = Faulty::new(rig.host());
    let service = ServiceName::new(SERVICE).unwrap();
    let version = Version::new(VERSION).unwrap();
    let package = digest();
    let order = FillOrder {
        service: &service,
        version: &version,
        package_digest: &package,
    };
    let seen = Cell::new(None);
    let mut same = |dir: &Path| -> Result<(), String> {
        seen.set(Some(dir.to_path_buf()));
        Ok(())
    };
    let mut fill = |_: &Path| -> Result<(), String> { panic!("filled a version that is there") };
    let again = stage_filled(&order, &fake, &mut same, &mut fill).unwrap();
    assert!(!again.fresh);
    assert_eq!(again.dir, first.dir);
    assert_eq!(seen.take(), Some(first.dir));
    assert!(
        fake.calls().iter().all(|c| !c.writes()),
        "{:?}",
        fake.calls()
    );
    assert_eq!(rig.listing(), published(&rig));
}

/// Catches a refusing `same` that is taken for success or that changes the version: its error
/// comes back as `Fill(e)` unchanged, and the version, its seal and their modes are as they
/// were.
#[test]
fn a_same_check_that_refuses_is_the_callers_error_and_changes_nothing() {
    let rig = Rig::new("fill-same-refuses");
    call(&rig.host(), VERSION, &digest()).result.unwrap();
    let dir = rig.service().join(VERSION);
    let before = tree_digest(&dir).unwrap();
    let run = call_with(
        &rig.host(),
        VERSION,
        &digest(),
        Some("the files differ"),
        &install_into,
    );
    assert_eq!((run.same, run.fill), (1, 0));
    match run.result {
        Err(FillError::Fill(why)) => assert_eq!(why, "the files differ"),
        other => panic!("{other:?}"),
    }
    assert_eq!(tree_digest(&dir).unwrap(), before);
    assert_eq!(mode(&dir), 0o555);
    assert_eq!(rig.listing(), published(&rig));
}

/// Catches a version sealed for one package taken for another: a version sealed for another
/// digest is `ArtifactChanged` naming both digests (`Failed`, release, `artifact_changed`),
/// `same` and `fill` are not called, and the version is untouched.
#[test]
fn a_version_sealed_for_another_digest_is_refused_and_not_checked() {
    let rig = Rig::new("fill-other-digest");
    call(&rig.host(), VERSION, &digest()).result.unwrap();
    let run = call(&rig.host(), VERSION, &other_digest());
    assert_eq!((run.same, run.fill), (0, 0));
    let err = unpack_error(run);
    let UnpackError::ArtifactChanged { version, why } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(version, VERSION);
    assert_eq!(
        why,
        &format!(
            "was sealed for the archive {}, not {}",
            digest().as_str(),
            other_digest().as_str()
        )
    );
    assert_eq!(
        err.outcome(),
        (Outcome::Failure, Class::Release, "artifact_changed")
    );
    assert_eq!(rig.listing(), published(&rig));
}

/// Catches a version with no seal taken as installed: `ArtifactChanged` "has no seal", `same` and
/// `fill` not called, the directory left alone.
#[test]
fn a_version_without_a_seal_is_refused() {
    let rig = Rig::new("fill-no-seal");
    fs::create_dir_all(rig.service().join(VERSION)).unwrap();
    let run = call(&rig.host(), VERSION, &digest());
    assert_eq!((run.same, run.fill), (0, 0));
    let err = unpack_error(run);
    assert!(
        matches!(&err, UnpackError::ArtifactChanged { why, .. } if why == "has no seal"),
        "{err:?}"
    );
    assert_eq!(rig.listing(), [VERSION.to_owned()]);
}

/// Catches an unreadable or foreign seal taken as a match: a seal that is not JSON is
/// `SealUnreadable` (`Deferred`, `io_error`), one of a newer schema is `SealSchema`
/// (`daemon_too_old`), and `same` is not called for either.
#[test]
fn a_seal_that_cannot_be_read_or_is_of_a_newer_schema_is_refused() {
    let rig = Rig::new("fill-bad-seal");
    fs::create_dir_all(rig.service().join(VERSION)).unwrap();
    for (text, reason) in [("{", "io_error"), ("{\"schema\":99}", "daemon_too_old")] {
        fs::write(rig.service().join(format!("{VERSION}.seal")), text).unwrap();
        let run = call(&rig.host(), VERSION, &digest());
        assert_eq!((run.same, run.fill), (0, 0), "{text}");
        let err = unpack_error(run);
        assert_eq!(err.outcome().2, reason, "{err:?}");
        fs::remove_file(rig.service().join(format!("{VERSION}.seal"))).unwrap();
        fs::create_dir_all(rig.service().join(VERSION)).unwrap();
    }
}

/// Catches a tree edited after its seal taken as the sealed one: a changed file is
/// `ArtifactChanged` (`artifact_changed`), `same` is not called, and the edit is left in place.
#[test]
fn a_tree_changed_after_its_seal_is_refused() {
    let rig = Rig::new("fill-changed");
    call(&rig.host(), VERSION, &digest()).result.unwrap();
    let file = rig.service().join(VERSION).join("target.json");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&file, "{\"edited\":true}\n").unwrap();
    let run = call(&rig.host(), VERSION, &digest());
    assert_eq!((run.same, run.fill), (0, 0));
    let err = unpack_error(run);
    assert!(
        matches!(&err, UnpackError::ArtifactChanged { .. }),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "{\"edited\":true}\n");
}

/// Catches a version that cannot be looked at, or a tree that cannot be read, taken as absent or
/// as changed: a failing look at the version and a tree with an unreadable directory are
/// `Io` (`io_error`).
#[test]
fn a_version_that_cannot_be_looked_at_or_read_is_an_io_error() {
    let rig = Rig::new("fill-look");
    call(&rig.host(), VERSION, &digest()).result.unwrap();
    let fake = Faulty::new(rig.host()).fail(
        Box::new(|c| matches!(c, Call::Exists(p) if p.ends_with(VERSION))),
        0,
        EIO,
    );
    let err = unpack_error(call(&fake, VERSION, &digest()));
    assert_eq!(err.outcome().2, "io_error", "{err:?}");

    if rustix::process::geteuid().is_root() {
        eprintln!("skipped: root reads a directory whose mode is 0, so none can be unreadable");
        return;
    }
    let tools = rig.service().join(VERSION).join("tree/usr");
    fs::set_permissions(&tools, fs::Permissions::from_mode(0o000)).unwrap();
    let run = call(&rig.host(), VERSION, &digest());
    assert_eq!((run.same, run.fill), (0, 0));
    let err = unpack_error(run);
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot read the version"),
        "{err:?}"
    );
    fs::set_permissions(&tools, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Catches a failed fill that leaves a stage, a seal or a version: whatever the fill wrote
/// before it failed, its error comes back as `Fill(e)` and the service directory is as it was.
#[test]
fn a_fill_that_fails_leaves_no_stage_no_seal_and_no_version() {
    let rig = Rig::new("fill-fails");
    let run = call_with(&rig.host(), VERSION, &digest(), None, &|stage| {
        install_into(stage)?;
        Err("dpkg-deb failed".to_owned())
    });
    assert_eq!((run.same, run.fill), (0, 1));
    match run.result {
        Err(FillError::Fill(why)) => assert_eq!(why, "dpkg-deb failed"),
        other => panic!("{other:?}"),
    }
    assert!(rig.listing().is_empty(), "{:?}", rig.listing());
}

/// The stage is handed to the fill as an empty directory the owner alone can use.
#[test]
fn the_stage_handed_to_the_fill_is_empty_and_private() {
    let rig = Rig::new("fill-stage");
    let run = call_with(&rig.host(), VERSION, &digest(), None, &|stage| {
        assert_eq!(mode(stage), 0o700);
        assert_eq!(fs::read_dir(stage).unwrap().count(), 0);
        assert!(stage.starts_with(&rig.packages));
        install_into(stage)
    });
    run.result.unwrap();
}

/// Catches a failure at a step of the publish that leaves a stage, a temporary seal or a seal
/// without its version: a failure at the fsync of the filled tree's top, of the temporary seal,
/// at the rename of the seal, at the fsync after it and at the rename of the tree gives the
/// outcome of its error and leaves the service directory empty; ENOSPC is `no_space`.
#[test]
fn a_failure_at_each_step_of_the_publish_leaves_the_service_as_it_was() {
    let at: Vec<(&str, Matcher, i32, &str)> = vec![
        (
            "the fsync of the stage",
            Box::new(
                |c| matches!(c, Call::SyncDir(p) if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".stage-"))),
            ),
            EIO,
            "io_error",
        ),
        (
            "the write of the temporary seal",
            Box::new(|c| matches!(c, Call::Write(p, _) if p.ends_with(".1.5.0.seal.tmp"))),
            28,
            "no_space",
        ),
        (
            "the fsync of the temporary seal",
            Box::new(|c| matches!(c, Call::FileSync(p) if p.ends_with(".1.5.0.seal.tmp"))),
            EIO,
            "io_error",
        ),
        (
            "the rename of the seal",
            Box::new(|c| matches!(c, Call::Rename(p, _) if p.ends_with(".1.5.0.seal.tmp"))),
            EIO,
            "io_error",
        ),
        (
            "the fsync after the seal's rename",
            Box::new(|c| matches!(c, Call::SyncDir(p) if p == Path::new(SERVICE))),
            EIO,
            "io_error",
        ),
        (
            "the rename of the tree",
            Box::new(|c| matches!(c, Call::Rename(_, to) if to.ends_with(VERSION))),
            EIO,
            "io_error",
        ),
    ];
    for (what, matcher, errno, reason) in at {
        let rig = Rig::new("fill-publish-fails");
        let fake = Faulty::new(rig.host()).fail(matcher, 0, errno);
        let err = unpack_error(call(&fake, VERSION, &digest()));
        assert_eq!(err.outcome().2, reason, "{what}: {err:?}");
        assert!(rig.listing().is_empty(), "{what}: {:?}", rig.listing());
    }
}

/// Catches a version published by a failed call being removed, or left 0700 for good: a failed
/// mode or fsync of the version after its rename leaves the version and its seal (the error is
/// `io_error`), and the next call finds it sealed, makes it 0555 and returns `fresh: false`
/// after `same`, without filling.
#[test]
fn a_version_left_writable_by_a_crash_is_made_read_only_by_the_next_call() {
    for what in ["mode", "fsync"] {
        let rig = Rig::new("fill-settle");
        let matcher: Matcher = if what == "mode" {
            Box::new(|c| matches!(c, Call::SetMode(p, _) if p.ends_with(VERSION)))
        } else {
            Box::new(|c| matches!(c, Call::SyncDir(p) if p.ends_with(VERSION)))
        };
        let fake = Faulty::new(rig.host()).fail(matcher, 0, EIO);
        let err = unpack_error(call(&fake, VERSION, &digest()));
        assert_eq!(err.outcome().2, "io_error", "{what}");
        assert_eq!(rig.listing(), published(&rig), "{what}");
        let dir = rig.service().join(VERSION);
        assert_eq!(mode(&dir), if what == "mode" { 0o700 } else { 0o555 });

        let run = call(&rig.host(), VERSION, &digest());
        assert_eq!((run.same, run.fill), (1, 0), "{what}");
        assert!(!run.result.unwrap().fresh);
        assert_eq!(mode(&dir), 0o555, "{what}");
        Layout::package(&rig.service()).check_tree(VERSION).unwrap();
    }
}

/// Catches a crash that leaves a state the next call cannot finish: a crash at every call of
/// the port, in turn, leaves nothing but orphans or a whole sealed version, and the next call on
/// a healthy port ends with the version sealed, 0555, and passing the step runner's check.
#[test]
fn a_crash_at_any_call_is_finished_by_the_next_call() {
    let probe = Rig::new("fill-crash-probe");
    let recorder = Faulty::new(probe.host());
    call(&recorder, VERSION, &digest()).result.unwrap();
    let total = recorder.calls().len();
    assert!(total > 10, "{total}");
    for at in 0..total {
        let rig = Rig::new("fill-crash");
        let fake = Faulty::new(rig.host()).crash_at(at);
        let first = call(&fake, VERSION, &digest());
        assert!(first.result.is_err(), "call {at}");
        let next = call(&rig.host(), VERSION, &digest()).result.unwrap();
        assert_eq!(next.dir, rig.service().join(VERSION), "call {at}");
        assert_eq!(rig.listing(), published(&rig), "call {at}");
        assert_eq!(mode(&next.dir), 0o555, "call {at}");
        Layout::package(&rig.service()).check_tree(VERSION).unwrap();
    }
}

/// Catches a name the package directory keeps for itself reaching the disk: `current`,
/// `previous`, a `.seal` and a `.tmp` name are `ReservedVersion` (`reserved_version`), 246 and 255
/// bytes are `VersionTooLong` (`version_too_long`), and neither `fill` nor any call that
/// creates or writes is made.
#[test]
fn a_reserved_or_too_long_version_is_refused_before_anything_is_made() {
    let rig = Rig::new("fill-names");
    let long = ["1".repeat(246), "1".repeat(255)];
    for version in [
        "current",
        "previous",
        "1.4.2.seal",
        "x.tmp",
        &long[0],
        &long[1],
    ] {
        let fake = Faulty::new(rig.host());
        let run = call(&fake, version, &digest());
        assert_eq!((run.same, run.fill), (0, 0), "{version}");
        let err = unpack_error(run);
        let reason = err.outcome().2;
        assert_eq!(
            reason,
            if version.len() > 100 {
                "version_too_long"
            } else {
                "reserved_version"
            }
        );
        assert!(fake.calls().is_empty(), "{version}: {:?}", fake.calls());
    }
    assert!(!rig.service().exists());
}

/// Catches orphans surviving into a publish: a `.stage-*` directory with read-only contents, a
/// `.<version>.seal.tmp` and a seal without its version are removed before the fill runs; a
/// sealed version, a note and another service's orphan are kept.
#[test]
fn orphans_of_the_service_are_removed_before_the_fill() {
    let rig = Rig::new("fill-orphans");
    let service = rig.service();
    fs::create_dir_all(service.join(".stage-old/bin")).unwrap();
    fs::write(service.join(".stage-old/bin/half"), "half").unwrap();
    fs::set_permissions(
        service.join(".stage-old/bin"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    fs::write(service.join(format!(".{VERSION}.seal.tmp")), "{").unwrap();
    fs::write(service.join("9.9.9.seal"), "{}").unwrap();
    fs::write(service.join("notes"), "kept").unwrap();
    let other = rig.packages.join("other");
    fs::create_dir_all(other.join(".stage-z")).unwrap();

    let listed = Cell::new(Vec::new());
    let run = call_with(&rig.host(), VERSION, &digest(), None, &|stage| {
        listed.set(
            fs::read_dir(stage.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect(),
        );
        install_into(stage)
    });
    run.result.unwrap();
    let mut during = listed.take();
    during.sort();
    assert_eq!(during.len(), 2, "{during:?}");
    assert!(during.contains(&"notes".to_owned()));
    assert!(
        during
            .iter()
            .any(|n| n.starts_with(".stage-") && n != ".stage-old")
    );
    assert_eq!(
        rig.listing(),
        ["1.5.0", "1.5.0.seal", "notes"].map(str::to_owned)
    );
    assert!(other.join(".stage-z").is_dir());
}

/// Catches a service directory that cannot be made, locked or cleaned, or a stage that cannot be
/// made, taken as success: each is `Io` (`io_error`) and nothing is filled.
#[test]
fn a_service_or_stage_that_cannot_be_made_stops_the_work() {
    let at: Vec<Matcher> = vec![
        Box::new(|c| matches!(c, Call::CreateService(_))),
        Box::new(|c| matches!(c, Call::Lock(_))),
        Box::new(|c| matches!(c, Call::ReadDir(_))),
        Box::new(|c| matches!(c, Call::CreateStage(_))),
    ];
    for matcher in at {
        let rig = Rig::new("fill-start");
        let fake = Faulty::new(rig.host()).fail(matcher, 0, EACCES);
        let run = call(&fake, VERSION, &digest());
        assert_eq!(run.fill, 0);
        let err = unpack_error(run);
        assert_eq!(
            err.outcome(),
            (Outcome::Deferred, Class::Environment, "io_error")
        );
        assert!(rig.listing().is_empty());
    }
}

/// Catches a tree the digest cannot cover being sealed: a fifo in the filled tree is `Io`
/// (`io_error`) naming the entry, a stage with a directory that cannot be read is `Io` at the
/// fsync, and a directory that becomes unreadable between the fsync and the digest is `Io` at
/// the digest; none leaves a stage, a seal or a version.
#[test]
fn a_filled_tree_that_cannot_be_synced_or_digested_is_refused_and_cleaned_up() {
    let rig = Rig::new("fill-tree");
    let run = call_with(&rig.host(), VERSION, &digest(), None, &|stage| {
        install_into(stage)?;
        hostkeeper_testkit::fifo(&stage.join("tree/pipe"), 0o644).map_err(|e| e.to_string())
    });
    let err = unpack_error(run);
    assert!(
        matches!(&err, UnpackError::Io { what, source } if *what == "cannot read the filled tree" && source.to_string().contains("neither a regular file nor a directory")),
        "{err:?}"
    );
    assert_eq!(err.outcome().2, "io_error");
    assert!(rig.listing().is_empty());

    if rustix::process::geteuid().is_root() {
        eprintln!("skipped: root reads a directory whose mode is 0, so none can be unreadable");
        return;
    }
    let rig = Rig::new("fill-tree-unreadable");
    let run = call_with(&rig.host(), VERSION, &digest(), None, &|stage| {
        install_into(stage)?;
        fs::set_permissions(stage.join("apt"), fs::Permissions::from_mode(0o000))
            .map_err(|e| e.to_string())
    });
    let err = unpack_error(run);
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot make the filled tree durable"),
        "{err:?}"
    );
    only_the_stage_remains(&rig);

    let rig = Rig::new("fill-tree-digest");
    let packages = rig.packages.clone();
    let fake = Faulty::new(rig.host()).at(
        Box::new(|c| matches!(c, Call::SyncDir(p) if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".stage-")))),
        move |c| {
            let Call::SyncDir(stage) = c else { return };
            let real = packages.join(stage);
            fs::set_permissions(real.join("apt"), fs::Permissions::from_mode(0o000)).unwrap();
        },
    );
    let err = unpack_error(call(&fake, VERSION, &digest()));
    assert!(
        matches!(&err, UnpackError::Io { what, .. } if *what == "cannot read the filled tree"),
        "{err:?}"
    );
    only_the_stage_remains(&rig);
}

/// A directory whose mode is 0 cannot be opened, so the port cannot remove it: the stage stays,
/// for the orphan removal of a later call, and no seal or version does.
fn only_the_stage_remains(rig: &Rig) {
    let left = rig.listing();
    assert!(
        left.iter().all(|name| name.starts_with(".stage-")),
        "{left:?}"
    );
}

/// `FillError` says its cause in its own words, for either side.
#[test]
fn a_fill_error_displays_the_error_of_either_side() {
    let rig = Rig::new("fill-display");
    let fill: FillError<String> = FillError::Fill("no network".to_owned());
    assert_eq!(fill.to_string(), "no network");
    let err = unpack_error(call(
        &Faulty::new(rig.host()).fail(Box::new(|c| matches!(c, Call::CreateService(_))), 0, EIO),
        VERSION,
        &digest(),
    ));
    let text = err.to_string();
    assert_eq!(FillError::<String>::Unpack(err).to_string(), text);
}
