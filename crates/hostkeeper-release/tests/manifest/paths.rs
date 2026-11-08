//! `run` names and `place` entries: units of the service, by today's rules (`names.rs`), and
//! paths outside the prefixes hostkeeper and the system keep.

use hostkeeper_config::manifest::REFUSED_PREFIXES;
use hostkeeper_config::names::{PathError, UnitError};
use hostkeeper_proto::hostkeeper::release::v1::{Manifest, Place};
use hostkeeper_release::Installer;
use hostkeeper_release::manifest::{self, ManifestError};

use crate::support::{manifest, place, web};

fn with_run(run: &[&str]) -> Manifest {
    Manifest {
        run: run.iter().map(|unit| (*unit).to_owned()).collect(),
        health_url: None,
        ..manifest(Installer::Tarball)
    }
}

fn with_place(places: Vec<Place>) -> Manifest {
    Manifest {
        place: places,
        ..manifest(Installer::Tarball)
    }
}

fn check(manifest: &Manifest) -> Result<(), ManifestError> {
    manifest::check(&web(), manifest)
}

/// Catches: a run unit of another service, of a kind a package may not carry, a template, or
/// with a bad instance passing; a launchd label of another service passing; a unit or label of
/// the service refused.
#[test]
fn a_run_entry_is_a_unit_or_a_label_of_the_service() {
    for good in [
        "web.service",
        "web-cache.timer",
        "web@9301.service",
        "web.socket",
        "web",
        "web.worker",
        // No unit suffix: read as a launchd label, as `[units] run` reads it today.
        "web.mount",
    ] {
        assert_eq!(check(&with_run(&[good])), Ok(()), "{good}");
    }
    let long = format!("web-{}.service", "a".repeat(250));
    for (bad, why) in [
        ("api.service", UnitError::NotTheServices),
        ("webx.service", UnitError::NotTheServices),
        ("web@.service", UnitError::BadInstance),
        ("web@a b.service", UnitError::BadInstance),
        ("web@a@b.service", UnitError::BadInstance),
        (long.as_str(), UnitError::TooLong),
        (
            "api.worker",
            UnitError::BadLabel(
                "it is neither the service name nor the service name, `.` and more",
            ),
        ),
        ("", UnitError::BadLabel("it is empty")),
    ] {
        assert_eq!(
            check(&with_run(&[bad])),
            Err(ManifestError::RunUnit {
                unit: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        check(&with_run(&["api.service"]))
            .expect_err("refused")
            .to_string(),
        "the run entry \"api.service\" does not begin with the service name, alone or followed by `-`"
    );
}

/// Catches: a place source that leaves the tree or names an absolute path passing, a kind other
/// than unit or file passing.
#[test]
fn a_place_source_is_relative_and_a_kind_is_unit_or_file() {
    for (bad, why) in [
        ("../etc/shadow", PathError::DotComponent),
        ("files/./web.conf", PathError::DotComponent),
        ("/etc/passwd", PathError::EmptyComponent),
        ("", PathError::Empty),
    ] {
        assert_eq!(
            check(&with_place(vec![place("file", bad, "/etc/web/web.conf")])),
            Err(ManifestError::PlaceSource {
                entry: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        check(&with_place(vec![place("link", "files/a", "/etc/web/a")])),
        Err(ManifestError::PlaceKind {
            entry: "files/a".to_owned(),
            kind: "link".to_owned()
        })
    );
    assert_eq!(
        ManifestError::PlaceKind {
            entry: "files/a".to_owned(),
            kind: "link".to_owned()
        }
        .to_string(),
        "the place entry \"files/a\" has the kind \"link\", neither unit nor file"
    );
    assert_eq!(
        ManifestError::PlaceSource {
            entry: "../x".to_owned(),
            why: PathError::DotComponent
        }
        .to_string(),
        "the place source \"../x\" has a `.` or `..` component"
    );
}

/// Catches: a placed unit of another service, without a suffix, or with a bad instance passing;
/// a template or a plist of the service refused.
#[test]
fn a_placed_unit_is_a_unit_file_of_the_service() {
    for good in [
        "web.service",
        "web@.service",
        "web-cache.timer",
        "web.plist",
        "web.worker.plist",
    ] {
        assert_eq!(
            check(&with_place(vec![place("unit", "units/u", good)])),
            Ok(()),
            "{good}"
        );
    }
    for (bad, why) in [
        ("api.service", UnitError::NotTheServices),
        ("web", UnitError::NotAUnit),
        ("web@a/b.service", UnitError::BadInstance),
        (
            "api.plist",
            UnitError::BadLabel(
                "it is neither the service name nor the service name, `.` and more",
            ),
        ),
    ] {
        assert_eq!(
            check(&with_place(vec![place("unit", "units/u", bad)])),
            Err(ManifestError::PlaceUnit {
                path: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        ManifestError::PlaceUnit {
            path: "web".to_owned(),
            why: UnitError::NotAUnit
        }
        .to_string(),
        "the unit place path \"web\" is not a unit of a kind a package may carry (`.service`, `.socket`, `.timer`)"
    );
}

/// Catches: a placed file under any refused prefix, or at one, passing; a path beside a prefix
/// (`/usrx`, not `/usr`) refused; a path that is not absolute passing.
#[test]
fn a_placed_file_is_absolute_and_outside_every_refused_prefix() {
    for prefix in REFUSED_PREFIXES {
        for path in [prefix.to_owned(), format!("{prefix}/web/a.conf")] {
            assert_eq!(
                check(&with_place(vec![place("file", "files/a", &path)])),
                Err(ManifestError::ReservedPath {
                    path: path.clone(),
                    // `/var/lib/hostkeeper` and `/var/lib/updater` come before `/var/lib`.
                    prefix,
                }),
                "{path}"
            );
        }
        let beside = format!("{prefix}x/a.conf");
        // Beside `/var/lib/hostkeeper` is still below `/var/lib`.
        let want = match beside.starts_with("/var/lib/") {
            true => Err(ManifestError::ReservedPath {
                path: beside.clone(),
                prefix: "/var/lib",
            }),
            false => Ok(()),
        };
        assert_eq!(
            check(&with_place(vec![place("file", "files/a", &beside)])),
            want,
            "{beside}"
        );
    }
    assert_eq!(
        check(&with_place(vec![place(
            "file",
            "files/a",
            "/var/lib/web/a"
        )])),
        Err(ManifestError::ReservedPath {
            path: "/var/lib/web/a".to_owned(),
            prefix: "/var/lib"
        })
    );
    assert_eq!(
        ManifestError::ReservedPath {
            path: "/usr/bin/web".to_owned(),
            prefix: "/usr"
        }
        .to_string(),
        "the file place path \"/usr/bin/web\" is /usr or below it, where a package may not place a file"
    );
    for (bad, why) in [
        ("etc/web/a.conf", PathError::NotAbsolute),
        ("/etc/web/../a", PathError::DotComponent),
        ("-/etc/a", PathError::SystemdPrefix('-')),
    ] {
        assert_eq!(
            check(&with_place(vec![place("file", "files/a", bad)])),
            Err(ManifestError::PlacePath {
                path: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        ManifestError::PlacePath {
            path: "etc".to_owned(),
            why: PathError::NotAbsolute
        }
        .to_string(),
        "the file place path \"etc\" is not absolute"
    );
}

/// Catches: two place entries of one path passing, which would link one path twice.
#[test]
fn two_place_entries_of_one_path_are_refused() {
    let twice = with_place(vec![
        place("file", "files/a", "/etc/web/a.conf"),
        place("file", "files/b", "/etc/web/a.conf"),
    ]);
    assert_eq!(
        check(&twice),
        Err(ManifestError::DuplicatePlace("/etc/web/a.conf".to_owned()))
    );
    let units = with_place(vec![
        place("unit", "units/a", "web.service"),
        place("unit", "units/b", "web.service"),
    ]);
    assert_eq!(
        check(&units),
        Err(ManifestError::DuplicatePlace("web.service".to_owned()))
    );
    assert_eq!(
        ManifestError::DuplicatePlace("web.service".to_owned()).to_string(),
        "two place entries have the path \"web.service\""
    );
}
