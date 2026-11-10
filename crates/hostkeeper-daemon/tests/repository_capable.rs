//! The native installer a host could run once it has a repository entry (`repositories::capable`),
//! which a pushed set may give it after the start.

use std::fs;
use std::path::Path;

use hostkeeper_daemon::repositories::capable;
use hostkeeper_install::Announcement;
use hostkeeper_install::announce::Family;
use hostkeeper_install::repository::announce::{APT_TOOLS, DPKG_STATUS};
use hostkeeper_release::Installer;
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

fn put(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
    fs::write(path, text).expect("the file");
}

fn debian(root: &Path) {
    for tool in APT_TOOLS {
        put(root, tool, "");
    }
    put(
        root,
        DPKG_STATUS,
        "Package: apt\nStatus: install ok installed\nVersion: 2.6.1\n",
    );
}

fn announcement(family: Option<Family>) -> Announcement {
    Announcement {
        family,
        ..Announcement::default()
    }
}

/// Proves a Debian host with apt's tools and a recent apt is capable of `apt` whether or not it
/// has an entry, and a host of another family, or without the tools, is not.
#[test]
fn a_debian_host_with_apt_is_capable_of_it_and_no_other_host_is() {
    let root = scratch("capable-debian");
    debian(root.path());
    assert_eq!(
        capable(&announcement(Some(Family::Deb)), root.path()),
        Some(Installer::Apt)
    );
    assert_eq!(capable(&announcement(Some(Family::Rpm)), root.path()), None);
    assert_eq!(capable(&announcement(None), root.path()), None);
    let bare = scratch("capable-bare");
    assert_eq!(capable(&announcement(Some(Family::Deb)), bare.path()), None);
}
