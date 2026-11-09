//! `prepare` of a native package: the daemon's version directory holds the package's tree in
//! `tree/` (docs/design/packages-native.md section 10.2), so the staged program is the one the
//! platform's package installs, `tree/usr/lib/hello/hello` for the `.deb` and
//! `tree/Library/Application Support/hello/bin/hello` for the `.pkg`; an unpacked archive keeps
//! its `bin/hello`.

mod support;

use std::fs;
use std::path::Path;

use hello::hook::{DEB_PROGRAM, PKG_PROGRAM, Units, staged_program};
use hostkeeper_hook::Step;
use support::{FakeProbe, FakeRunner, TARGET, World};

/// The world's staged release turned into a native version directory: `bin/` gone, and the
/// program at `relative` under `tree/` (or no program when `relative` is `None`).
fn native(world: &World, relative: Option<&str>) {
    let artifact = world.artifact();
    fs::remove_dir_all(artifact.join("bin")).expect("remove the archive's bin");
    fs::create_dir_all(artifact.join("tree")).expect("the tree");
    if let Some(relative) = relative {
        let program = artifact.join("tree").join(relative);
        fs::create_dir_all(program.parent().expect("a parent")).expect("its directory");
        fs::write(program, "#!/bin/sh\n").expect("the program");
    }
}

/// Catches a hook that looks for `bin/hello` in a native version directory, or for the other
/// platform's path: each platform's `prepare` succeeds with its own package's program in the
/// tree.
#[test]
fn prepare_of_a_native_package_takes_the_program_of_the_tree() {
    for (units, relative, name) in [
        (Units::Systemd, DEB_PROGRAM, "native-deb"),
        (Units::Launchd, PKG_PROGRAM, "native-pkg"),
    ] {
        let world = World::new(name, units);
        native(&world, Some(relative));
        let ran = world.step(
            Step::Prepare,
            &FakeRunner::default(),
            &FakeProbe::ready(TARGET),
        );
        assert_eq!(ran.code, 0, "{units:?}: {ran:?}");
    }
}

/// Catches a native version directory accepted without its program, or with the program at
/// the other platform's path: `prepare` fails `bad_artifact`, naming the path it looked at.
#[test]
fn prepare_of_a_native_package_without_its_program_is_bad_artifact() {
    for (units, other, name) in [
        (Units::Systemd, PKG_PROGRAM, "native-deb-missing"),
        (Units::Launchd, DEB_PROGRAM, "native-pkg-missing"),
    ] {
        let world = World::new(name, units);
        native(&world, Some(other));
        let ran = world.step(
            Step::Prepare,
            &FakeRunner::default(),
            &FakeProbe::ready(TARGET),
        );
        assert_eq!(ran.code, 1, "{units:?}: {ran:?}");
        let looked = staged_program(&world.artifact(), units);
        assert!(ran.stdout.contains("bad_artifact"), "{ran:?}");
        assert!(
            ran.stdout.contains(&format!(
                "{} is not a file: the staged release has no program",
                looked.display()
            )),
            "{ran:?}"
        );
    }
}

/// Catches the layout taken from anything but the staged release: a directory with no `tree`
/// is an archive, whose program is `bin/hello` whatever the platform.
#[test]
fn the_staged_program_follows_the_version_directory() {
    let dir = Path::new("/v/1.2.3");
    for units in [Units::Systemd, Units::Launchd] {
        assert_eq!(staged_program(dir, units), dir.join("bin/hello"));
    }
    assert_eq!(DEB_PROGRAM, "usr/lib/hello/hello");
    assert_eq!(PKG_PROGRAM, "Library/Application Support/hello/bin/hello");
}
