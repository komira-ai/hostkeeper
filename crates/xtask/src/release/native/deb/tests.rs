use std::io::Write;
use std::path::Path;

use hostkeeper_mdm_core::pins::{Pins, Tool};
use sha2::{Digest, Sha256};

use super::{
    HOSTKEEPER_REFUSED, MACOS_REFUSED, MDM_ARCHIVES, MDM_PLATFORM, NO_ARCHIVES, NO_FAIL_READY,
    Request, control, deb_with,
};
use crate::release::Error;
use crate::release::contract::{Platform, Service};
use crate::release::fakes::scratch;
use crate::release::package::elf::fixture::{AARCH64, Spec, X86_64};
use crate::release::package::linux_check::check_service;

const MTIME: u64 = 1_800_000_000;

/// The archives of every tool, each a `.tar.gz` holding the members its pin names, written into
/// `dir`, and pins of them.
fn pins_in(dir: &Path) -> Pins {
    std::fs::create_dir_all(dir).expect("the archives directory");
    let mut text = String::new();
    for tool in Tool::ALL {
        let mut builder = tar::Builder::new(Vec::new());
        for (member, name) in tool.members("1.0.0") {
            let data = format!("the {name} binary\n").into_bytes();
            let mut header = tar::Header::new_ustar();
            header.set_mode(0o755);
            header.set_size(data.len() as u64);
            builder
                .append_data(&mut header, member, data.as_slice())
                .expect("append");
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&builder.into_inner().expect("tar"))
            .expect("gzip");
        let bytes = gz.finish().expect("gzip");
        std::fs::write(dir.join(format!("{tool}.tar.gz")), &bytes).expect("write");
        let sum = hex::encode(Sha256::digest(&bytes));
        text.push_str(&format!(
            "{tool} 1.0.0 https://example.com/{tool}/v1.0.0/{tool}.tar.gz {sum}\n"
        ));
    }
    Pins::parse(&text).expect("the fixture pins")
}

fn request(dir: &Path, service: Service, platform: Platform) -> Request {
    let program = match platform {
        Platform::LinuxArm64 => Spec::static_pie(AARCH64).build(),
        _ => Spec::static_pie(X86_64).build(),
    };
    std::fs::write(dir.join("program"), program).expect("write the program");
    Request {
        service,
        platform,
        version: "0.1.5".parse().expect("a version"),
        commit: "a".repeat(40).parse().expect("a commit"),
        binary: dir.join("program"),
        fail_ready: false,
        archives: None,
        out: dir.join("out"),
    }
}

fn build(r: &Request, pins: impl FnOnce() -> Result<Pins, Error>) -> Result<Vec<String>, Error> {
    deb_with(r, || Ok(MTIME), pins)
}

fn no_pins() -> Result<Pins, Error> {
    panic!("hello's deb reads no pins")
}

#[test]
fn hellos_deb_is_named_for_its_version_and_architecture_and_passes_the_service_check() {
    // Catches: a deb named otherwise, one the service check refuses (a wrong owner or
    // mode, a unit's program not packaged), or one the reader cannot read.
    let d = scratch("native-deb-hello");
    let r = request(&d, Service::Hello, Platform::LinuxX86_64);
    let lines = build(&r, no_pins).expect("built");
    let path = d.join("out/hello_0.1.5_amd64.deb");
    let hex = crate::release::file_sha256(&path).expect("the deb");
    assert_eq!(lines, [format!("hello_0.1.5_amd64.deb: sha256 {hex}")]);
    let checked = check_service(&path, Service::Hello).expect("passes");
    assert!(
        checked.iter().any(|l| l
            .ends_with("2 units in /usr/lib/systemd/system/, each starting a file of the package")),
        "{checked:?}"
    );
}

#[test]
fn the_deb_is_the_same_bytes_for_the_same_inputs_and_dated_by_the_commit() {
    // Catches: a time, an order or a compression setting that is not fixed, so two builds of one
    // commit differ, or a date that is not the commit's.
    let d = scratch("native-deb-same");
    let r = request(&d, Service::Hello, Platform::LinuxArm64);
    build(&r, no_pins).expect("built");
    let first = std::fs::read(d.join("out/hello_0.1.5_arm64.deb")).expect("read");
    build(&r, no_pins).expect("built again");
    let second = std::fs::read(d.join("out/hello_0.1.5_arm64.deb")).expect("read");
    assert_eq!(first, second);
    deb_with(&r, || Ok(MTIME + 1), no_pins).expect("built later");
    let later = std::fs::read(d.join("out/hello_0.1.5_arm64.deb")).expect("read");
    assert_ne!(first, later);
    assert!(first.starts_with(
        b"!<arch>\ndebian-binary   1800000000  0     0     100644  4         `\n2.0\n"
    ));
    let control_at = 8 + 60 + 4;
    assert_eq!(&first[control_at..control_at + 16], b"control.tar.gz  ");
}

#[test]
fn the_failing_builds_deb_is_named_by_its_service_version_and_carries_the_tilde_version() {
    // Catches: a failing build whose control says the service version (a Debian revision that
    // sorts above the release) or whose file name carries the `~` an asset name may not.
    let d = scratch("native-deb-fail-ready");
    let mut r = request(&d, Service::Hello, Platform::LinuxArm64);
    r.fail_ready = true;
    let lines = build(&r, no_pins).expect("built");
    assert!(lines[0].starts_with("hello_0.1.5-fail-ready_arm64.deb: sha256 "));
    let path = d.join("out/hello_0.1.5-fail-ready_arm64.deb");
    let bytes = std::fs::read(&path).expect("read");
    let text = crate::channels::apt::deb::control_of(&bytes).expect("control");
    assert!(text.starts_with("Package: hello\nVersion: 0.1.5~fail.ready\nArchitecture: arm64\n"));
    assert!(!d.join("out/hello_0.1.5~fail.ready_arm64.deb").exists());
    check_service(&path, Service::Hello).expect("passes");
}

#[test]
fn pxes_deb_is_built_for_both_linux_platforms_and_holds_the_program_and_the_unit() {
    // Proves: pxe's deb is named pxe_<version>_<arch>.deb, passes the service check, installs the
    // given program at /usr/lib/pxe/pxe and one unit that starts it, and carries the pxe
    // description in its control.
    // Catches: a file other than the program and the unit, a unit whose program is not packaged,
    // an arch left unbuilt, or the hello description on pxe's package.
    for (platform, arch) in [
        (Platform::LinuxX86_64, "amd64"),
        (Platform::LinuxArm64, "arm64"),
    ] {
        let d = scratch(&format!("native-deb-pxe-{arch}"));
        let r = request(&d, Service::Pxe, platform);
        let lines = build(&r, no_pins).expect("built");
        let path = d.join(format!("out/pxe_0.1.5_{arch}.deb"));
        let hex = crate::release::file_sha256(&path).expect("the deb");
        assert_eq!(lines, [format!("pxe_0.1.5_{arch}.deb: sha256 {hex}")]);
        let checked = check_service(&path, Service::Pxe).expect("passes");
        assert!(
            checked.iter().any(|l| l.ends_with(
                "1 units in /usr/lib/systemd/system/, each starting a file of the package"
            )),
            "{checked:?}"
        );
        let bytes = std::fs::read(&path).expect("read");
        let text = crate::channels::apt::deb::control_of(&bytes).expect("control");
        assert!(
            text.starts_with(&format!(
                "Package: pxe\nVersion: 0.1.5\nArchitecture: {arch}\n"
            )),
            "{text}"
        );
        assert!(
            text.contains("Description: pxe, the boot server of network install\n"),
            "{text}"
        );
    }
}

#[test]
fn pxes_deb_is_not_built_from_a_dynamic_or_foreign_program() {
    // Proves: a pxe program linked to glibc, one for the other architecture, and bytes that are
    // not an ELF file are each refused with the reason, and nothing is written.
    // Catches: a pxe deb packing a binary that cannot run on a host without its libraries.
    let d = scratch("native-deb-pxe-not-static");
    for (platform, program, why) in [
        (
            Platform::LinuxX86_64,
            Spec::glibc().build(),
            "it asks for the interpreter /lib64/ld-linux-x86-64.so.2",
        ),
        (
            Platform::LinuxX86_64,
            Spec::static_pie(AARCH64).build(),
            "ELF machine 0xb7, not 0x3e",
        ),
        (
            Platform::LinuxArm64,
            Spec::static_pie(X86_64).build(),
            "ELF machine 0x3e, not 0xb7",
        ),
    ] {
        let r = request(&d, Service::Pxe, platform);
        std::fs::write(&r.binary, program).expect("write the program");
        let arch = if platform == Platform::LinuxArm64 {
            "arm64"
        } else {
            "amd64"
        };
        let want = format!(
            "{} is not a static linux {arch} program: {why}",
            r.binary.display()
        );
        assert_eq!(build(&r, no_pins), Err(Error::Failed(want)));
    }
    let r = request(&d, Service::Pxe, Platform::LinuxX86_64);
    std::fs::write(&r.binary, b"the program").expect("write the program");
    let e = build(&r, no_pins).expect_err("refused");
    assert!(
        e.to_string()
            .contains("is not a static linux amd64 program"),
        "{e}"
    );
    assert!(!d.join("out").exists());
}

#[test]
fn the_control_names_the_package_version_architecture_and_size_and_no_dependency() {
    // Catches: a control file dpkg refuses (no Package, Version or Architecture), a size that is
    // not rounded up to KiB, or a dependency declared.
    assert_eq!(
        control("hello", "0.1.5", "amd64", "d\n more", 1025),
        "Package: hello\nVersion: 0.1.5\nArchitecture: amd64\nMaintainer: The hostkeeper project <https://github.com/komira-ai/hostkeeper>\nInstalled-Size: 2\nSection: misc\nPriority: optional\nHomepage: https://github.com/komira-ai/hostkeeper\nDescription: d\n more\n"
    );
    assert!(control("hello", "1", "arm64", "d", 0).contains("Installed-Size: 0\n"));
}

#[test]
fn mdms_deb_carries_the_pinned_tools_and_passes_the_service_check() {
    // Catches: mdm's deb built without its tools, or one the service check refuses (its five
    // units that are not run units, say).
    let d = scratch("native-deb-mdm");
    let pins = pins_in(&d.join("archives"));
    let mut r = request(&d, Service::Mdm, Platform::LinuxX86_64);
    r.archives = Some(d.join("archives"));
    build(&r, || Ok(pins)).expect("built");
    let path = d.join("out/mdm_0.1.5_amd64.deb");
    let checked = check_service(&path, Service::Mdm).expect("passes");
    assert!(
        checked.iter().any(|l| l.contains(": 8 units in")),
        "{checked:?}"
    );
}

#[test]
fn an_archive_that_is_not_its_pin_refuses_mdms_deb_and_writes_nothing() {
    // Catches: a tool taken from an archive whose digest is not the pin's.
    let d = scratch("native-deb-mdm-pin");
    let pins = pins_in(&d.join("archives"));
    std::fs::write(d.join("archives/caddy.tar.gz"), b"other").expect("replace");
    let mut r = request(&d, Service::Mdm, Platform::LinuxX86_64);
    r.archives = Some(d.join("archives"));
    let e = build(&r, || Ok(pins)).expect_err("refused");
    assert!(
        matches!(&e, Error::Failed(why) if why.contains("caddy.tar.gz: its SHA-256 is")),
        "{e:?}"
    );
    assert!(!d.join("out").exists());
}

#[test]
fn each_request_a_deb_does_not_take_is_refused_by_name_before_anything_is_read() {
    // Catches: hostkeeper's deb (nfpm's) or a macOS deb built here, hello given archives, mdm a
    // failing build, no archives or another platform.
    let d = scratch("native-deb-refused");
    let cases: Vec<(Request, &str)> = vec![
        (
            request(&d, Service::Hostkeeper, Platform::LinuxX86_64),
            HOSTKEEPER_REFUSED,
        ),
        (
            request(&d, Service::Hello, Platform::MacosArm64),
            MACOS_REFUSED,
        ),
        (
            Request {
                archives: Some(d.join("a")),
                ..request(&d, Service::Hello, Platform::LinuxX86_64)
            },
            NO_ARCHIVES,
        ),
        (
            Request {
                fail_ready: true,
                archives: Some(d.join("a")),
                ..request(&d, Service::Mdm, Platform::LinuxX86_64)
            },
            NO_FAIL_READY,
        ),
        (
            Request {
                archives: Some(d.join("a")),
                ..request(&d, Service::Pxe, Platform::LinuxX86_64)
            },
            NO_ARCHIVES,
        ),
        (
            Request {
                fail_ready: true,
                ..request(&d, Service::Pxe, Platform::LinuxArm64)
            },
            NO_FAIL_READY,
        ),
        (
            request(&d, Service::Pxe, Platform::MacosArm64),
            MACOS_REFUSED,
        ),
        (
            request(&d, Service::Mdm, Platform::LinuxX86_64),
            MDM_ARCHIVES,
        ),
        (
            Request {
                archives: Some(d.join("a")),
                ..request(&d, Service::Mdm, Platform::LinuxArm64)
            },
            MDM_PLATFORM,
        ),
    ];
    for (r, why) in cases {
        assert_eq!(
            deb_with(&r, || panic!("no time is read"), no_pins),
            Err(Error::Failed(why.to_owned()))
        );
    }
    assert!(!d.join("out").exists());
}

#[test]
fn a_program_that_cannot_be_read_is_an_error_naming_it() {
    // Catches: a missing program packaged as empty.
    let d = scratch("native-deb-no-program");
    let mut r = request(&d, Service::Hello, Platform::LinuxX86_64);
    r.binary = d.join("absent");
    let e = build(&r, no_pins).expect_err("refused");
    assert!(
        matches!(&e, Error::Io { what, .. } if what.contains("absent")),
        "{e:?}"
    );
}

#[test]
fn a_commit_time_git_cannot_give_stops_the_build_and_writes_nothing() {
    // Catches: a deb dated by anything but the commit when git fails.
    let d = scratch("native-deb-no-time");
    let r = request(&d, Service::Hello, Platform::LinuxX86_64);
    let e = deb_with(&r, || Err(Error::Failed("no time".to_owned())), no_pins);
    assert_eq!(e, Err(Error::Failed("no time".to_owned())));
    assert!(!d.join("out").exists());
}
