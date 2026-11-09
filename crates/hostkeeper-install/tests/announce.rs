//! Which installers a daemon announces (packages.md section 5.4; packages-tests.md, "Which
//! installers a daemon announces", the cases of this build): every host announces
//! `hostkeeper-archive`, of any family, and the family, which chooses the native installer of
//! A7 and A8, is read from the host's `os-release`, written here under a scratch root.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_install::announce::{
    ALWAYS, Family, Host, MAX_OS_RELEASE_BYTES, announce, announce_with, family, read_bounded,
};
use hostkeeper_release::Installer;
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// Writes `text` at `file` under `root`.
fn put(root: &Path, file: &str, text: &[u8]) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
    fs::write(path, text).expect("the file");
}

fn linux(root: &Path) -> Host<'_> {
    Host { macos: false, root }
}

/// The announcement of a Linux host whose `/etc/os-release` is `text`.
fn of_etc(name: &str, text: &str) -> (Vec<Installer>, Option<Family>) {
    let dir = scratch(name);
    put(dir.path(), "etc/os-release", text.as_bytes());
    let announced = announce(linux(dir.path()));
    (announced.installers, announced.family)
}

#[test]
fn every_host_announces_the_archive() {
    assert_eq!(ALWAYS, [Installer::HostkeeperArchive]);
    let (installers, _) = of_etc("always", "ID=debian\n");
    assert_eq!(installers, ALWAYS.to_vec());
    let dir = scratch("always-names");
    let announced = announce(linux(dir.path()));
    assert_eq!(announced.names(), vec!["hostkeeper-archive"]);
}

#[test]
fn an_id_alone_names_its_family() {
    for (id, want) in [
        ("debian", Family::Deb),
        ("ubuntu", Family::Deb),
        ("fedora", Family::Rpm),
        ("rhel", Family::Rpm),
        ("centos", Family::Rpm),
        ("amzn", Family::Rpm),
        ("suse", Family::Rpm),
        ("opensuse", Family::Rpm),
    ] {
        assert_eq!(family(&format!("ID={id}\n")), Some(want), "ID={id}");
        // A family's host announces no native installer in this build: the archive, alone.
        let (installers, found) = of_etc(&format!("id-{id}"), &format!("ID={id}\n"));
        assert_eq!(
            (installers, found),
            (ALWAYS.to_vec(), Some(want)),
            "ID={id}"
        );
    }
}

#[test]
fn a_family_only_in_id_like_is_found() {
    let cases = [
        ("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n", Family::Deb),
        (
            "ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n",
            Family::Rpm,
        ),
        ("ID=\"amzn\"\nID_LIKE=\"centos rhel fedora\"\n", Family::Rpm),
        (
            "ID=\"opensuse-leap\"\nID_LIKE=\"suse opensuse\"\n",
            Family::Rpm,
        ),
    ];
    for (text, want) in cases {
        assert_eq!(family(text), Some(want), "{text}");
    }
}

#[test]
fn manjaro_whose_arch_family_no_longer_maps_has_no_family() {
    let (installers, found) = of_etc("manjaro", "ID=manjaro\nID_LIKE=arch\n");
    assert_eq!(installers, ALWAYS.to_vec());
    assert_eq!(found, None);
}

#[test]
fn an_id_of_one_family_wins_over_an_id_like_token_of_another() {
    assert_eq!(family("ID_LIKE=fedora\nID=ubuntu\n"), Some(Family::Deb));
    assert_eq!(family("ID=fedora\nID_LIKE=\"debian\"\n"), Some(Family::Rpm));
    // The first ID_LIKE token that names a family wins, in order.
    assert_eq!(
        family("ID=x\nID_LIKE=\"arch rhel debian\"\n"),
        Some(Family::Rpm)
    );
}

#[test]
fn a_debian_host_with_rpm_on_disk_announces_no_other_installer() {
    let dir = scratch("debian-rpm");
    put(dir.path(), "etc/os-release", b"ID=debian\n");
    put(dir.path(), "usr/bin/rpm", b"");
    let announced = announce(linux(dir.path()));
    assert_eq!(announced.installers, ALWAYS.to_vec());
    assert_eq!(announced.family, Some(Family::Deb));
}

#[test]
fn quoted_and_unquoted_values_give_the_same_family() {
    for text in [
        "ID=ubuntu\n",
        "ID=\"ubuntu\"\n",
        "ID='ubuntu'\n",
        "ID=\"ub\\untu\"\n",
        "ID=x\nID_LIKE=debian\n",
        "ID=x\nID_LIKE='debian'\n",
    ] {
        assert_eq!(family(text), Some(Family::Deb), "{text}");
    }
    // A backslash in single quotes is kept as it is, so the value names nothing.
    assert_eq!(family("ID='ub\\untu'\n"), None);
}

#[test]
fn comments_blank_lines_and_other_keys_are_passed_over() {
    let text = "# a comment\n\n  \nNAME=\"Debian GNU/Linux\"\nPRETTY_NAME='x'\n  ID=debian  \n";
    assert_eq!(family(text), Some(Family::Deb));
}

#[test]
fn usr_lib_os_release_is_read_when_etc_os_release_is_missing() {
    let dir = scratch("usr-lib");
    put(dir.path(), "usr/lib/os-release", b"ID=fedora\n");
    assert_eq!(announce(linux(dir.path())).family, Some(Family::Rpm));
    // /etc/os-release, when it can be read, is the one read.
    put(dir.path(), "etc/os-release", b"ID=debian\n");
    assert_eq!(announce(linux(dir.path())).family, Some(Family::Deb));
}

#[test]
fn an_etc_os_release_that_links_to_usr_lib_is_followed() {
    let dir = scratch("etc-link");
    put(dir.path(), "usr/lib/os-release", b"ID=ubuntu\n");
    fs::create_dir_all(dir.path().join("etc")).expect("etc");
    std::os::unix::fs::symlink("../usr/lib/os-release", dir.path().join("etc/os-release"))
        .expect("the link");
    assert_eq!(announce(linux(dir.path())).family, Some(Family::Deb));
}

#[test]
fn an_unknown_id_an_empty_file_and_no_file_give_no_family() {
    assert_eq!(of_etc("unknown", "ID=plan9\n"), (ALWAYS.to_vec(), None));
    assert_eq!(of_etc("empty", ""), (ALWAYS.to_vec(), None));
    assert_eq!(
        of_etc("no-id", "NAME=x\nID_LIKE=\n"),
        (ALWAYS.to_vec(), None)
    );
    let dir = scratch("none");
    let announced = announce(linux(dir.path()));
    assert_eq!(
        (announced.installers, announced.family),
        (ALWAYS.to_vec(), None)
    );
}

#[test]
fn a_malformed_line_gives_no_family() {
    for text in [
        "ID=debian\nthis line has no equals sign\n",
        "ID=\"debian\n",
        "ID='debian\n",
        "ID=\"debian\\\n",
        "ID=\"debian\"x\n",
    ] {
        assert_eq!(family(text), None, "{text:?}");
        let (installers, found) = of_etc("malformed", text);
        assert_eq!((installers, found), (ALWAYS.to_vec(), None), "{text:?}");
    }
}

#[test]
fn a_file_too_long_or_not_utf8_is_read_as_none() {
    let dir = scratch("bounded");
    let long = dir.path().join("long");
    let mut text = b"ID=debian\n".to_vec();
    text.resize(
        usize::try_from(MAX_OS_RELEASE_BYTES).expect("fits") + 1,
        b'\n',
    );
    fs::write(&long, &text).expect("the long file");
    assert_eq!(read_bounded(&long), None);
    text.truncate(usize::try_from(MAX_OS_RELEASE_BYTES).expect("fits"));
    fs::write(&long, &text).expect("the file at the bound");
    assert!(read_bounded(&long).is_some());
    let binary = dir.path().join("binary");
    fs::write(&binary, [b'I', b'D', b'=', 0xff, b'\n']).expect("the file");
    assert_eq!(read_bounded(&binary), None);
    assert_eq!(read_bounded(&dir.path().join("missing")), None);
}

#[test]
fn a_mac_announces_the_archive_and_never_reads_os_release() {
    let dir = scratch("mac");
    put(dir.path(), "etc/os-release", b"ID=debian\n");
    let read = RefCell::new(Vec::<PathBuf>::new());
    let reader = |path: &Path| {
        read.borrow_mut().push(path.to_path_buf());
        None
    };
    let announced = announce_with(
        Host {
            macos: true,
            root: dir.path(),
        },
        &reader,
    );
    assert_eq!(announced.installers, ALWAYS.to_vec());
    assert_eq!(announced.family, None);
    assert!(read.borrow().is_empty());
    // The same root on Linux reads both files in order when neither can be read.
    announce_with(linux(dir.path()), &reader);
    assert_eq!(
        *read.borrow(),
        vec![
            dir.path().join("etc/os-release"),
            dir.path().join("usr/lib/os-release")
        ]
    );
}

#[test]
fn each_family_has_its_name() {
    assert_eq!(Family::Deb.as_str(), "deb");
    assert_eq!(Family::Rpm.as_str(), "rpm");
}
