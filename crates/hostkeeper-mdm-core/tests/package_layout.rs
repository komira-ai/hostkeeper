//! The two builds of the `mdm` package (`package::Layout`): the archive, run through the daemon's
//! link `current`, and the `.deb`, installed in `/usr/lib/mdm` and `/usr/lib/systemd/system/`.
//! Which build the program is follows from the path it runs as; each build fixes `MDM_BIN` to
//! its own binary directory, finds a staged release's binaries where that build stages them,
//! and starts its units' programs from its own directory.

mod common;

use common::example;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::package::{
    BIN, DEB_BIN, FIXED, Layout, PackageError, UNITS, settings, settings_of,
};

/// The example with every key the package fixes left out.
fn without_fixed() -> String {
    example()
        .lines()
        .filter(|l| !FIXED.iter().any(|(k, _)| l.starts_with(&format!("{k}="))))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Catches a program path read as the wrong build: the `.deb`'s program, installed or in a
/// staged release's tree, is the `.deb`'s; the archive's program, a bare name, and paths that
/// only look alike are the archive's.
#[test]
fn the_build_is_the_one_the_programs_path_names() {
    for deb in [
        "/usr/lib/mdm/mdm",
        "/var/lib/updater/state/packages/mdm/1.2.3/tree/usr/lib/mdm/mdm",
    ] {
        assert_eq!(Layout::of_program(deb), Layout::Deb, "{deb}");
    }
    for archive in [
        "/var/lib/hostkeeper/packages/mdm/current/bin/mdm",
        "/var/lib/hostkeeper/packages/mdm/1.2.3/bin/mdm",
        "mdm",
        "./mdm",
        "",
        "/usr/lib/mdm/mdm-other",
        "/usr/lib/mdm/hostkeeper-hook",
        "/usr/lib/mdmx/mdm",
    ] {
        assert_eq!(Layout::of_program(archive), Layout::Archive, "{archive:?}");
    }
}

/// Catches a build that fixes the other build's binary directory: the `.deb` fills in and
/// accepts `/usr/lib/mdm` and refuses the archive's directory, and the archive the reverse;
/// every other fixed key is the same in both.
#[test]
fn each_build_fixes_mdm_bin_to_its_own_binaries() {
    let text = settings_of(&without_fixed(), Layout::Deb).expect("the example fits the .deb");
    let c = MdmConfig::parse(&text).expect("parses");
    assert_eq!(c.bin().as_str(), DEB_BIN);
    assert_eq!(
        MdmConfig::parse(&settings(&without_fixed()).expect("fits"))
            .expect("parses")
            .bin()
            .as_str(),
        BIN
    );

    let with = |bin: &str| format!("{}MDM_BIN={bin}\n", without_fixed());
    assert!(settings_of(&with(DEB_BIN), Layout::Deb).is_ok());
    assert_eq!(
        settings_of(&with(BIN), Layout::Deb),
        Err(PackageError::Fixed {
            key: "MDM_BIN",
            given: BIN.to_owned(),
            fixed: DEB_BIN,
        })
    );
    assert_eq!(
        settings(&with(DEB_BIN)),
        Err(PackageError::Fixed {
            key: "MDM_BIN",
            given: DEB_BIN.to_owned(),
            fixed: BIN,
        })
    );
    let (deb, archive) = (Layout::Deb.fixed(), Layout::Archive.fixed());
    assert_eq!(archive, FIXED);
    for ((key, d), (_, a)) in deb.iter().zip(archive.iter()) {
        if *key == "MDM_BIN" {
            assert_eq!((*d, *a), (DEB_BIN, BIN));
        } else {
            assert_eq!(d, a, "{key}");
        }
    }
}

/// Catches `prepare` looking for a staged release's binaries where its build does not put
/// them: an archive's are in its `bin/`, a `.deb`'s in `tree/usr/lib/mdm` of the daemon's
/// version directory.
#[test]
fn a_staged_releases_binaries_are_where_its_build_stages_them() {
    assert_eq!(Layout::Archive.staged_bin("/v/9"), "/v/9/bin");
    assert_eq!(Layout::Deb.staged_bin("/v/9"), "/v/9/tree/usr/lib/mdm");
}

/// Catches a `.deb` unit that still starts a program through the archive's `current` (which a
/// `.deb` host does not have), or one that differs from the archive's unit in anything but
/// that directory: every program of every `.deb` unit is in `/usr/lib/mdm`, and the archive's
/// units are the committed ones unchanged.
#[test]
fn the_debs_units_start_their_programs_from_usr_lib_mdm() {
    for unit in &UNITS {
        assert_eq!(Layout::Archive.unit_text(unit), unit.text, "{}", unit.name);
        let deb = Layout::Deb.unit_text(unit);
        assert!(
            !deb.contains("/var/lib/hostkeeper"),
            "{} names the archive's directory:\n{deb}",
            unit.name
        );
        assert_eq!(
            deb.replace(&format!("{DEB_BIN}/"), &format!("{BIN}/")),
            unit.text,
            "{}",
            unit.name
        );
        for line in deb.lines() {
            if let Some(command) = line.strip_prefix("ExecStart=") {
                let program = command.split_whitespace().next().unwrap_or_default();
                assert!(
                    program.starts_with(&format!("{DEB_BIN}/")),
                    "{}: {line}",
                    unit.name
                );
            }
        }
    }
}
