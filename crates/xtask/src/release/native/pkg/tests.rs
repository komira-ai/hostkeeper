use std::path::Path;

use super::{COMPONENT, IDENTIFIER, Request, build, check, payload, pkg_name, pkg_path_problem};
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
            "Library/Application Support/hello/hostkeeper-hook",
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
    assert_eq!(
        mode("Library/Application Support/hello/hostkeeper-hook"),
        0o755
    );
    assert_eq!(mode("Library/LaunchDaemons/hello.a.plist"), 0o644);
    assert_eq!(tree_of(&root).len(), 9);
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
            "/Library/LaunchDaemons/sshd.plist (\"./Library/LaunchDaemons/sshd.plist\") is in the payload: outside the paths of a pkg of hello: /Library/Application Support/hello/ and its jobs' plists /Library/LaunchDaemons/hello[.*].plist",
            "/usr (\"./usr\") is in the payload: outside the paths of a pkg of hello: /Library/Application Support/hello/ and its jobs' plists /Library/LaunchDaemons/hello[.*].plist",
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
    assert!(
        found.iter().any(|l| l.contains("not \"0.1.5-fail-ready\"")),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|l| l.starts_with("PackageInfo: version is \"0.1.5\"")),
        "{found:?}"
    );
    assert!(
        found.iter().any(|l| l.starts_with("missing /")),
        "{found:?}"
    );
}

#[test]
fn a_pkg_path_is_the_services_directory_its_jobs_plists_or_a_directory_above() {
    // Catches: the payload rule of section 4.7 widened or narrowed.
    for (path, dir) in [
        ("", true),
        ("Library", true),
        ("Library/LaunchDaemons", true),
        ("Library/Application Support/hello", true),
        ("Library/Application Support/hello/bin/hello", false),
        ("Library/LaunchDaemons/hello.plist", false),
        ("Library/LaunchDaemons/hello.a.plist", false),
    ] {
        assert_eq!(pkg_path_problem("hello", path, dir), None, "{path}");
    }
    for (path, dir) in [
        ("Library", false),
        ("Library/LaunchDaemons/hello.a.plist", true),
        ("Library/LaunchDaemons/helloworld.plist", false),
        ("Library/LaunchDaemons/sub/hello.a.plist", false),
        ("Library/LaunchAgents/hello.plist", false),
        ("usr/local/bin/hello", false),
        ("Library/Application Support/hellox/x", false),
    ] {
        assert!(pkg_path_problem("hello", path, dir).is_some(), "{path}");
    }
}
