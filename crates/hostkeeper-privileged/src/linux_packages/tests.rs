//! The list of releases, the finding of the packages, and the container's script and arguments.

use std::path::Path;

use hostkeeper_testkit::Scratch;

use super::{
    Format, LinuxPackagesError, MOUNT, PROGRAMS, Packages, RELEASES_TEXT, Release, container_name,
    docker_args, find_packages, parse_releases, script,
};

const DIGEST: &str = "268fb24c39bca92d326dd83ea3abebe4a3f7751c804ffef23bb6d857dd343ff8";

fn entry(name: &str, format: &str, image: &str) -> String {
    format!("[[release]]\nname = \"{name}\"\nformat = \"{format}\"\nimage = \"{image}\"\n")
}

fn pinned() -> String {
    format!("debian@sha256:{DIGEST}")
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe.parent().expect("its directory").join("privileged-lib");
    Scratch::under(&base, name).expect("create the scratch directory")
}

fn touch(dir: &Path, names: &[&str]) {
    for name in names {
        std::fs::write(dir.join(name), "").expect("write a file");
    }
}

/// Catches a committed list that lacks a format or an oldest or newest deb release, or an image
/// a moved tag could change.
#[test]
fn the_committed_list_pins_every_image_and_covers_each_format() {
    let releases = parse_releases(RELEASES_TEXT).expect("the committed list parses");
    assert_eq!(releases.len(), 9);
    for format in Format::ALL {
        assert!(releases.iter().any(|r| r.format == format), "{format:?}");
        assert!(!format.as_str().is_empty());
    }
    for name in ["debian:12", "debian:13", "ubuntu:22.04", "ubuntu:26.04"] {
        let release = releases.iter().find(|r| r.name == name).expect(name);
        assert_eq!(release.format, Format::Deb);
    }
    assert!(releases.iter().all(|r| r.image.contains("@sha256:")));
}

/// Catches a list that is read when it is not one: each defect is refused with its message.
#[test]
fn a_list_with_any_defect_is_refused() {
    let good = entry("debian:12", "deb", &pinned());
    assert_eq!(
        parse_releases(&good).expect("one release"),
        vec![Release {
            name: "debian:12".to_owned(),
            format: Format::Deb,
            image: pinned()
        }]
    );
    let digest_short = format!("debian@sha256:{}", &DIGEST[1..]);
    let digest_upper = format!("debian@sha256:{}", DIGEST.to_uppercase());
    let no_repository = format!("@sha256:{DIGEST}");
    let cases: Vec<(String, String)> = vec![
        (
            "not toml [".to_owned(),
            "the list of releases is not TOML: ".to_owned(),
        ),
        (
            format!("other = 1\n{good}"),
            "the list of releases has the key \"other\", which it does not define".to_owned(),
        ),
        (
            "release = 3".to_owned(),
            "the list's release is not an array of tables".to_owned(),
        ),
        (
            "release = [1]".to_owned(),
            "the list's release 1 is not a table".to_owned(),
        ),
        (
            format!("{good}extra = 1\n"),
            "the list of releases has the key \"release.extra\", which it does not define"
                .to_owned(),
        ),
        (
            "[[release]]\nname = \"a\"\nimage = \"x\"\n".to_owned(),
            "release 1 has no format".to_owned(),
        ),
        (
            "[[release]]\nname = 3\nformat = \"deb\"\nimage = \"x\"\n".to_owned(),
            "the list's release 1 name is not text".to_owned(),
        ),
        (
            entry("a", "apk", &pinned()),
            "release \"a\" has the format \"apk\", not deb, rpm or arch".to_owned(),
        ),
        (
            entry("a", "deb", "debian:12"),
            "release \"a\" has the image \"debian:12\", which is not pinned by digest".to_owned(),
        ),
        (
            entry("a", "deb", &digest_short),
            format!(
                "release \"a\" has the image \"{digest_short}\", which is not pinned by digest"
            ),
        ),
        (
            entry("a", "deb", &digest_upper),
            format!(
                "release \"a\" has the image \"{digest_upper}\", which is not pinned by digest"
            ),
        ),
        (
            entry("a", "deb", &no_repository),
            format!(
                "release \"a\" has the image \"{no_repository}\", which is not pinned by digest"
            ),
        ),
        (
            format!("{good}{good}"),
            "the release \"debian:12\" is listed twice".to_owned(),
        ),
        (
            "release = []".to_owned(),
            "the list of releases is empty".to_owned(),
        ),
    ];
    for (text, want) in cases {
        let err = parse_releases(&text).expect_err(&text);
        assert!(err.to_string().starts_with(&want), "{text:?}: {err}");
    }
}

/// Catches packages taken from a directory that holds none, two, or a name a script cannot take:
/// each fails by the format's suffix, and the one of each format is found.
#[test]
fn the_one_package_of_each_format_is_found_and_anything_else_fails_by_name() {
    let dir = scratch("find");
    let missing = dir.path().join("missing");
    let err = find_packages(&missing).expect_err("no directory");
    assert!(matches!(err, LinuxPackagesError::Unreadable { .. }));
    assert!(
        err.to_string()
            .starts_with(&format!("{}: cannot list", missing.display()))
    );

    touch(dir.path(), &["a.deb", "a.rpm", "README"]);
    let err = find_packages(dir.path()).expect_err("no arch package");
    assert_eq!(
        err.to_string(),
        format!(
            "{} holds 0 files ending .pkg.tar.zst, not one: build the packages as the job's steps do",
            dir.path().display()
        )
    );
    touch(dir.path(), &["a.pkg.tar.zst", "b.deb"]);
    let err = find_packages(dir.path()).expect_err("two debs");
    assert_eq!(
        err,
        LinuxPackagesError::PackageCount {
            dir: dir.path().to_path_buf(),
            suffix: ".deb",
            found: 2
        }
    );
    std::fs::remove_file(dir.path().join("b.deb")).expect("remove");
    let found = find_packages(dir.path()).expect("one of each");
    assert_eq!(found.file_name(Format::Deb), "a.deb");
    assert_eq!(found.file_name(Format::Rpm), "a.rpm");
    assert_eq!(found.file_name(Format::Arch), "a.pkg.tar.zst");

    let odd = scratch("find-odd");
    touch(odd.path(), &["a b.deb", "a.rpm", "a.pkg.tar.zst"]);
    let err = find_packages(odd.path()).expect_err("an odd name");
    assert_eq!(err, LinuxPackagesError::BadFileName("a b.deb".to_owned()));
    assert_eq!(
        err.to_string(),
        "the package file name \"a b.deb\" holds a character other than letters, digits and `._+-`"
    );
}

/// Catches a container that installs with another tool than its format's, skips a program, or
/// does not put a stand-in for the systemd tools an image lacks.
#[test]
fn the_script_installs_with_the_formats_tool_and_runs_each_program() {
    let cases = [
        (Format::Deb, "dpkg -i /packages/h.deb >&2"),
        (Format::Rpm, "rpm -i /packages/h.rpm >&2"),
        (
            Format::Arch,
            "pacman -U --noconfirm /packages/h.pkg.tar.zst >&2",
        ),
    ];
    for (format, install) in cases {
        let file = format!("h{}", format.suffix());
        let lines: Vec<String> = script(format, &file).lines().map(str::to_owned).collect();
        assert_eq!(lines[0], "set -eu");
        assert!(lines[1].contains("command -v \"$tool\""), "{}", lines[1]);
        assert!(
            lines[1].contains("systemctl systemd-tmpfiles"),
            "{}",
            lines[1]
        );
        assert_eq!(lines[2], install);
        assert_eq!(
            lines[3..],
            [
                "/usr/bin/hostkeeper --version",
                "/usr/bin/hostkeeper-pxe --version",
                "/usr/lib/hostkeeper/gh --version"
            ]
        );
    }
    assert_eq!(
        PROGRAMS.map(|p| p.1),
        ["hostkeeper", "hostkeeper-pxe", "gh"]
    );
}

/// Catches a container that may pull, reach a network, write the packages, or share its name with
/// another run's.
#[test]
fn the_docker_run_pulls_nothing_has_no_network_and_mounts_the_packages_read_only() {
    let packages = Packages {
        dir: "/t/linux-packages".into(),
        deb: "h.deb".to_owned(),
        rpm: "h.rpm".to_owned(),
        arch: "h.pkg.tar.zst".to_owned(),
    };
    let release = Release {
        name: "debian:12".to_owned(),
        format: Format::Deb,
        image: pinned(),
    };
    let name = container_name(7, 2);
    assert_eq!(name, "hostkeeper-ci-linux-packages-7-2");
    assert_ne!(name, container_name(8, 2));
    assert_ne!(name, container_name(7, 3));
    let args = docker_args(&name, &release, &packages);
    assert_eq!(
        args[..11],
        [
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--name",
            "hostkeeper-ci-linux-packages-7-2",
            "-v",
            "/t/linux-packages:/packages:ro",
            &pinned(),
            "/bin/sh",
            "-c"
        ]
    );
    assert_eq!(args[11], script(Format::Deb, "h.deb"));
    assert_eq!(args.len(), 12);
    assert_eq!(MOUNT, "/packages");
}
