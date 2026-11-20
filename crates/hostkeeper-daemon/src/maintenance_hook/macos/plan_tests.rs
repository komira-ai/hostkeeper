//! The tests of the install plan's file, in a scratch run directory: each plan read back as it was
//! kept, its bytes compared whole, and each refusal by its whole text.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use hostkeeper_testkit::Scratch;

use super::*;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn update() -> Plan {
    Plan {
        build: "25H62".to_owned(),
        install: Install::Update {
            label: "macOS Tahoe 26.7-25H62".to_owned(),
        },
    }
}

fn upgrade() -> Plan {
    Plan {
        build: "26B70".to_owned(),
        install: Install::Upgrade {
            app: "/Applications/Install macOS 27.app".to_owned(),
        },
    }
}

/// Proves: a minor update's and a major upgrade's plan each read back as kept, from a file of
/// mode 0600 holding the run, the build and the label or the app, and nothing else; a second plan
/// of the run replaces the first.
///
/// Catches: a plan that does not survive to the restart or the check, or one readable by others.
#[test]
fn a_plan_reads_back_as_it_was_kept() {
    let dir = scratch("macos-plan-roundtrip");
    let plans = PlanFile::new(Some(dir.path()));
    plans.save("run-1", &update()).expect("kept");
    let path = dir.path().join(PLAN_FILE);
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        r#"{"build":"25H62","label":"macOS Tahoe 26.7-25H62","runId":"run-1"}"#
    );
    let mode = fs::metadata(&path).expect("its mode").permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600);
    assert_eq!(plans.load("run-1"), Ok(update()));
    plans.save("run-1", &upgrade()).expect("kept");
    assert_eq!(plans.load("run-1"), Ok(upgrade()));
}

/// Proves: a plan of another run, one that is not JSON, one larger than the bound, one with no
/// build, and one naming both or neither of a label and an app, or an app outside `/Applications`
/// or not named `Install macOS *.app`, are each refused with a text naming the file and never
/// quoting it; with no run directory nothing is kept or read.
///
/// Catches: an earlier run's plan installed or checked, or a corrupted plan making `verify`
/// remove a directory that is not a macOS installer.
#[test]
fn a_plan_that_is_not_this_runs_is_refused() {
    let dir = scratch("macos-plan-refused");
    let plans = PlanFile::new(Some(dir.path()));
    let path = dir.path().join(PLAN_FILE);
    let refused = |why: &str| Err(format!("the install plan {} {why}", path.display()));
    plans.save("run-1", &update()).expect("kept");
    assert_eq!(plans.load("run-2"), refused("is not of the run run-2"));
    let cases = [
        ("{not json", "is not a plan's JSON"),
        (r#"{"runId":"run-1","label":"x"}"#, "names no build"),
        (
            r#"{"runId":"run-1","build":"","label":"x"}"#,
            "names no build",
        ),
        (
            r#"{"runId":"run-1","build":"1A1"}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
        (
            r#"{"runId":"run-1","build":"1A1","label":"x","installer":"/Applications/Install macOS 27.app"}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
        (
            r#"{"runId":"run-1","build":"1A1","label":""}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
        (
            r#"{"runId":"run-1","build":"1A1","installer":"/"}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
        (
            r#"{"runId":"run-1","build":"1A1","installer":"/Applications/Xcode.app"}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
        (
            r#"{"runId":"run-1","build":"1A1","installer":"/Applications/Install macOS 27.app/x/../.."}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
        (
            r#"{"runId":"run-1","build":"1A1","installer":"/Applications/Install macOS .app"}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
        (
            r#"{"runId":"run-1","build":"1A1","installer":"/Library/Install macOS 27.app"}"#,
            "names neither one update label nor one installer app in /Applications",
        ),
    ];
    for (text, why) in cases {
        fs::write(&path, text).expect("written");
        assert_eq!(plans.load("run-1"), refused(why), "{text}");
    }
    fs::write(&path, vec![b' '; 16 * 1024 + 1]).expect("written");
    assert_eq!(plans.load("run-1"), refused("is larger than 16384 bytes"));

    let none = PlanFile::new(None);
    let no_dir =
        Err("the hook has no run directory: its parameters came on standard input".to_owned());
    assert_eq!(none.save("run-1", &update()), no_dir);
    assert_eq!(none.load("run-1"), no_dir);
}

/// Proves: a plan whose directory is gone cannot be kept, and the failure names the file.
///
/// Catches: an install started with no plan behind it.
#[test]
fn a_plan_that_cannot_be_written_names_its_file() {
    let dir = scratch("macos-plan-unwritable");
    let gone = dir.path().join("gone");
    let plans = PlanFile::new(Some(&gone));
    assert_eq!(
        plans.save("run-1", &update()),
        Err(format!(
            "the install plan {} could not be written: No such file or directory (os error 2)",
            gone.join(PLAN_FILE).display()
        ))
    );
}
