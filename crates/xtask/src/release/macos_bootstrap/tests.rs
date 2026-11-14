use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{
    BINARY_PATH, COMPONENT, IDENTIFIER, PLIST, PLIST_PATH, POSTINSTALL, Request, Want, build,
    check, payload, pkg_name,
};
use crate::release::Error;
use crate::release::contract::Version;
use crate::release::fakes::{FakePkgbuild, scratch, tree_of};
use crate::release::macos_pkg::tests::{gzip, odc, odc_entry};
use crate::release::ports::PortError;

const BINARY: &[u8] = b"the bootstrap program";

fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

fn package_info(version: &str, scripts: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<pkg-info overwrite-permissions=\"true\" \
         relocatable=\"false\" identifier=\"{IDENTIFIER}\" version=\"{version}\" \
         install-location=\"/\" format-version=\"2\" auth=\"root\">\n    <payload \
         numberOfFiles=\"6\" installKBytes=\"1\"/>\n{scripts}    <bundle-version/>\n</pkg-info>\n"
    )
}

const POSTINSTALL_INFO: &str =
    "    <scripts>\n        <postinstall file=\"./postinstall\"/>\n    </scripts>\n";

fn distribution(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<installer-gui-script minSpecVersion=\"1\">\n    \
         <pkg-ref id=\"{IDENTIFIER}\"/>\n    <choices-outline>\n        <line choice=\"default\">\n            \
         <line choice=\"{IDENTIFIER}\"/>\n        </line>\n    </choices-outline>\n    \
         <choice id=\"default\"/>\n    <choice id=\"{IDENTIFIER}\" visible=\"false\">\n        \
         <pkg-ref id=\"{IDENTIFIER}\"/>\n    </choice>\n    <pkg-ref id=\"{IDENTIFIER}\" \
         version=\"{version}\" onConclusion=\"none\">#{COMPONENT}</pkg-ref>\n    <product \
         id=\"{IDENTIFIER}\" version=\"{version}\"/>\n</installer-gui-script>\n"
    )
}

/// The payload of the build, as `pkgbuild` writes it: `.` and `./<path>`, root and wheel.
fn entries() -> Vec<Vec<u8>> {
    payload(BINARY.to_vec())
        .into_iter()
        .map(|(path, want)| {
            let name = if path.is_empty() {
                ".".to_owned()
            } else {
                format!("./{path}")
            };
            match want {
                Want::Dir => odc_entry(&name, 0o040_755, 0, 0, b""),
                Want::File { mode, bytes } => odc_entry(&name, 0o100_000 | mode, 0, 0, &bytes),
            }
        })
        .collect()
}

/// An expanded product in `dir` with the given payload and `PackageInfo`, and the good script.
fn expanded(dir: &Path, version: &str, payload_entries: &[Vec<u8>], info: &str) -> PathBuf {
    let at = dir.join("expanded");
    let component = at.join(COMPONENT);
    std::fs::create_dir_all(component.join("Scripts")).expect("dirs");
    std::fs::write(at.join("Distribution"), distribution(version)).expect("write");
    std::fs::write(component.join("Bom"), b"bom").expect("write");
    std::fs::write(component.join("PackageInfo"), info).expect("write");
    std::fs::write(component.join("Payload"), gzip(&odc(payload_entries))).expect("write");
    let script = component.join("Scripts/postinstall");
    std::fs::write(&script, POSTINSTALL).expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("mode");
    at
}

fn good(dir: &Path) -> PathBuf {
    expanded(
        dir,
        "0.1.5",
        &entries(),
        &package_info("0.1.5", POSTINSTALL_INFO),
    )
}

fn binary(dir: &Path) -> PathBuf {
    let path = dir.join("hostkeeper-bootstrap");
    std::fs::write(&path, BINARY).expect("write");
    path
}

fn problems(result: Result<Vec<String>, Error>) -> Vec<String> {
    match result {
        Err(Error::Refused(lines)) => lines,
        other => panic!("not refused: {other:?}"),
    }
}

fn request(d: &Path) -> Request {
    Request {
        version: version(),
        binary: binary(d),
        stage: d.join("stage"),
        out: d.join("out"),
    }
}

/// The index of the entry named `name` in `list`.
fn index_of(list: &[Vec<u8>], name: &str) -> usize {
    list.iter()
        .position(|e| e[76..].starts_with(format!("{name}\0").as_bytes()))
        .unwrap_or_else(|| panic!("{name}"))
}

#[test]
fn the_pkg_is_built_from_exactly_the_program_the_plist_and_the_script() {
    // Catches: another file in the tree, a script not handed to pkgbuild, another identifier or
    // version, the product under another name, and a stage that already exists.
    let d = scratch("bootstrap-pkg-build");
    let fakes = FakePkgbuild::default();
    let lines = build(&fakes, &request(&d)).expect("built");
    assert_eq!(
        pkg_name(&version()),
        "hostkeeper-bootstrap-0.1.5-macos-arm64.pkg"
    );
    assert!(
        lines[0].starts_with("hostkeeper-bootstrap-0.1.5-macos-arm64.pkg: sha256 "),
        "{lines:?}"
    );
    let calls = fakes.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].scripts, Some(d.join("stage/scripts")));
    assert_eq!(
        (calls[0].identifier.as_str(), calls[0].version.as_str()),
        (IDENTIFIER, "0.1.5")
    );
    assert_eq!(calls[0].pkg, d.join("stage").join(COMPONENT));
    assert_eq!(
        fakes.trees.borrow()[0],
        [
            "Library/",
            "Library/LaunchDaemons/",
            "Library/LaunchDaemons/dev.hostkeeper.bootstrap.plist",
            "usr/",
            "usr/local/",
            "usr/local/lib/",
            "usr/local/lib/hostkeeper-bootstrap/",
            "usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap",
        ]
    );
    let mode = |p: &str| {
        std::fs::metadata(d.join("stage").join(p))
            .expect(p)
            .permissions()
            .mode()
            & 0o7777
    };
    assert_eq!(mode(&format!("root/{BINARY_PATH}")), 0o755);
    assert_eq!(mode(&format!("root/{PLIST_PATH}")), 0o644);
    assert_eq!(mode("root/usr/local/lib"), 0o755);
    assert_eq!(mode("scripts/postinstall"), 0o755);
    assert_eq!(
        std::fs::read_to_string(d.join("stage/scripts/postinstall")).expect("script"),
        POSTINSTALL
    );
    assert_eq!(
        std::fs::read(d.join("stage/root").join(BINARY_PATH)).expect("program"),
        BINARY
    );
    assert_eq!(
        fakes.product_calls.borrow()[0].product,
        d.join("out/hostkeeper-bootstrap-0.1.5-macos-arm64.pkg")
    );
    assert_eq!(tree_of(&d.join("stage/scripts")), ["postinstall"]);
    assert!(
        build(&fakes, &request(&d)).is_err(),
        "a stage that exists is refused"
    );
}

#[test]
fn a_missing_program_and_a_failing_pkgbuild_or_productbuild_are_errors() {
    // Catches: a build that goes on with no program, or hides what pkgbuild or productbuild said.
    let d = scratch("bootstrap-pkg-build-fail");
    let mut missing = request(&d);
    missing.binary = d.join("absent");
    assert!(matches!(
        build(&FakePkgbuild::default(), &missing),
        Err(Error::Io { .. })
    ));
    let component_fails = FakePkgbuild {
        fail: Some(PortError::new("pkgbuild", "boom")),
        ..FakePkgbuild::default()
    };
    let error = build(&component_fails, &request(&d)).expect_err("fails");
    assert!(error.to_string().contains("boom"), "{error}");
    let d = scratch("bootstrap-pkg-build-fail-product");
    let product_fails = FakePkgbuild {
        fail_product: Some(PortError::new("productbuild", "bang")),
        ..FakePkgbuild::default()
    };
    let error = build(&product_fails, &request(&d)).expect_err("fails");
    assert!(error.to_string().contains("bang"), "{error}");
}

#[test]
fn the_committed_plist_and_script_are_what_the_check_demands_of_the_job() {
    // Catches: a plist or script in packaging/ that the check would refuse, so a release could
    // never pass its own check.
    let d = scratch("bootstrap-pkg-good");
    let lines = check(&good(&d), &version(), &binary(&d)).expect("passes");
    assert!(
        lines[0].contains(&format!("{IDENTIFIER} 0.1.5")),
        "{lines:?}"
    );
    assert!(PLIST.contains("<string>dev.hostkeeper.bootstrap</string>"));
}

#[test]
fn a_missing_part_of_the_product_or_the_component_is_refused() {
    // Catches: a product with another file, no component, or a component with another part.
    let d = scratch("bootstrap-pkg-parts");
    let at = good(&d);
    std::fs::write(at.join("extra"), b"x").expect("write");
    std::fs::write(at.join(COMPONENT).join("Extra"), b"x").expect("write");
    std::fs::remove_file(at.join(COMPONENT).join("Bom")).expect("remove");
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        [
            "extra: an expanded product of the bootstrap pkg holds only the file Distribution and the directory hostkeeper-bootstrap.pkg",
            "Extra: an expanded component of the bootstrap pkg holds only the files Bom, PackageInfo and Payload and the directory Scripts",
            "missing Bom",
        ]
    );
    let d = scratch("bootstrap-pkg-nothing");
    let empty = d.join("empty");
    std::fs::create_dir(&empty).expect("dir");
    assert_eq!(
        problems(check(&empty, &version(), &binary(&d))),
        ["missing Distribution", "missing hostkeeper-bootstrap.pkg"]
    );
}

#[test]
fn a_component_without_its_package_info_scripts_or_payload_is_refused_naming_each() {
    // Catches: a component that is read as passing when the parts that carry its rules are not
    // there to be read.
    let d = scratch("bootstrap-pkg-bare-component");
    let at = good(&d);
    let component = at.join(COMPONENT);
    std::fs::remove_file(component.join("PackageInfo")).expect("remove");
    std::fs::remove_file(component.join("Payload")).expect("remove");
    std::fs::remove_dir_all(component.join("Scripts")).expect("remove");
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        ["missing PackageInfo", "missing Payload", "missing Scripts"]
    );
}

#[test]
fn another_version_identifier_or_script_in_package_info_is_refused() {
    // Catches: the pkg of another release, a preinstall that runs code as root, and no script
    // to load the job.
    let d = scratch("bootstrap-pkg-info");
    let info = package_info("0.1.4", "    <preinstall file=\"./preinstall\"/>\n")
        .replace(IDENTIFIER, "dev.hostkeeper.other");
    let at = expanded(&d, "0.1.5", &entries(), &info);
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        [
            "PackageInfo: identifier is \"dev.hostkeeper.other\", not \"dev.hostkeeper.bootstrap\"",
            "PackageInfo: version is \"0.1.4\", not \"0.1.5\"",
            "PackageInfo names a preinstall script: the pkg's one script is its postinstall",
            "PackageInfo names no postinstall script (<postinstall file=\"./postinstall\"/>): nothing would load the job",
        ]
    );
    let bad = scratch("bootstrap-pkg-info-bad");
    let at = expanded(&bad, "0.1.5", &entries(), "no element here");
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        ["PackageInfo has no readable <pkg-info> element"]
    );
    let info = package_info("0.1.5", POSTINSTALL_INFO).replace(" install-location=\"/\"", "");
    let bare = scratch("bootstrap-pkg-info-no-location");
    let at = expanded(&bare, "0.1.5", &entries(), &info);
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        ["PackageInfo has no install-location"]
    );
}

#[test]
fn a_scripts_directory_with_another_file_mode_or_bytes_is_refused() {
    // Catches: a second script, a script that is not the committed one, and one that is not
    // executable.
    let d = scratch("bootstrap-pkg-scripts");
    let at = good(&d);
    let scripts = at.join(COMPONENT).join("Scripts");
    std::fs::write(scripts.join("preinstall"), b"x").expect("write");
    std::fs::write(scripts.join("postinstall"), b"#!/bin/sh\nexit 0\n").expect("write");
    std::fs::set_permissions(
        scripts.join("postinstall"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("mode");
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        [
            "Scripts holds \"preinstall\": the pkg's one script is postinstall",
            "Scripts: postinstall has mode 644, not 755",
            "Scripts: postinstall is not packaging/macos-bootstrap/postinstall",
        ]
    );
    let d = scratch("bootstrap-pkg-no-script");
    let at = good(&d);
    std::fs::remove_file(at.join(COMPONENT).join("Scripts/postinstall")).expect("remove");
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        ["Scripts holds no postinstall"]
    );
}

#[test]
fn a_payload_that_is_not_gzip_or_not_cpio_is_refused() {
    // Catches: a Payload that is read as empty and so passes.
    let d = scratch("bootstrap-pkg-payload-bad");
    let at = good(&d);
    std::fs::write(at.join(COMPONENT).join("Payload"), b"plain").expect("write");
    let found = problems(check(&at, &version(), &binary(&d)));
    assert_eq!(found.len(), 1);
    assert!(found[0].starts_with("Payload"), "{found:?}");
}

#[test]
fn another_owner_type_mode_bytes_entry_or_a_missing_entry_is_refused_naming_each() {
    // Catches: a file another account owns, a directory where a file goes, a program that is not
    // the one given, an executable plist, a file outside the pkg's two, and a short payload.
    let d = scratch("bootstrap-pkg-payload");
    let mut list = entries();
    let program = format!("./{BINARY_PATH}");
    let at = index_of(&list, &program);
    list[at] = odc_entry(&program, 0o100_755, 501, 20, b"another program");
    let plist = format!("./{PLIST_PATH}");
    let at = index_of(&list, &plist);
    list[at] = odc_entry(&plist, 0o100_755, 0, 0, PLIST.as_bytes());
    let lib = "./usr/local/lib";
    let at = index_of(&list, lib);
    list[at] = odc_entry(lib, 0o100_644, 0, 0, b"");
    list.push(odc_entry(
        "./usr/local/bin/hostkeeper",
        0o100_755,
        0,
        0,
        b"x",
    ));
    list.remove(index_of(&list, "./Library"));
    let at = expanded(&d, "0.1.5", &list, &package_info("0.1.5", POSTINSTALL_INFO));
    assert_eq!(
        problems(check(&at, &version(), &binary(&d))),
        [
            "/Library/LaunchDaemons/dev.hostkeeper.bootstrap.plist: mode 755, not 644",
            "/usr/local/lib: of type 100000, not 40000",
            "/usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap: owned by uid 501 and gid 20, not root and wheel (0 and 0)",
            "/usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap: not the bytes release macos-bootstrap-pkg writes",
            "/usr/local/bin/hostkeeper (\"./usr/local/bin/hostkeeper\") is in the payload, which holds only the program, the plist and their directories",
            "missing /Library in the payload",
        ]
    );
}

#[test]
fn a_plist_that_does_not_parse_or_says_another_thing_is_refused() {
    // Catches: each read-back of the job's plist taken out.
    let plist = format!("./{PLIST_PATH}");
    let cases: [(String, &str); 8] = [
        ("not a plist".to_owned(), " does not parse as a plist: "),
        (
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<array/>\n</plist>\n"
                .to_owned(),
            " is not a plist dictionary",
        ),
        (
            PLIST.replace("<string>dev.hostkeeper.bootstrap</string>", "<string>x.y</string>"),
            ": the label is Some(\"x.y\"), not \"dev.hostkeeper.bootstrap\"",
        ),
        (
            PLIST.replace("/usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap", "/bin/sh"),
            ": the job runs Some(\"/bin/sh\"), not \"/usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap\"",
        ),
        (
            PLIST.replace("<key>RunAtLoad</key>\n\t<true/>", "<key>RunAtLoad</key>\n\t<false/>"),
            ": RunAtLoad is not true, so the bootstrap would not start at boot",
        ),
        (
            PLIST.replace("<key>SuccessfulExit</key>\n\t\t<false/>", "<key>SuccessfulExit</key>\n\t\t<true/>"),
            ": KeepAlive is not {SuccessfulExit: false}, so launchd would not start the bootstrap again only after a failed run",
        ),
        (
            PLIST.replace(
                "<key>StandardOutPath</key>\n\t<string>/var/log/hostkeeper-bootstrap.log</string>",
                "<key>StandardOutPath</key>\n\t<string>/var/log/other.log</string>",
            ),
            ": StandardOutPath is Some(\"/var/log/other.log\"), not \"/var/log/hostkeeper-bootstrap.log\"",
        ),
        (
            PLIST.replace(
                "<key>StandardErrorPath</key>\n\t<string>/var/log/hostkeeper-bootstrap.log</string>",
                "",
            ),
            ": StandardErrorPath is None, not \"/var/log/hostkeeper-bootstrap.log\"",
        ),
    ];
    for (n, (text, words)) in cases.into_iter().enumerate() {
        let d = scratch(&format!("bootstrap-pkg-plist-{n}"));
        let mut list = entries();
        let at = index_of(&list, &plist);
        list[at] = odc_entry(&plist, 0o100_644, 0, 0, text.as_bytes());
        let at = expanded(&d, "0.1.5", &list, &package_info("0.1.5", POSTINSTALL_INFO));
        let found = problems(check(&at, &version(), &binary(&d)));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].ends_with("not the bytes release macos-bootstrap-pkg writes"),
            "{found:?}"
        );
        assert!(found[1].contains(words), "{found:?} lacks {words}");
    }
}

#[test]
fn an_unreadable_program_or_distribution_is_an_error_not_a_pass() {
    // Catches: a check that passes when it could not read what it checks.
    let d = scratch("bootstrap-pkg-unreadable");
    let at = good(&d);
    assert!(matches!(
        check(&at, &version(), &d.join("absent")),
        Err(Error::Io { .. })
    ));
    let d = scratch("bootstrap-pkg-no-distribution");
    let at = good(&d);
    std::fs::remove_file(at.join("Distribution")).expect("remove");
    std::fs::create_dir(at.join("Distribution")).expect("dir");
    assert!(
        check(&at, &version(), &binary(&d)).is_err(),
        "a Distribution that is a directory is refused"
    );
}
