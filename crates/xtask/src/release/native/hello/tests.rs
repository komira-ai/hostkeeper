use super::{DEB_PROGRAM, File, PKG_PROGRAM, deb_files, pkg_files, plist, unit};

/// The committed goldens of hello's native packages (`examples/hello/package/deb` and `pkg`):
/// each file of the good build but the program.
const DEB_A: &str = include_str!("../../../../../../examples/hello/package/deb/hello@a.service");
const DEB_B: &str = include_str!("../../../../../../examples/hello/package/deb/hello@b.service");
const PKG_A: &str = include_str!("../../../../../../examples/hello/package/pkg/hello.a.plist");
const PKG_B: &str = include_str!("../../../../../../examples/hello/package/pkg/hello.b.plist");

fn listed(files: &[File]) -> Vec<(&str, u32)> {
    files.iter().map(|f| (f.path.as_str(), f.mode)).collect()
}

fn text(files: &[File], path: &str) -> String {
    let file = files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path}"));
    String::from_utf8(file.data.clone()).expect("text")
}

#[test]
fn the_deb_holds_the_program_and_one_plain_unit_per_slot_as_the_goldens() {
    // Catches: a file moved, added or left out, a mode changed, the program's bytes not the
    // given binary's, or a unit that differs from its committed golden.
    let files = deb_files(b"BIN".to_vec(), false);
    assert_eq!(
        listed(&files),
        [
            ("/usr/lib/hello/hello", 0o755),
            ("/usr/lib/systemd/system/hello@a.service", 0o644),
            ("/usr/lib/systemd/system/hello@b.service", 0o644),
        ]
    );
    assert_eq!(files[0].data, b"BIN");
    assert_eq!(
        text(&files, "/usr/lib/systemd/system/hello@a.service"),
        DEB_A
    );
    assert_eq!(
        text(&files, "/usr/lib/systemd/system/hello@b.service"),
        DEB_B
    );
}

#[test]
fn the_pkg_holds_the_program_and_one_disabled_job_per_slot_as_the_goldens() {
    // Catches: as for the deb, and a plist whose Disabled entry is lost.
    let files = pkg_files(b"BIN".to_vec(), false).expect("the files");
    assert_eq!(
        listed(&files),
        [
            ("/Library/Application Support/hello/bin/hello", 0o755),
            ("/Library/LaunchDaemons/hello.a.plist", 0o644),
            ("/Library/LaunchDaemons/hello.b.plist", 0o644),
        ]
    );
    assert_eq!(files[0].data, b"BIN");
    assert_eq!(text(&files, "/Library/LaunchDaemons/hello.a.plist"), PKG_A);
    assert_eq!(text(&files, "/Library/LaunchDaemons/hello.b.plist"), PKG_B);
    for slot in ["a", "b"] {
        let value = plist::Value::from_reader_xml(plist(slot, false).expect("a plist").as_bytes())
            .expect("parses");
        let dict = value.as_dictionary().expect("a dictionary");
        assert_eq!(
            dict.get("Disabled").and_then(plist::Value::as_boolean),
            Some(true)
        );
        assert_eq!(
            dict.get("Label").and_then(plist::Value::as_string),
            Some(format!("hello.{slot}").as_str())
        );
    }
}

#[test]
fn the_failing_build_differs_only_in_the_fail_ready_flag_of_each_command() {
    // Catches: a failing build that does not start the program with --fail-ready (so it would
    // pass verify), or one that differs in anything else.
    for slot in ["a", "b"] {
        assert_eq!(
            unit(slot, true),
            unit(slot, false).replace(" --port 9310\n", " --port 9310 --fail-ready\n")
        );
        assert_eq!(
            plist(slot, true).expect("a plist"),
            plist(slot, false).expect("a plist").replace(
                "<string>9310</string>\n",
                "<string>9310</string>\n\t\t<string>--fail-ready</string>\n"
            )
        );
    }
    let (good, failing) = (
        deb_files(b"B".to_vec(), false),
        deb_files(b"B".to_vec(), true),
    );
    assert_eq!(listed(&good), listed(&failing));
}

#[test]
fn the_programs_are_where_hellos_hook_looks_in_a_staged_tree() {
    // Catches: a package whose program is not where hello's prepare looks for it, which would
    // fail every update with bad_artifact.
    assert_eq!(
        DEB_PROGRAM.trim_start_matches('/'),
        hello::hook::DEB_PROGRAM
    );
    assert_eq!(
        PKG_PROGRAM.trim_start_matches('/'),
        hello::hook::PKG_PROGRAM
    );
    assert_eq!(hello::cli::DEFAULT_PORT, 9310);
}

#[test]
fn a_slot_that_is_not_part_of_a_launchd_label_is_refused_naming_its_plist() {
    // Catches: a plist written for a label launchd would refuse.
    assert_eq!(
        plist("a b", false),
        Err(crate::release::Error::Failed(
            "/Library/LaunchDaemons/hello.a b.plist: hello.a b is not a launchd label, or its job is not plist text".to_owned()
        ))
    );
}
