//! Which installers a daemon announces (packages.md section 5.4; packages-tests.md, "Which
//! installers a daemon announces", the cases of this build): every host announces
//! `hostkeeper-archive`, of any family, and the family, which chooses the native installer of
//! A7 and A8, is read from the host's `os-release`, written here under a scratch root.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_install::announce::{
    ALWAYS, Family, MAX_OS_RELEASE_BYTES, OsHost, announce, announce_with, family, read_bounded,
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

fn linux(root: &Path) -> OsHost<'_> {
    OsHost { macos: false, root }
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
fn an_etc_os_release_that_cannot_be_read_falls_back_to_usr_lib() {
    let dir = scratch("etc-unreadable");
    put(dir.path(), "usr/lib/os-release", b"ID=fedora\n");
    let mut long = b"ID=debian\n".to_vec();
    long.resize(
        usize::try_from(MAX_OS_RELEASE_BYTES).expect("fits") + 1,
        b'\n',
    );
    put(dir.path(), "etc/os-release", &long);
    assert_eq!(announce(linux(dir.path())).family, Some(Family::Rpm));
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
        OsHost {
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

/// The native repository installer of A7 (packages-repositories.md section 4.1): `apt` on a
/// Debian host with its tools, an installed apt of 2.2 or later and a repository entry.
mod native {
    use super::{ALWAYS, Family, Installer, Path, announce, fs, linux, put, scratch};
    use hostkeeper_install::announce::Announcement;
    use hostkeeper_install::repository::announce::{
        APT_FLOOR, APT_TOOLS, DPKG_STATUS, announced, installed_apt, native,
    };
    use hostkeeper_install::repository::config::{Repositories, Repository};

    fn one_repository() -> Repositories {
        Repositories::new(vec![Repository {
            name: "vendor".to_owned(),
            uri: "https://repo.example.com/debian".to_owned(),
            suite: "stable".to_owned(),
            components: vec!["main".to_owned()],
            keyring: "/etc/hostkeeper/repositories/k.gpg".into(),
            text: String::new(),
        }])
    }

    /// The dpkg status stanza of an installed apt at `version`.
    fn apt_status(version: &str) -> String {
        format!(
            "Package: dpkg\nStatus: install ok installed\nVersion: 1.21.22\n\n\
             Package: apt\nStatus: install ok installed\nPriority: required\nVersion: {version}\n\n\
             Package: zlib1g\nStatus: install ok installed\nVersion: 1:1.2.13\n"
        )
    }

    /// A host root with every apt tool and a status file holding `status`.
    fn host(name: &str, status: &str) -> hostkeeper_testkit::Scratch {
        let root = scratch(name);
        for tool in APT_TOOLS {
            put(root.path(), tool, b"");
        }
        put(root.path(), DPKG_STATUS, status.as_bytes());
        root
    }

    fn native_of(family: Option<Family>, root: &Path) -> Option<Installer> {
        native(family, root, &one_repository())
    }

    /// Proves a Debian host with every apt tool, an installed apt of 2.6 and one repository
    /// gets `apt`, and that the tools are the six of apt and dpkg.
    #[test]
    fn a_debian_host_with_the_tools_a_recent_apt_and_a_repository_gets_apt() {
        let root = host("native-ok", &apt_status("2.6.1"));
        assert_eq!(
            native_of(Some(Family::Deb), root.path()),
            Some(Installer::Apt)
        );
        assert_eq!(APT_TOOLS.len(), 6);
    }

    /// Proves any one missing apt tool, taken away alone, leaves the host with no native installer.
    #[test]
    fn a_host_missing_any_one_tool_gets_none() {
        for tool in APT_TOOLS {
            let root = host("native-tool", &apt_status("2.6.1"));
            fs::remove_file(root.path().join(tool)).expect("the tool");
            assert_eq!(native_of(Some(Family::Deb), root.path()), None, "{tool}");
        }
        // A directory is not a tool.
        let root = host("native-dir", &apt_status("2.6.1"));
        fs::remove_file(root.path().join(APT_TOOLS[0])).expect("the tool");
        fs::create_dir(root.path().join(APT_TOOLS[0])).expect("a directory");
        assert_eq!(native_of(Some(Family::Deb), root.path()), None);
    }

    /// Proves the floor is apt 2.2 itself: 2.2 and later are announced, 2.1.18 and an
    /// unparseable version are not, and an epoch counts.
    #[test]
    fn the_apt_floor_is_2_2_given() {
        assert_eq!(APT_FLOOR, "2.2");
        for (version, want) in [
            ("2.2", Some(Installer::Apt)),
            ("2.2.4", Some(Installer::Apt)),
            ("2.10.0", Some(Installer::Apt)),
            ("3.0.3", Some(Installer::Apt)),
            ("1:1.0", Some(Installer::Apt)),
            ("2.1.18", None),
            ("1.8.2.3", None),
            ("2.2~rc1", None),
            ("not a version", None),
        ] {
            let root = host("native-version", &apt_status(version));
            assert_eq!(native_of(Some(Family::Deb), root.path()), want, "{version}");
        }
    }

    /// Proves a missing apt stanza, an apt that is not `install ok installed`, an apt stanza with
    /// no Version, and a missing status file all give none.
    #[test]
    fn an_apt_not_installed_or_not_found_gives_none() {
        for (name, status) in [
            (
                "none",
                "Package: dpkg\nStatus: install ok installed\nVersion: 1.21\n",
            ),
            (
                "removed",
                "Package: apt\nStatus: deinstall ok config-files\nVersion: 2.6.1\n",
            ),
            (
                "half",
                "Package: apt\nStatus: install ok half-configured\nVersion: 2.6.1\n",
            ),
            ("noversion", "Package: apt\nStatus: install ok installed\n"),
            (
                "utils",
                "Package: apt-utils\nStatus: install ok installed\nVersion: 2.6.1\n",
            ),
            ("empty", ""),
        ] {
            let root = host(name, status);
            assert_eq!(native_of(Some(Family::Deb), root.path()), None, "{name}");
            assert_eq!(
                installed_apt(&root.path().join(DPKG_STATUS)),
                None,
                "{name}"
            );
        }
        let root = host("native-nostatus", "");
        fs::remove_file(root.path().join(DPKG_STATUS)).expect("the file");
        assert_eq!(native_of(Some(Family::Deb), root.path()), None);
        assert_eq!(installed_apt(&root.path().join(DPKG_STATUS)), None);
    }

    /// Proves another package's installed state or version never leaks into apt's: the
    /// version of the stanza that names apt, and only that, is the answer.
    #[test]
    fn the_status_of_another_package_is_not_apts() {
        let status = "Package: dpkg\nStatus: install ok installed\nVersion: 9.9\n\n\
                      Package: apt\nStatus: deinstall ok config-files\nVersion: 8.8\n\n\
                      Package: apt\nStatus: install ok installed\nVersion: 2.6.1\n\n\
                      Package: other\nStatus: install ok installed\nVersion: 7.7\n";
        let root = host("native-leak", status);
        assert_eq!(
            installed_apt(&root.path().join(DPKG_STATUS)),
            Some("2.6.1".to_owned())
        );
    }

    /// Proves the status file is read whatever the order of apt's fields and with the last stanza
    /// unterminated: Version before Status, Status before Package, and no trailing blank line.
    #[test]
    fn the_status_file_is_read_in_any_field_order_and_unterminated() {
        for (name, status) in [
            (
                "vfirst",
                "Version: 2.6.1\nPackage: apt\nStatus: install ok installed\n",
            ),
            (
                "sfirst",
                "Status: install ok installed\nVersion: 2.6.1\nPackage: apt\n",
            ),
            (
                "blank-end",
                "Package: apt\nVersion: 2.6.1\nStatus: install ok installed\n\n",
            ),
            (
                "no-newline",
                "Package: apt\nVersion: 2.6.1\nStatus: install ok installed",
            ),
        ] {
            let root = host(name, status);
            assert_eq!(
                installed_apt(&root.path().join(DPKG_STATUS)),
                Some("2.6.1".to_owned()),
                "{name}"
            );
            assert_eq!(
                native_of(Some(Family::Deb), root.path()),
                Some(Installer::Apt),
                "{name}"
            );
        }
    }

    /// Proves a status file that is not UTF-8 is read as no installed apt, not a panic.
    #[test]
    fn a_status_file_that_is_not_utf8_gives_none() {
        let root = host("native-binary", "");
        put(
            root.path(),
            DPKG_STATUS,
            b"Package: apt\nStatus: install ok installed\nVersion: \xff\n",
        );
        assert_eq!(installed_apt(&root.path().join(DPKG_STATUS)), None);
    }

    /// Proves a host with no repository entry gets none, whatever else it has.
    #[test]
    fn a_host_with_no_repository_gets_none() {
        let root = host("native-norepo", &apt_status("2.6.1"));
        assert_eq!(
            native(Some(Family::Deb), root.path(), &Repositories::default()),
            None
        );
    }

    /// Proves the rpm family and a host with no family get none, even with apt's tools and status.
    #[test]
    fn the_rpm_family_and_no_family_get_none() {
        let root = host("native-rpm", &apt_status("2.6.1"));
        assert_eq!(native_of(Some(Family::Rpm), root.path()), None);
        assert_eq!(native_of(None, root.path()), None);
    }

    /// Proves a Debian host with `/usr/bin/rpm` on disk still gets `apt` alone: the family
    /// decides, not what is installed.
    #[test]
    fn a_debian_host_with_rpm_present_still_gets_apt_alone() {
        let root = host("native-rpm-on-disk", &apt_status("2.6.1"));
        put(root.path(), "etc/os-release", b"ID=debian\n");
        put(root.path(), "usr/bin/rpm", b"");
        let found = announce(linux(root.path()));
        assert_eq!(found.family, Some(Family::Deb));
        let native = native(found.family, root.path(), &one_repository());
        assert_eq!(native, Some(Installer::Apt));
        let all = announced(
            found,
            native,
            &[Installer::HostkeeperArchive, Installer::Apt],
        );
        assert_eq!(
            all.installers,
            vec![Installer::HostkeeperArchive, Installer::Apt]
        );
    }

    /// Proves the native installer is added only when the build registered it: with `apt`
    /// registered it follows the archive; without, nothing is added; `None` adds nothing.
    #[test]
    fn announced_adds_a_native_installer_only_when_registered() {
        let base = Announcement::default();
        let with = announced(
            base.clone(),
            Some(Installer::Apt),
            &[Installer::HostkeeperArchive, Installer::Apt],
        );
        assert_eq!(
            with.installers,
            vec![Installer::HostkeeperArchive, Installer::Apt]
        );
        assert_eq!(with.family, None);
        assert_eq!(with.names(), vec!["hostkeeper-archive", "apt"]);
        let without = announced(
            base.clone(),
            Some(Installer::Apt),
            &[Installer::HostkeeperArchive],
        );
        assert_eq!(without, base);
        assert_eq!(without.installers, ALWAYS.to_vec());
        let none = announced(
            base.clone(),
            None,
            &[Installer::HostkeeperArchive, Installer::Apt],
        );
        assert_eq!(none, base);
    }
}
