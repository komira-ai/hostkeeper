//! The check that systemd loads the package's own unit after a native install over an archive
//! version (packages-native.md section 10.6; packages-repositories.md section 4.5,
//! `unit_not_loaded`), against the fake's `systemctl show`, which reads the unit directory with a
//! link's semantics: a link there comes first, and one whose target is gone is no unit.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_install::repository::loaded::{SHOW_PROPERTIES, UnitLoad, check_units, parse_show};
use hostkeeper_install::repository::{Class, Failure, Outcome, Reason, Tool, ToolOutput};

use crate::apt_fake::{Deb, Events, FakeHost};
use crate::support::Scratch;

const UNIT: &str = "hostkeeper.service";
const PACKAGE_UNIT: &str = "usr/lib/systemd/system/hostkeeper.service";
const UNIT_DIR: &str = "/etc/systemd/system";
const BOUND: u64 = 30;

/// A host whose package `hostkeeper` is installed from a deb holding `PACKAGE_UNIT`, with its
/// extracted tree, and whose unit directory the fake reads.
struct Host {
    scratch: Scratch,
    fake: FakeHost,
}

impl Host {
    fn new(name: &str) -> Host {
        let scratch = Scratch::new(&format!("loaded-{name}"));
        let fake = FakeHost::new(&scratch.path().join("installer.lock"), Events::default());
        let deb = Deb::new("hostkeeper", "0.1.246", "amd64").file(PACKAGE_UNIT, 0o644, "[Unit]\n");
        fake.edit(|s| s.debs.push(deb));
        fake.set_installed("hostkeeper", "0.1.246", 'i', true);
        fs::create_dir_all(scratch.path().join("etc")).expect("the unit directory");
        fake.watch_units(&scratch.path().join("etc"), UNIT_DIR);
        let unit = scratch.path().join("tree").join(PACKAGE_UNIT);
        fs::create_dir_all(unit.parent().expect("a parent")).expect("the tree");
        fs::write(unit, "[Unit]\n").expect("the tree's unit");
        Host { scratch, fake }
    }

    fn tree(&self) -> PathBuf {
        self.scratch.path().join("tree")
    }

    /// The unit directory's entry `name` on disk.
    fn at(&self, name: &str) -> PathBuf {
        self.scratch.path().join("etc").join(name)
    }

    fn check(&self, units: &[&str]) -> Result<(), Failure> {
        let units: Vec<String> = units.iter().map(|u| (*u).to_owned()).collect();
        check_units(&self.fake, &self.tree(), &units, BOUND)
    }

    /// The units `systemctl show` was asked about, in order.
    fn shown(&self) -> Vec<String> {
        self.fake
            .calls_of("systemctl show")
            .into_iter()
            .map(|call| {
                assert_eq!(call.run.tool, Tool::Systemctl);
                assert_eq!(call.run.bound_secs, BOUND);
                assert_eq!(&call.run.args[..3], ["show", SHOW_PROPERTIES, "--"]);
                call.run.args[3].clone()
            })
            .collect()
    }
}

fn not_loaded(detail: &str) -> Failure {
    Failure::new(Reason::UnitNotLoaded, detail)
}

/// Catches an install over an archive version ending well while systemd still finds no unit:
/// the dangling link the move left is `unit_not_loaded`, of class environment, a failure.
#[test]
fn a_dangling_link_in_the_unit_directory_is_unit_not_loaded() {
    let host = Host::new("dangling");
    symlink(
        "/var/lib/hostkeeper/packages/hostkeeper/current/units/hostkeeper.service",
        host.at(UNIT),
    )
    .expect("the archive's link");

    let failure = host.check(&[UNIT]).expect_err("the unit is hidden");

    assert_eq!(
        failure,
        not_loaded(
            "systemd loads hostkeeper.service from no file (LoadState=not-found), not from the \
             package's unit in /usr/lib/systemd/system/: a unit left in /etc/systemd/system/ \
             hides it"
        )
    );
    assert_eq!(
        (failure.reason().class(), failure.reason().outcome()),
        (Class::Environment, Outcome::Failed)
    );
    assert_eq!(failure.reason().token(), "unit_not_loaded");
}

/// Catches a check that only asks whether the unit loads: a file, or a link that resolves, in
/// the unit directory loads, but it is not the package's unit.
#[test]
fn a_unit_file_or_live_link_in_the_unit_directory_is_unit_not_loaded() {
    for (name, make) in [
        (
            "file",
            (|at: &Path, _: &Path| fs::write(at, "[Unit]\n")) as fn(&Path, &Path) -> _,
        ),
        ("link", |at: &Path, tree: &Path| {
            symlink(tree.join(PACKAGE_UNIT), at)
        }),
    ] {
        let host = Host::new(name);
        make(&host.at(UNIT), &host.tree()).expect("the unit in the unit directory");

        let failure = host.check(&[UNIT]).expect_err("the unit is hidden");

        assert_eq!(
            failure,
            not_loaded(
                "systemd loads hostkeeper.service from /etc/systemd/system/hostkeeper.service \
                 (LoadState=loaded), not from the package's unit in /usr/lib/systemd/system/: a \
                 unit left in /etc/systemd/system/ hides it"
            ),
            "{name}"
        );
    }
}

/// Catches a unit that does not load read as the package's because its file is: a `LoadState`
/// other than `loaded` with the package's `FragmentPath` (a unit file systemd refused) is
/// `unit_not_loaded`.
#[test]
fn the_packages_file_that_does_not_load_is_unit_not_loaded() {
    let host = Host::new("bad-setting");
    host.fake.answer(
        "systemctl show",
        0,
        Ok(ToolOutput {
            code: Some(0),
            stdout:
                b"LoadState=bad-setting\nFragmentPath=/usr/lib/systemd/system/hostkeeper.service\n"
                    .to_vec(),
            stderr: Vec::new(),
            cut: false,
        }),
    );

    assert_eq!(
        host.check(&[UNIT]),
        Err(not_loaded(
            "systemd does not load the package's unit \
             /usr/lib/systemd/system/hostkeeper.service (LoadState=bad-setting)"
        ))
    );
}

/// Catches the package's own unit refused: with the unit directory clear, systemd loads it from
/// the package's file, from `/usr/lib` or `/lib`.
#[test]
fn the_packages_unit_loaded_from_its_file_passes() {
    let host = Host::new("passes");
    host.check(&[UNIT]).expect("the package's unit");
    assert_eq!(host.shown(), [UNIT]);

    let lib = Host::new("passes-lib");
    let unit = "hostkeeper-boot.service";
    let rel = format!("lib/systemd/system/{unit}");
    lib.fake.edit(|s| {
        let deb = s.debs.pop().expect("the deb").file(&rel, 0o644, "[Unit]\n");
        s.debs.push(deb);
    });
    fs::create_dir_all(lib.tree().join("lib/systemd/system")).expect("the tree");
    fs::write(lib.tree().join(&rel), "[Unit]\n").expect("the tree's unit");
    lib.check(&[unit]).expect("the package's unit in /lib");
}

/// Catches a unit the package does not install checked, and a template asked about: only the
/// archive's units that the package also installs are the package's to answer to.
#[test]
fn units_the_package_does_not_install_and_templates_are_not_checked() {
    let host = Host::new("unchecked");
    let template = "hello@.service";
    fs::write(
        host.tree().join("usr/lib/systemd/system").join(template),
        "[Unit]\n",
    )
    .expect("a template");
    symlink("/gone", host.at("hostkeeper-old.service")).expect("an archive-only unit");

    host.check(&["hostkeeper-old.service", template, UNIT])
        .expect("only the package's unit is checked");

    assert_eq!(host.shown(), [UNIT]);
}

/// Catches a failed `systemctl show` read as a verdict on the unit: a command that cannot run,
/// or that exits other than 0, is `io_error`, which the daemon tries again.
#[test]
fn a_show_that_cannot_run_or_fails_is_io_error() {
    let host = Host::new("cannot-run");
    host.fake.cannot_run("systemctl show");
    assert_eq!(
        host.check(&[UNIT]),
        Err(Failure::new(
            Reason::IoError,
            "systemctl show hostkeeper.service: systemctl show was not found"
        ))
    );

    let host = Host::new("exits");
    host.fake.answer(
        "systemctl show",
        0,
        Ok(ToolOutput {
            code: Some(1),
            stdout: Vec::new(),
            stderr: b"Failed to connect to bus\n".to_vec(),
            cut: false,
        }),
    );
    assert_eq!(
        host.check(&[UNIT]),
        Err(Failure::new(
            Reason::IoError,
            "systemctl show hostkeeper.service failed: Failed to connect to bus"
        ))
    );
}

/// Catches `systemctl show`'s output misread: both properties in either order, a line of
/// another property, a line with no `=`, and a value holding `=` are each read as systemd
/// prints them.
#[test]
fn show_output_is_read_by_property() {
    assert_eq!(
        parse_show("FragmentPath=/usr/lib/a=b.service\nOther=1\nnoise\nLoadState=loaded\n"),
        UnitLoad {
            load_state: "loaded".to_owned(),
            fragment_path: "/usr/lib/a=b.service".to_owned(),
        }
    );
    assert_eq!(parse_show(""), UnitLoad::default());
}
