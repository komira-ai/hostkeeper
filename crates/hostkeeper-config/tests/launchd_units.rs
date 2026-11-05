//! A launchd plist under `units/` and a launchd label in `[units] run` (design sections 4.7 and
//! 5.9), beside the systemd units the manifest loader has always taken.

mod support;

use std::fs;
use std::os::unix::fs::symlink;

use hostkeeper_config::manifest::ManifestRefusal;
use hostkeeper_config::names::{UnitError, launchd_label, plist_label};
use support::package::{EXAMPLE, Package, with, with_in};

const RUN: &str = r#"run = ["tierstore@9301.service", "tierstore@9302.service"]"#;
const URL: &str = "http://127.0.0.1:{port}/healthz";
const NOT_LINKED: &str = "is neither a file of units/ nor an instance of a template there";

/// A package whose `units/` holds only the plists named by `files`.
fn plist_package(name: &str, files: &[&str]) -> Package {
    let package = Package::new(name);
    fs::remove_file(package.scratch.path().join("units/tierstore@.service")).expect("remove");
    for file in files {
        package
            .scratch
            .file(&format!("units/{file}"), "<plist/>\n", 0o444);
    }
    package
}

/// The example with the run list `run` and a health URL with a fixed port.
fn manifest_running(run: &str) -> String {
    with_in(
        &with(RUN, &format!("run = [{run}]")),
        URL,
        "http://127.0.0.1:9301/healthz",
    )
}

fn bad_label(why: &'static str) -> UnitError {
    UnitError::BadLabel(why)
}

fn run_refusal(unit: &str, why: &str) -> ManifestRefusal {
    ManifestRefusal::RunUnit {
        unit: unit.to_owned(),
        why: why.to_owned(),
    }
}

/// Catches a loader that refuses a plist under `units/` or a label in `run`.
#[test]
fn manifest_accepts_a_launchd_plist_and_label() {
    let package = plist_package("accepts", &["tierstore.web-1.plist"]);
    let manifest = package
        .load_text(&manifest_running(r#""tierstore.web-1""#))
        .expect("loads");
    assert_eq!(manifest.run, vec!["tierstore.web-1".to_owned()]);
}

/// Catches a label equal to the service name being refused.
#[test]
fn a_label_equal_to_the_service_loads() {
    let package = plist_package("equal", &["tierstore.plist"]);
    let manifest = package
        .load_text(&manifest_running(r#""tierstore""#))
        .expect("loads");
    assert_eq!(manifest.run, vec!["tierstore".to_owned()]);
}

/// Catches a label of the form `<service>.x` being refused.
#[test]
fn a_label_of_the_service_dot_x_loads() {
    let package = plist_package("dotx", &["tierstore.x.plist", "tierstore.y.plist"]);
    let manifest = package
        .load_text(&manifest_running(r#""tierstore.x", "tierstore.y""#))
        .expect("loads");
    assert_eq!(manifest.run.len(), 2);
}

/// Catches a plist that no run entry names being refused: a package may carry a helper job.
#[test]
fn a_plist_no_run_entry_names_loads() {
    let package = plist_package("spare", &["tierstore.x.plist"]);
    let manifest = package.load_text(&with(RUN, "run = []")).expect("loads");
    assert!(manifest.run.is_empty());
}

/// Catches a label of another service being taken as the service's.
#[test]
fn a_run_label_of_another_service_is_refused() {
    let package = plist_package("other", &["tierstore.x.plist"]);
    assert_eq!(
        package.refusal(&manifest_running(r#""other.x""#)),
        run_refusal(
            "other.x",
            "is not a launchd label of the service: it is neither the service name nor the \
             service name, `.` and more"
        )
    );
}

/// Catches a bare prefix of the service name (`tierstorex`) being taken as its label.
#[test]
fn a_run_label_with_the_service_as_a_bare_prefix_is_refused() {
    let package = plist_package("prefix", &["tierstore.x.plist"]);
    assert_eq!(
        package.refusal(&manifest_running(r#""tierstorex""#)),
        run_refusal(
            "tierstorex",
            "is not a launchd label of the service: it is neither the service name nor the \
             service name, `.` and more"
        )
    );
}

/// Catches `<service>.` with nothing after the dot being taken as a label.
#[test]
fn a_run_label_with_nothing_after_the_dot_is_refused() {
    let package = plist_package("nodot", &["tierstore.x.plist"]);
    assert_eq!(
        package.refusal(&manifest_running(r#""tierstore.""#)),
        run_refusal(
            "tierstore.",
            "is not a launchd label of the service: it is neither the service name nor the \
             service name, `.` and more"
        )
    );
}

/// Catches a character outside `A-Za-z0-9._-` in a run label being taken.
#[test]
fn a_run_label_with_a_bad_character_is_refused() {
    let package = plist_package("badchar", &["tierstore.x.plist"]);
    assert_eq!(
        package.refusal(&manifest_running(r#""tierstore.x y""#)),
        run_refusal(
            "tierstore.x y",
            "is not a launchd label of the service: it has a character outside `A-Za-z0-9._-`"
        )
    );
}

/// Catches a run label over 255 bytes being taken.
#[test]
fn a_run_label_over_the_length_cap_is_refused() {
    let package = plist_package("long", &["tierstore.x.plist"]);
    let label = format!("tierstore.{}", "a".repeat(246));
    assert_eq!(label.len(), 256);
    assert_eq!(
        package.refusal(&manifest_running(&format!("\"{label}\""))),
        run_refusal(
            &label,
            "is not a launchd label of the service: it is longer than 255 bytes"
        )
    );
}

/// Catches an empty run entry being taken as a label.
#[test]
fn an_empty_run_entry_is_refused() {
    let package = plist_package("empty", &["tierstore.x.plist"]);
    assert_eq!(
        package.refusal(&manifest_running(r#""""#)),
        run_refusal("", "is not a launchd label of the service: it is empty")
    );
}

/// Catches a plist whose label is not the service's being accepted into `units/`.
#[test]
fn a_plist_of_another_service_is_refused() {
    let package = plist_package("otherplist", &["other.x.plist"]);
    assert_eq!(
        package.refusal(&manifest_running(r#""tierstore.x""#)),
        ManifestRefusal::UnitFile {
            file: "other.x.plist".to_owned(),
            why: bad_label("it is neither the service name nor the service name, `.` and more"),
        }
    );
}

/// Catches a run label whose plist is missing from `units/` being accepted.
#[test]
fn a_run_label_without_its_plist_is_refused() {
    let package = plist_package("noplist", &["tierstore.x.plist"]);
    assert_eq!(
        package.refusal(&manifest_running(r#""tierstore.y""#)),
        run_refusal("tierstore.y", NOT_LINKED)
    );
}

/// Catches a plist that is a link being followed or taken.
#[test]
fn a_plist_that_is_a_symlink_is_refused() {
    let package = plist_package("link", &[]);
    symlink(
        "nowhere",
        package.scratch.path().join("units/tierstore.x.plist"),
    )
    .expect("link");
    assert_eq!(
        package.refusal(&manifest_running(r#""tierstore.x""#)),
        ManifestRefusal::UnitNotAFile {
            file: "tierstore.x.plist".to_owned()
        }
    );
}

/// Catches a plist that is a directory being taken.
#[test]
fn a_plist_that_is_a_directory_is_refused() {
    let package = plist_package("dir", &[]);
    package.scratch.dir("units/tierstore.x.plist", 0o755);
    assert_eq!(
        package.refusal(&manifest_running(r#""tierstore.x""#)),
        ManifestRefusal::UnitNotAFile {
            file: "tierstore.x.plist".to_owned()
        }
    );
}

/// Catches a package that mixes systemd units and plists being taken.
#[test]
fn a_package_with_service_and_plist_files_is_refused() {
    let package = Package::new("mixed");
    package
        .scratch
        .file("units/tierstore.x.plist", "x\n", 0o444);
    assert_eq!(
        package.refusal(EXAMPLE),
        ManifestRefusal::UnitFile {
            file: "tierstore.x.plist".to_owned(),
            why: UnitError::MixedKinds,
        }
    );
    assert_eq!(
        UnitError::MixedKinds.to_string(),
        "is a launchd plist, and units/ also holds systemd units"
    );
}

/// Catches a `{port}` health URL being accepted with a launchd label, which has no port.
#[test]
fn a_port_url_with_a_label_run_entry_is_refused() {
    let package = plist_package("port", &["tierstore.x.plist"]);
    let text = with(RUN, r#"run = ["tierstore.x"]"#);
    assert_eq!(
        package.refusal(&text),
        ManifestRefusal::PortWithoutInstance {
            unit: "tierstore.x".to_owned()
        }
    );
}

/// Catches the label rule at its edges: the cap is 255 bytes, and the plist name is exactly
/// `<label>.plist`.
#[test]
fn the_label_rule_at_its_edges() {
    let longest = format!("web.{}", "a".repeat(251));
    assert_eq!(launchd_label("web", &longest), Ok(longest.as_str()));
    assert_eq!(
        launchd_label("web", &format!("{longest}a")),
        Err(bad_label("it is longer than 255 bytes"))
    );
    assert_eq!(launchd_label("web", "web"), Ok("web"));
    assert_eq!(launchd_label("web", "web.x"), Ok("web.x"));
    assert_eq!(plist_label("web", "web.x.plist"), Ok("web.x"));
    assert_eq!(
        plist_label("web", "web.x"),
        Err(bad_label("a plist file is named `<label>.plist`"))
    );
    assert_eq!(plist_label("web", ".plist"), Err(bad_label("it is empty")));
    assert_eq!(
        UnitError::BadLabel("it is empty").to_string(),
        "is not a launchd label of the service: it is empty"
    );
}
