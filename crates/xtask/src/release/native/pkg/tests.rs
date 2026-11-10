use std::path::Path;

use super::{COMPONENT, IDENTIFIER, Request, build, check, payload, pkg_name};
use crate::release::Error;
use crate::release::contract::Version;
use crate::release::fakes::{FakePkgbuild, scratch, tree_of};
use crate::release::macos_pkg::tests::{dir_entry, gzip, odc, odc_entry};
use crate::release::native::hello::pkg_files;

const BINARY: &[u8] = b"the hello program";

fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

fn package_info(version: &str, scripts: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<pkg-info overwrite-permissions=\"true\" \
         relocatable=\"false\" identifier=\"{IDENTIFIER}\" version=\"{version}\" \
         install-location=\"/\" format-version=\"2\" auth=\"root\">\n    <payload \
         numberOfFiles=\"10\" installKBytes=\"1\"/>\n{scripts}    <bundle-version/>\n</pkg-info>\n"
    )
}

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

/// The payload of the build, as `pkgbuild` writes it: `.` and `./<path>`, uid 0, gid `gid`.
fn entries(fail_ready: bool, gid: u32) -> Vec<Vec<u8>> {
    let files = pkg_files(BINARY.to_vec(), fail_ready).expect("files");
    payload(files)
        .into_iter()
        .map(|(path, want)| {
            let name = if path.is_empty() {
                ".".to_owned()
            } else {
                format!("./{path}")
            };
            match want {
                super::Want::Dir => odc_entry(&name, 0o040_755, 0, gid, b""),
                super::Want::File { mode, bytes } => {
                    odc_entry(&name, 0o100_000 | mode, 0, gid, &bytes)
                }
            }
        })
        .collect()
}

/// An expanded product of hello's pkg in `dir`, with `payload` and `package_info`.
fn expanded(
    dir: &Path,
    version: &str,
    payload_entries: &[Vec<u8>],
    info: &str,
) -> std::path::PathBuf {
    let at = dir.join("expanded");
    std::fs::create_dir_all(at.join(COMPONENT)).expect("dirs");
    std::fs::write(at.join("Distribution"), distribution(version)).expect("write");
    std::fs::write(at.join(COMPONENT).join("Bom"), b"bom").expect("write");
    std::fs::write(at.join(COMPONENT).join("PackageInfo"), info).expect("write");
    std::fs::write(
        at.join(COMPONENT).join("Payload"),
        gzip(&odc(payload_entries)),
    )
    .expect("write");
    at
}

fn binary(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("hello");
    std::fs::write(&path, BINARY).expect("write");
    path
}

fn problems(result: Result<Vec<String>, Error>) -> Vec<String> {
    match result {
        Err(Error::Refused(lines)) => lines,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn the_pkg_is_built_with_no_scripts_from_exactly_hellos_payload() {
    // Catches: a scripts directory handed to pkgbuild, a tree with another file or mode, another
    // identifier or version, or the product written under another name.
    let d = scratch("native-pkg-build");
    let fakes = FakePkgbuild::default();
    let request = Request {
        version: version(),
        fail_ready: true,
        binary: binary(&d),
        stage: d.join("stage"),
        out: d.join("out"),
    };
    let lines = build(&fakes, &request).expect("built");
    assert_eq!(
        pkg_name("0.1.5-fail-ready"),
        "hello-0.1.5-fail-ready-macos-arm64.pkg"
    );
    assert!(
        lines[0].starts_with("hello-0.1.5-fail-ready-macos-arm64.pkg: sha256 "),
        "{lines:?}"
    );
    let calls = fakes.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].scripts, None);
    assert_eq!(calls[0].identifier, IDENTIFIER);
    assert_eq!(calls[0].version, "0.1.5-fail-ready");
    assert_eq!(calls[0].pkg, d.join("stage").join(COMPONENT));
    assert_eq!(
        fakes.trees.borrow()[0],
        [
            "Library/",
            "Library/Application Support/",
            "Library/Application Support/hello/",
            "Library/Application Support/hello/bin/",
            "Library/Application Support/hello/bin/hello",
            "Library/LaunchDaemons/",
            "Library/LaunchDaemons/hello.a.plist",
            "Library/LaunchDaemons/hello.b.plist",
        ]
    );
    let product = fakes.product_calls.borrow();
    assert_eq!(
        product[0].product,
        d.join("out/hello-0.1.5-fail-ready-macos-arm64.pkg")
    );
    let root = d.join("stage/root");
    let mode = |p: &str| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(root.join(p))
            .expect(p)
            .permissions()
            .mode()
            & 0o7777
    };
    assert_eq!(mode("Library/Application Support/hello/bin/hello"), 0o755);
    assert_eq!(mode("Library/LaunchDaemons/hello.a.plist"), 0o644);
    assert_eq!(tree_of(&root).len(), 8);
    assert!(
        build(&fakes, &request).is_err(),
        "a stage that exists is refused"
    );
}

#[test]
fn a_good_expanded_pkg_of_each_build_passes_with_group_wheel_or_admin() {
    // Catches: a check that refuses what pkgbuild writes for either build, or either group.
    let d = scratch("native-pkg-good");
    let bin = binary(&d);
    for (fail_ready, text, gid) in [(false, "0.1.5", 0), (true, "0.1.5-fail-ready", 80)] {
        let sub = d.join(text);
        let at = expanded(
            &sub,
            text,
            &entries(fail_ready, gid),
            &package_info(text, ""),
        );
        let lines = check(&at, &version(), fail_ready, &bin).expect("passes");
        assert!(
            lines[0].contains(&format!("{IDENTIFIER} {text}")),
            "{lines:?}"
        );
    }
}

#[test]
fn a_component_with_scripts_or_a_package_info_naming_one_is_refused() {
    // Catches: a pkg that runs code as root at install.
    let d = scratch("native-pkg-scripts");
    let bin = binary(&d);
    let info = package_info(
        "0.1.5",
        "    <scripts>\n        <postinstall file=\"./postinstall\"/>\n    </scripts>\n",
    );
    let at = expanded(&d, "0.1.5", &entries(false, 0), &info);
    std::fs::create_dir(at.join(COMPONENT).join("Scripts")).expect("Scripts");
    assert_eq!(
        problems(check(&at, &version(), false, &bin)),
        [
            "Scripts: an expanded component of a service's pkg holds only the files Bom, PackageInfo and Payload: no Scripts, since it has no script",
            "PackageInfo names a postinstall script: a service's pkg has no script",
        ]
    );
}

#[test]
fn a_plist_not_disabled_an_entry_outside_the_paths_and_a_wrong_owner_are_refused() {
    // Catches: a job launchd would load at boot before hostkeeper enables it, a file outside
    // the service's paths, and an entry another account owns.
    let d = scratch("native-pkg-payload");
    let bin = binary(&d);
    let mut list = entries(false, 0);
    let plist_name = "./Library/LaunchDaemons/hello.a.plist";
    let at_plist = list
        .iter()
        .position(|e| {
            e.windows(plist_name.len())
                .any(|w| w == plist_name.as_bytes())
        })
        .expect("the plist");
    let enabled = crate::release::native::hello::plist("a", false)
        .expect("plist")
        .replace("\t<key>Disabled</key>\n\t<true/>\n", "");
    list[at_plist] = odc_entry(plist_name, 0o100_644, 0, 0, enabled.as_bytes());
    list.push(odc_entry(
        "./Library/LaunchDaemons/sshd.plist",
        0o100_644,
        0,
        0,
        b"",
    ));
    list.push(dir_entry("./usr"));
    list.push(odc_entry(
        "./Library/Application Support/hello/extra",
        0o100_644,
        501,
        20,
        b"",
    ));
    let at = expanded(&d, "0.1.5", &list, &package_info("0.1.5", ""));
    let found = problems(check(&at, &version(), false, &bin));
    assert_eq!(
        found,
        [
            "/Library/LaunchDaemons/hello.a.plist: not the bytes release macos-pkg writes",
            "/Library/LaunchDaemons/hello.a.plist: Disabled is not true, so launchd would load the job at boot before hostkeeper enables it",
            "/Library/LaunchDaemons/sshd.plist (\"./Library/LaunchDaemons/sshd.plist\") is in the payload: not a file of hello's pkg",
            "/usr (\"./usr\") is in the payload: not a file of hello's pkg",
            "/Library/Application Support/hello/extra (\"./Library/Application Support/hello/extra\") is in the payload: not a file of hello's pkg",
        ]
    );
}

#[test]
fn another_version_identifier_or_a_missing_entry_is_refused() {
    // Catches: the good build's pkg checked as the failing build's, and a payload short of a file.
    let d = scratch("native-pkg-version");
    let bin = binary(&d);
    let mut list = entries(false, 0);
    list.pop();
    let at = expanded(&d, "0.1.5", &list, &package_info("0.1.5", ""));
    let found = problems(check(&at, &version(), true, &bin));
    let fail_ready = pkg_files(BINARY.to_vec(), true).expect("files");
    let plist_b = &fail_ready.last().expect("a file").path;
    assert_eq!(
        found,
        [
            "Distribution: the <pkg-ref> of #hello.pkg has the version Some(\"0.1.5\"), not \"0.1.5-fail-ready\"".to_owned(),
            "Distribution: the <product> version is Some(\"0.1.5\"), not \"0.1.5-fail-ready\"".to_owned(),
            "PackageInfo: version is \"0.1.5\", not \"0.1.5-fail-ready\"".to_owned(),
            "/Library/LaunchDaemons/hello.a.plist: not the bytes release macos-pkg writes".to_owned(),
            format!("missing {plist_b} in the payload"),
        ]
    );
}

/// The index of the entry named `name` in `list`.
fn index_of(list: &[Vec<u8>], name: &str) -> usize {
    list.iter()
        .position(|e| e[76..].starts_with(format!("{name}\0").as_bytes()))
        .unwrap_or_else(|| panic!("{name}"))
}

#[test]
fn a_wanted_entry_of_another_owner_type_or_mode_is_refused_naming_each() {
    // Catches: a file of the package another account owns, a directory where a file goes, and a
    // plist that is executable.
    let d = scratch("native-pkg-attributes");
    let bin = binary(&d);
    let mut list = entries(false, 0);
    let program = "./Library/Application Support/hello/bin/hello";
    let at = index_of(&list, program);
    list[at] = odc_entry(program, 0o100_755, 501, 20, BINARY);
    let plist_a = "./Library/LaunchDaemons/hello.a.plist";
    let at = index_of(&list, plist_a);
    let text = crate::release::native::hello::plist("a", false).expect("plist");
    list[at] = odc_entry(plist_a, 0o100_755, 0, 0, text.as_bytes());
    let plist = "./Library/LaunchDaemons/hello.b.plist";
    let at = index_of(&list, plist);
    list[at] = dir_entry(plist);
    let at = expanded(&d, "0.1.5", &list, &package_info("0.1.5", ""));
    assert_eq!(
        problems(check(&at, &version(), false, &bin)),
        [
            "/Library/Application Support/hello/bin/hello: owned by uid 501 and gid 20, not root and wheel or admin",
            "/Library/LaunchDaemons/hello.a.plist: mode 755, not 644",
            "/Library/LaunchDaemons/hello.b.plist: of type 40000, not 100000",
        ]
    );
}

#[test]
fn a_plist_that_does_not_parse_is_no_dictionary_has_another_label_or_runs_a_program_elsewhere_is_refused()
 {
    // Catches: each read-back of a job's plist taken out.
    let name = "./Library/LaunchDaemons/hello.a.plist";
    let good = crate::release::native::hello::plist("a", false).expect("plist");
    let cases: [(String, &str); 4] = [
        ("not a plist".to_owned(), " does not parse as a plist: "),
        (
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<array/>\n</plist>\n"
                .to_owned(),
            " is not a plist dictionary",
        ),
        (
            good.replace("<string>hello.a</string>", "<string>hello.x</string>"),
            ": the label is Some(\"hello.x\"), not \"hello.a\"",
        ),
        (
            good.replace(
                "/Library/Application Support/hello/bin/hello",
                "/usr/local/bin/hello",
            ),
            ": the job runs Some(\"/usr/local/bin/hello\"), which is not a file of the payload",
        ),
    ];
    for (n, (text, words)) in cases.into_iter().enumerate() {
        let d = scratch(&format!("native-pkg-plist-{n}"));
        let bin = binary(&d);
        let mut list = entries(false, 0);
        let at = index_of(&list, name);
        list[at] = odc_entry(name, 0o100_644, 0, 0, text.as_bytes());
        let at = expanded(&d, "0.1.5", &list, &package_info("0.1.5", ""));
        let found = problems(check(&at, &version(), false, &bin));
        assert_eq!(
            found[0],
            "/Library/LaunchDaemons/hello.a.plist: not the bytes release macos-pkg writes"
        );
        assert_eq!(found.len(), 2, "{words}: {found:?}");
        assert!(
            found[1].starts_with(&format!("/Library/LaunchDaemons/hello.a.plist{words}")),
            "{words}: {found:?}"
        );
    }
}

#[test]
fn a_package_info_without_its_element_or_an_attribute_or_with_a_preinstall_and_a_bad_payload_are_refused()
 {
    // Catches: each read of PackageInfo and of the payload taken out.
    let d = scratch("native-pkg-info");
    let bin = binary(&d);
    let at = expanded(&d, "0.1.5", &entries(false, 0), "<nothing/>");
    std::fs::write(at.join(COMPONENT).join("Payload"), b"not gzip").expect("write");
    let found = problems(check(&at, &version(), false, &bin));
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0], "PackageInfo has no readable <pkg-info> element");
    assert!(
        found[1].starts_with("Payload is not gzip data: "),
        "{found:?}"
    );

    let d = scratch("native-pkg-info-attr");
    let info = package_info(
        "0.1.5",
        "    <scripts>\n        <preinstall file=\"./preinstall\"/>\n    </scripts>\n",
    )
    .replace(" install-location=\"/\"", "")
    .replace(
        &format!("identifier=\"{IDENTIFIER}\""),
        "identifier=\"dev.other\"",
    );
    let at = expanded(&d, "0.1.5", &entries(false, 0), &info);
    assert_eq!(
        problems(check(&at, &version(), false, &binary(&d))),
        [
            "PackageInfo: identifier is \"dev.other\", not \"dev.hostkeeper.hello\"",
            "PackageInfo has no install-location",
            "PackageInfo names a preinstall script: a service's pkg has no script",
        ]
    );
}

#[test]
fn an_expanded_product_without_its_parts_names_each_missing_one() {
    // Catches: a check that passes a product with no Distribution, or a component with no
    // PackageInfo or Payload, because it only reads the parts that are there.
    let d = scratch("native-pkg-parts");
    let bin = binary(&d);
    let at = expanded(&d, "0.1.5", &entries(false, 0), &package_info("0.1.5", ""));
    std::fs::remove_file(at.join("Distribution")).expect("remove");
    std::fs::remove_file(at.join(COMPONENT).join("PackageInfo")).expect("remove");
    std::fs::remove_file(at.join(COMPONENT).join("Payload")).expect("remove");
    assert_eq!(
        problems(check(&at, &version(), false, &bin)),
        [
            "missing Distribution",
            "missing PackageInfo",
            "missing Payload"
        ]
    );
    std::fs::remove_dir_all(at.join(COMPONENT)).expect("remove");
    assert_eq!(
        problems(check(&at, &version(), false, &bin)),
        ["missing Distribution", "missing hello.pkg"]
    );
}

#[test]
fn an_entry_of_root_with_a_group_other_than_wheel_or_admin_is_refused() {
    // Catches: the group of an entry not checked when its owner is root.
    let d = scratch("native-pkg-group");
    let bin = binary(&d);
    let mut list = entries(false, 0);
    let program = "./Library/Application Support/hello/bin/hello";
    let at = index_of(&list, program);
    list[at] = odc_entry(program, 0o100_755, 0, 20, BINARY);
    let at = expanded(&d, "0.1.5", &list, &package_info("0.1.5", ""));
    assert_eq!(
        problems(check(&at, &version(), false, &bin)),
        [
            "/Library/Application Support/hello/bin/hello: owned by uid 0 and gid 20, not root and wheel or admin"
        ]
    );
}
