//! Tests of the shape check of an expanded package.

use std::fs;

use hostkeeper_testkit::Scratch;

use super::*;

const ID: &str = "com.example.hello";
const VERSION: &str = "1.2.0";

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    Scratch::under(&target.join("tmp"), name).unwrap()
}

fn package_info(extra: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<pkg-info format-version=\"2\" identifier=\"{ID}\" version=\"{VERSION}\" install-location=\"/\" auth=\"root\">{extra}<payload numberOfFiles=\"1\" installKBytes=\"0\"/></pkg-info>\n"
    )
}

/// Makes a component in `dir`: `PackageInfo`, `Bom` and a `Payload` directory.
fn component(dir: &Path, info: &str) {
    fs::create_dir_all(dir.join("Payload")).unwrap();
    fs::write(dir.join("PackageInfo"), info).unwrap();
    fs::write(dir.join("Bom"), b"bom").unwrap();
}

fn distribution(choice_attrs: &str, refs: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<installer-gui-script minSpecVersion=\"1\"><choices-outline><line choice=\"default\"/></choices-outline><choice id=\"default\" {choice_attrs}><pkg-ref id=\"{ID}\"/></choice>{refs}</installer-gui-script>"
    )
}

const REF: &str = "<pkg-ref id=\"com.example.hello\" version=\"1.2.0\">#hello.pkg</pkg-ref>";

/// A product archive with one component `hello.pkg` and the `Distribution` `text`.
fn product(dir: &Path, text: &str) {
    component(&dir.join("hello.pkg"), &package_info(""));
    fs::write(dir.join("Distribution"), text).unwrap();
}

fn refused(dir: &Path) -> ShapeError {
    check(dir, ID, VERSION).unwrap_err()
}

#[test]
fn a_component_package_is_its_own_component() {
    let dir = scratch("shcomp");
    component(dir.path(), &package_info(""));
    let found = check(dir.path(), ID, VERSION).unwrap();
    assert_eq!(found.dir, dir.path());
    assert_eq!(found.package_info(), dir.path().join("PackageInfo"));
    assert_eq!(found.bom(), dir.path().join("Bom"));
    assert_eq!(found.payload(), dir.path().join("Payload"));
}

#[test]
fn a_product_archive_with_one_component_finds_it() {
    let dir = scratch("shprod");
    product(dir.path(), &distribution("start_selected=\"true\"", REF));
    fs::create_dir(dir.path().join("Resources")).unwrap();
    let found = check(dir.path(), ID, VERSION).unwrap();
    assert_eq!(found.dir, dir.path().join("hello.pkg"));
}

/// Catches: a product archive of two components installing the second unchecked.
#[test]
fn a_product_archive_of_two_components_is_refused() {
    let dir = scratch("shtwo");
    let refs = format!("{REF}<pkg-ref id=\"com.example.other\">#other.pkg</pkg-ref>");
    product(dir.path(), &distribution("", &refs));
    component(&dir.path().join("other.pkg"), &package_info(""));
    assert_eq!(refused(dir.path()), ShapeError::Components(2));
}

#[test]
fn a_distribution_naming_no_component_is_refused() {
    let dir = scratch("shnone");
    product(dir.path(), "<installer-gui-script/>");
    assert_eq!(refused(dir.path()), ShapeError::Components(0));
}

#[test]
fn one_component_at_two_paths_is_refused() {
    let dir = scratch("shpaths");
    let refs = format!("{REF}<pkg-ref id=\"{ID}\">#again.pkg</pkg-ref>");
    product(dir.path(), &distribution("", &refs));
    assert_eq!(refused(dir.path()), ShapeError::Paths(2));
}

#[test]
fn a_component_with_no_path_is_refused() {
    let dir = scratch("shnopath");
    product(
        dir.path(),
        &distribution("", &format!("<pkg-ref id=\"{ID}\"/>")),
    );
    assert_eq!(refused(dir.path()), ShapeError::Paths(0));
}

#[test]
fn the_same_path_named_twice_is_one_path() {
    let dir = scratch("shsame");
    let refs = format!("{REF}{REF}");
    product(dir.path(), &distribution("", &refs));
    assert!(check(dir.path(), ID, VERSION).is_ok());
}

#[test]
fn a_percent_escaped_path_names_the_decoded_directory() {
    let dir = scratch("shpercent");
    component(&dir.path().join("my hello.pkg"), &package_info(""));
    let refs = format!("<pkg-ref id=\"{ID}\">#my%20hello.pkg</pkg-ref>");
    fs::write(dir.path().join("Distribution"), distribution("", &refs)).unwrap();
    let found = check(dir.path(), ID, VERSION).unwrap();
    assert_eq!(found.dir, dir.path().join("my hello.pkg"));
}

/// Catches: a component path that climbs out of the expansion or is not a plain name.
#[test]
fn component_paths_that_are_not_a_plain_hash_name_are_refused() {
    for path in [
        "hello.pkg",
        "#",
        "#.",
        "#..",
        "#a/b",
        "#a%2Fb",
        "#bad%2",
        "#bad%zz",
        "#bad%ff",
        "#../etc",
    ] {
        let dir = scratch("shbadpath");
        let refs = format!("<pkg-ref id=\"{ID}\">{path}</pkg-ref>");
        product(dir.path(), &distribution("", &refs));
        assert_eq!(
            refused(dir.path()),
            ShapeError::BadPath(path.into()),
            "{path}"
        );
    }
}

#[test]
fn a_component_path_with_no_directory_is_refused() {
    let dir = scratch("shnodir");
    product(
        dir.path(),
        &distribution("", &format!("<pkg-ref id=\"{ID}\">#gone.pkg</pkg-ref>")),
    );
    assert_eq!(refused(dir.path()), ShapeError::BadPath("#gone.pkg".into()));
}

/// Catches: a choice that is not selected by default being installed anyway, or skipped.
#[test]
fn a_choice_that_is_not_selected_or_enabled_is_refused() {
    for switch in CHOICE_SWITCHES {
        let dir = scratch("shchoice");
        product(
            dir.path(),
            &distribution(&format!("{switch}=\"false\""), REF),
        );
        assert_eq!(
            refused(dir.path()),
            ShapeError::NotSelected(switch.into(), "false".into())
        );
    }
}

#[test]
fn a_choice_with_every_switch_true_is_accepted() {
    let dir = scratch("shchoiceok");
    let attrs = "start_selected=\"true\" selected=\"true\" start_enabled=\"true\" enabled=\"true\"";
    product(dir.path(), &distribution(attrs, REF));
    assert!(check(dir.path(), ID, VERSION).is_ok());
}

/// Catches: an install-time script of a `Distribution` going unnoticed.
#[test]
fn a_distribution_with_a_script_or_a_check_is_refused() {
    for name in REFUSED_DISTRIBUTION {
        let dir = scratch("shscript");
        let text = distribution("", REF).replace(
            "</installer-gui-script>",
            &format!("<{name}>x</{name}></installer-gui-script>"),
        );
        product(dir.path(), &text);
        assert_eq!(refused(dir.path()), ShapeError::RefusedElement(name.into()));
    }
}

#[test]
fn an_extra_entry_in_the_expansion_is_refused() {
    let dir = scratch("shextra");
    product(dir.path(), &distribution("", REF));
    fs::write(dir.path().join("Stray"), b"x").unwrap();
    let err = refused(dir.path());
    assert_eq!(err, ShapeError::Extra("Stray".into()));
    assert!(!err.is_scripts());
}

/// Catches: a script archive beside or in the component, or a script element of the
/// `PackageInfo`, being accepted or refused as a shape defect (`manifest_invalid`) instead of a
/// script (`package_scripts`).
#[test]
fn a_script_in_or_beside_the_component_or_its_package_info_is_a_script() {
    let beside = scratch("shscripts-beside");
    product(beside.path(), &distribution("", REF));
    fs::create_dir(beside.path().join("Scripts")).unwrap();
    let inside = scratch("shscripts-in");
    component(inside.path(), &package_info(""));
    fs::create_dir(inside.path().join("Scripts")).unwrap();
    let mut cases = vec![
        (
            refused(beside.path()),
            "the product archive holds \"Scripts\"".to_owned(),
        ),
        (
            refused(inside.path()),
            "the component holds \"Scripts\"".to_owned(),
        ),
    ];
    for name in SCRIPT_ELEMENTS {
        let dir = scratch("shscripts-info");
        component(
            dir.path(),
            &package_info(&format!("<scripts><{name} file=\"./{name}\"/></scripts>")),
        );
        cases.push((
            refused(dir.path()),
            format!("the PackageInfo has a <{name}> element"),
        ));
    }
    for (err, detail) in cases {
        assert_eq!(err, ShapeError::Scripts(detail));
        assert!(err.is_scripts());
    }
}

/// Catches: a payload-free component with scripts reported for its missing `Payload`, not for its
/// scripts.
#[test]
fn a_payload_free_component_with_scripts_is_a_script() {
    let dir = scratch("shscripts-nopayload");
    component(dir.path(), &package_info(""));
    fs::remove_dir(dir.path().join("Payload")).unwrap();
    fs::create_dir(dir.path().join("Scripts")).unwrap();
    assert!(refused(dir.path()).is_scripts());
}

/// Catches: an entry of a component other than its `Bom`, `PackageInfo` and `Payload` passing.
#[test]
fn an_entry_in_the_component_beside_its_three_is_refused() {
    let dir = scratch("shcompextra");
    component(dir.path(), &package_info(""));
    fs::write(dir.path().join("Resources"), b"x").unwrap();
    assert_eq!(
        refused(dir.path()),
        ShapeError::ComponentExtra("Resources".into())
    );
}

/// Catches: the empty elements `pkgbuild` writes into every `PackageInfo` (`<relocate/>`,
/// `<scripts/>`, `<bundle-version/>`) being refused, which refused every real package.
#[test]
fn the_package_info_pkgbuild_writes_is_accepted() {
    let dir = scratch("shpkgbuild");
    component(
        dir.path(),
        &package_info(
            "<bundle-version/><upgrade-bundle/><update-bundle/><atomic-update-bundle/>\
             <strict-identifier/><relocate/><scripts/>",
        ),
    );
    check(dir.path(), ID, VERSION).unwrap();
}

#[test]
fn a_distribution_that_is_not_xml_is_refused() {
    let dir = scratch("shbadxml");
    product(dir.path(), "<!DOCTYPE x><a/>");
    assert_eq!(
        refused(dir.path()),
        ShapeError::Xml("the Distribution".into(), xml::XmlError::Doctype)
    );
}

#[test]
fn a_distribution_that_is_not_utf8_cannot_be_read() {
    let dir = scratch("shutf");
    product(dir.path(), "");
    fs::write(dir.path().join("Distribution"), b"\xff\xfe").unwrap();
    assert!(matches!(refused(dir.path()), ShapeError::Read(what, _) if what == "the Distribution"));
}

#[test]
fn a_component_missing_an_entry_is_refused_by_name() {
    for missing in ["PackageInfo", "Bom", "Payload"] {
        let dir = scratch("shmissing");
        component(dir.path(), &package_info(""));
        let path = dir.path().join(missing);
        match path.is_dir() {
            true => fs::remove_dir(&path).unwrap(),
            false => fs::remove_file(&path).unwrap(),
        }
        assert_eq!(refused(dir.path()), ShapeError::Missing(missing));
    }
}

#[test]
fn a_payload_that_is_a_file_is_refused() {
    let dir = scratch("shpayfile");
    component(dir.path(), &package_info(""));
    fs::remove_dir(dir.path().join("Payload")).unwrap();
    fs::write(dir.path().join("Payload"), b"x").unwrap();
    assert_eq!(refused(dir.path()), ShapeError::Missing("Payload"));
}

#[test]
fn a_package_info_that_is_not_xml_is_refused() {
    let dir = scratch("shinfoxml");
    component(dir.path(), "<pkg-info");
    assert_eq!(
        refused(dir.path()),
        ShapeError::Xml("the PackageInfo".into(), xml::XmlError::Unclosed)
    );
}

#[test]
fn a_package_info_whose_first_element_is_not_pkg_info_is_refused() {
    for text in ["<other identifier=\"x\"/>", "", "<?xml version=\"1.0\"?>"] {
        let dir = scratch("shnoinfo");
        component(dir.path(), text);
        assert_eq!(refused(dir.path()), ShapeError::NoPkgInfo, "{text:?}");
    }
}

/// Catches: a package of another identifier, version or location installed as the order's.
#[test]
fn an_identifier_version_or_location_other_than_the_orders_is_refused() {
    let cases = [
        (
            "identifier",
            package_info("").replace(ID, "com.example.other"),
            "com.example.other",
            ID,
        ),
        (
            "version",
            package_info("").replace("version=\"1.2.0\"", "version=\"1.2.1\""),
            "1.2.1",
            VERSION,
        ),
        (
            "install-location",
            package_info("").replace("install-location=\"/\"", "install-location=\"/usr/local\""),
            "/usr/local",
            "/",
        ),
        (
            "install-location",
            package_info("").replace(" install-location=\"/\"", ""),
            "",
            "/",
        ),
    ];
    for (key, info, found, want) in cases {
        let dir = scratch("shmismatch");
        component(dir.path(), &info);
        assert_eq!(
            refused(dir.path()),
            ShapeError::Mismatch(key, found.into(), want.into())
        );
    }
}

/// Catches: a `bundle`, alone or in a `relocate`, that lets `installer` write somewhere the
/// `Bom` does not say.
#[test]
fn a_package_info_naming_a_bundle_is_refused_alone_or_in_a_relocate() {
    for extra in [
        "<bundle id=\"x\"/>",
        "<relocate><bundle id=\"x\"/></relocate>",
    ] {
        let dir = scratch("shrelocate");
        component(dir.path(), &package_info(extra));
        assert_eq!(
            refused(dir.path()),
            ShapeError::RefusedPackageInfo("bundle".into()),
            "{extra}"
        );
    }
}

#[test]
fn a_product_archives_component_is_checked_like_a_component_package() {
    let dir = scratch("shprodinfo");
    product(dir.path(), &distribution("", REF));
    fs::write(
        dir.path().join("hello.pkg/PackageInfo"),
        package_info("").replace(VERSION, "9"),
    )
    .unwrap();
    assert_eq!(
        refused(dir.path()),
        ShapeError::Mismatch("version", "9".into(), VERSION.into())
    );
}

/// Catches: the reason of a package with a script depending on the order of its directory's
/// entries: a `Scripts` entry beside a stray entry, in the component or beside it, and a
/// `PackageInfo` script element with a stray entry or no `Bom`, are each `package_scripts`.
#[test]
fn a_script_is_package_scripts_whatever_else_the_expansion_holds() {
    let inside = scratch("shscripts-and-extra");
    component(inside.path(), &package_info(""));
    for name in ["Scripts", "Aaa", "Zzz"] {
        fs::create_dir(inside.path().join(name)).unwrap();
    }
    let beside = scratch("shscripts-beside-and-stray");
    product(beside.path(), &distribution("", REF));
    for name in ["Scripts", "Aaa", "Zzz"] {
        fs::create_dir(beside.path().join(name)).unwrap();
    }
    let info = package_info("<scripts><postinstall file=\"./postinstall\"/></scripts>");
    let no_bom = scratch("shscripts-no-bom");
    component(no_bom.path(), &info);
    fs::remove_file(no_bom.path().join("Bom")).unwrap();
    let stray = scratch("shscripts-info-and-stray");
    component(stray.path(), &info);
    fs::create_dir(stray.path().join("Aaa")).unwrap();
    for (dir, detail) in [
        (inside.path(), "the component holds \"Scripts\""),
        (beside.path(), "the product archive holds \"Scripts\""),
        (no_bom.path(), "the PackageInfo has a <postinstall> element"),
        (stray.path(), "the PackageInfo has a <postinstall> element"),
    ] {
        assert_eq!(refused(dir), ShapeError::Scripts(detail.into()), "{detail}");
    }
}

/// Catches: which stray entry is reported depending on the directory's order.
#[test]
fn of_two_stray_entries_the_first_by_name_is_reported() {
    let inside = scratch("shtwo-extras");
    component(inside.path(), &package_info(""));
    let beside = scratch("shtwo-extras-beside");
    product(beside.path(), &distribution("", REF));
    for dir in [inside.path(), beside.path()] {
        for name in ["Zzz", "Aaa"] {
            fs::write(dir.join(name), b"x").unwrap();
        }
    }
    assert_eq!(
        refused(inside.path()),
        ShapeError::ComponentExtra("Aaa".into())
    );
    assert_eq!(refused(beside.path()), ShapeError::Extra("Aaa".into()));
}

/// Catches: a component with no `PackageInfo` passing, or reported as another defect.
#[test]
fn a_component_without_its_package_info_is_refused() {
    let dir = scratch("shno-info");
    component(dir.path(), &package_info(""));
    fs::remove_file(dir.path().join("PackageInfo")).unwrap();
    assert_eq!(refused(dir.path()), ShapeError::Missing("PackageInfo"));
}

/// Catches: a `Scripts` archive inside a product archive's component (where `productbuild
/// --package` puts a component's scripts) passing.
#[test]
fn a_script_archive_in_a_product_archives_component_is_a_script() {
    let dir = scratch("shscripts-product-component");
    product(dir.path(), &distribution("", REF));
    fs::create_dir(dir.path().join("hello.pkg/Scripts")).unwrap();
    assert_eq!(
        refused(dir.path()),
        ShapeError::Scripts("the component holds \"Scripts\"".into())
    );
}
