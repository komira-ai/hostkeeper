use std::io::Write;
use std::path::Path;

use hostkeeper_mdm_core::pins::{Pins, Tool};
use sha2::{Digest, Sha256};

use super::{
    HELLO_ARCHIVES, HOSTKEEPER_REFUSED, MACOS_REFUSED, MDM_ARCHIVES, MDM_FAIL_READY, MDM_PLATFORM,
    Request, control, deb_with,
};
use crate::release::Error;
use crate::release::contract::{Platform, Service};
use crate::release::fakes::scratch;
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
    std::fs::write(dir.join("program"), b"the program").expect("write the program");
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
    // Catches: a deb named otherwise, one the service check refuses (a script, a path, a
    // missing hook, a unit's program not packaged), or one the reader cannot read.
    let d = scratch("native-deb-hello");
    let r = request(&d, Service::Hello, Platform::LinuxX86_64);
    let lines = build(&r, no_pins).expect("built");
    let path = d.join("out/hello_0.1.5_amd64.deb");
    let hex = crate::release::file_sha256(&path).expect("the deb");
    assert_eq!(lines, [format!("hello_0.1.5_amd64.deb: sha256 {hex}")]);
    let checked = check_service(&path, Service::Hello).expect("passes");
    assert!(
        checked.iter().any(|l| l.ends_with(
            "2 units in /usr/lib/systemd/system/, none enabled, each starting a file of the package"
        )),
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
fn the_failing_builds_deb_carries_the_fail_ready_version() {
    // Catches: a failing build named or versioned as the good one, which apt could not tell
    // apart from it.
    let d = scratch("native-deb-fail-ready");
    let mut r = request(&d, Service::Hello, Platform::LinuxArm64);
    r.fail_ready = true;
    let lines = build(&r, no_pins).expect("built");
    assert!(lines[0].starts_with("hello_0.1.5-fail-ready_arm64.deb: sha256 "));
    let path = d.join("out/hello_0.1.5-fail-ready_arm64.deb");
    check_service(&path, Service::Hello).expect("passes");
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
            HELLO_ARCHIVES,
        ),
        (
            Request {
                fail_ready: true,
                archives: Some(d.join("a")),
                ..request(&d, Service::Mdm, Platform::LinuxX86_64)
            },
            MDM_FAIL_READY,
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
