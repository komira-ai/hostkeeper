//! hostkeeper's own package at `stage` (packages-native.md section 10.4): held to its own list of
//! files, links and directories instead of the path rule of section 10.2 for a service, and
//! alone in being allowed maintainer scripts.

use hostkeeper_install::repository::tree::{OWN_FILES, OWN_LINKS, own_dirs};
use hostkeeper_install::repository::{Class, Order, Outcome, Reason};

use crate::apt_fake::Deb;
use crate::harness::*;

use Class::Release;
use Outcome::Failed;

/// The package `package` holding exactly hostkeeper's own list, with two maintainer scripts.
fn own_deb(package: &str) -> Deb {
    let mut deb = Deb::new(package, V1, "amd64")
        .member("postinst")
        .member("prerm");
    for dir in own_dirs() {
        deb = deb.dir(&dir, 0o755);
    }
    for file in OWN_FILES {
        deb = deb.file(file, 0o644, &unit(None));
    }
    for (path, target) in OWN_LINKS {
        deb = deb.link(path, target);
    }
    deb
}

/// `own_deb` without its scripts.
fn bare_deb(package: &str) -> Deb {
    let mut deb = own_deb(package);
    deb.members.clear();
    deb
}

fn run_the_unit(order: &mut Order) {
    order.run = vec!["hostkeeper.service".to_owned()];
}

/// Stages `package` as the service `service`, which must be refused: its reason and text.
fn refusal(name: &str, service: &str, package: &Deb) -> (Reason, String) {
    let (_, result, _) = super::tree::tree_stage(name, service, package, run_the_unit);
    let failure = result.expect_err(name);
    assert_failure(&failure, failure.reason(), Release, Failed);
    (failure.reason(), failure.detail().to_owned())
}

/// Proves hostkeeper's own package, holding exactly its list (each file, the link to its target,
/// each directory above them) and its scripts, stages for the service `hostkeeper`, with no hook.
#[test]
fn hostkeepers_own_package_with_exactly_its_list_stages() {
    let package = own_deb("hostkeeper");
    let (_env, result, dir) =
        super::tree::tree_stage("apt-own-list", "hostkeeper", &package, run_the_unit);
    assert!(!result.expect("the own list stages").hook);
    for file in OWN_FILES {
        assert!(dir.join("tree").join(file).is_file(), "{file}");
    }
    for (path, target) in OWN_LINKS {
        let link = std::fs::read_link(dir.join("tree").join(path)).expect("the link");
        assert_eq!(link, std::path::Path::new(target));
    }
}

/// Proves the list's directories are those above its entries.
#[test]
fn the_own_directories_are_those_above_its_entries() {
    let dirs: Vec<String> = own_dirs().into_iter().collect();
    assert_eq!(
        dirs,
        [
            "etc",
            "etc/issue.d",
            "usr",
            "usr/bin",
            "usr/lib",
            "usr/lib/hostkeeper",
            "usr/lib/systemd",
            "usr/lib/systemd/system",
            "usr/lib/tmpfiles.d",
            "usr/share",
            "usr/share/hostkeeper",
        ]
    );
}

/// Proves an entry outside the list is `manifest_invalid` for hostkeeper's own package, with the
/// text naming the path: a file in a directory the path rule of a service would allow, a
/// directory the list lacks, a file where the list has a link, a link with another target, a link
/// where the list has a file, a link at a path the list lacks, and a directory where it has a file.
#[test]
fn an_entry_outside_the_own_list_is_manifest_invalid() {
    let issue = "etc/issue.d/hostkeeper.issue";
    let cases: Vec<(&str, Deb, &str)> = vec![
        (
            "a file in usr/bin",
            own_deb("hostkeeper").file("usr/bin/extra", 0o755, "x"),
            "usr/bin/extra",
        ),
        (
            "a file in the package's library directory",
            own_deb("hostkeeper").file("usr/lib/hostkeeper/hostkeeper-hook", 0o755, "x"),
            "usr/lib/hostkeeper/hostkeeper-hook",
        ),
        (
            "a documentation file",
            own_deb("hostkeeper").file("usr/share/doc/hostkeeper/copyright", 0o644, "x"),
            "usr/share/doc",
        ),
        (
            "a directory the list lacks",
            own_deb("hostkeeper").dir("etc/hostkeeper", 0o755),
            "etc/hostkeeper",
        ),
        (
            "a file where the list has a link",
            own_deb("hostkeeper").without(issue).file(issue, 0o644, "x"),
            issue,
        ),
        (
            "a link with another target",
            own_deb("hostkeeper").link(issue, "/etc/shadow"),
            issue,
        ),
        (
            "a link where the list has a file",
            own_deb("hostkeeper").link("usr/bin/hostkeeper", "/bin/sh"),
            "usr/bin/hostkeeper",
        ),
        (
            "a link at a path the list lacks",
            own_deb("hostkeeper").link("etc/systemd", "/dev/null"),
            "etc/systemd",
        ),
        (
            "a directory where the list has a file",
            own_deb("hostkeeper").dir("usr/bin/hostkeeper-pxe", 0o755),
            "usr/bin/hostkeeper-pxe",
        ),
    ];
    for (what, package, path) in cases {
        let (reason, detail) = refusal("apt-own-extra", "hostkeeper", &package);
        assert_eq!(reason, Reason::ManifestInvalid, "{what}");
        assert_eq!(
            detail,
            format!(
                "the package installs /{path}, which is not on hostkeeper's own package's list"
            ),
            "{what}"
        );
    }
}

/// Proves a named pipe in hostkeeper's own package is refused as neither file, directory nor link.
#[test]
fn a_special_file_in_the_own_package_is_refused() {
    let package = own_deb("hostkeeper").fifo("usr/bin/pipe");
    let (reason, detail) = refusal("apt-own-fifo", "hostkeeper", &package);
    assert_eq!(reason, Reason::ManifestInvalid);
    assert_eq!(
        detail,
        "the package installs /usr/bin/pipe, which is neither a file, a directory nor a link"
    );
}

/// Proves the list is hostkeeper's own package's alone: the same tree (without scripts) as a
/// package named `hostkeeper` of another service, and as another package of the service
/// `hostkeeper`, is held to the path rule of a service.
#[test]
fn the_own_list_is_not_a_services() {
    let (reason, detail) = refusal("apt-list-elsewhere", SERVICE, &bare_deb("hostkeeper"));
    assert_eq!(reason, Reason::ManifestInvalid);
    assert_eq!(
        detail,
        "the package installs /etc/issue.d, outside the paths a service's package may use"
    );
    let (reason, detail) = refusal(
        "apt-list-other-package",
        "hostkeeper",
        &bare_deb("hostkeeperd"),
    );
    assert_eq!(reason, Reason::ManifestInvalid);
    assert_eq!(
        detail,
        "the package installs /etc/issue.d, outside the paths a service's package may use"
    );
}

/// Proves scripts stay refused as `package_scripts` for a package named `hostkeeper` of another
/// service and for another package of the service `hostkeeper`, whatever tree they hold.
#[test]
fn scripts_stay_refused_outside_the_own_package() {
    for (name, service, package) in [
        ("apt-script-elsewhere", SERVICE, "hostkeeper"),
        ("apt-script-other-package", "hostkeeper", "hostkeeperd"),
    ] {
        let (reason, detail) = refusal(name, service, &own_deb(package));
        assert_eq!(reason, Reason::PackageScripts, "{name}");
        assert_eq!(
            detail, "the package carries the maintainer scripts postinst, prerm",
            "{name}"
        );
    }
}
