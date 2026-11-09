//! The check of a service's `.deb`, on debs laid out here as `release deb` lays them out, each
//! changed in the one way a case names.

use std::io::Write;

use super::{HOSTKEEPER, check_service};
use crate::release::Error;
use crate::release::contract::Service;
use crate::release::native::{deb, hello, mdm};
use crate::testdir::TestDir;

fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-service-check-unit"),
        name,
    )
}

/// One entry of a fixture tar.
#[derive(Clone)]
struct Ent {
    path: String,
    kind: tar::EntryType,
    mode: u32,
    uid: u64,
    owner: &'static str,
    data: Vec<u8>,
    link: Option<&'static str>,
}

fn file(path: &str, mode: u32, data: &[u8]) -> Ent {
    Ent {
        path: path.to_owned(),
        kind: tar::EntryType::Regular,
        mode,
        uid: 0,
        owner: "root",
        data: data.to_vec(),
        link: None,
    }
}

fn dir(path: &str) -> Ent {
    Ent {
        kind: tar::EntryType::Directory,
        ..file(path, 0o755, b"")
    }
}

fn tar(entries: &[Ent]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(e.kind);
        if let Some(target) = e.link {
            h.set_link_name(target).expect("target");
        }
        h.set_mode(e.mode);
        h.set_uid(e.uid);
        h.set_gid(0);
        h.set_username(e.owner).expect("owner");
        h.set_groupname("root").expect("group");
        h.set_size(e.data.len() as u64);
        b.append_data(&mut h, &e.path, e.data.as_slice())
            .expect("entry");
    }
    b.into_inner().expect("tar")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).expect("gzip");
    e.finish().expect("gzip")
}

fn ar(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        out.extend(
            format!(
                "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                0,
                100_644,
                data.len()
            )
            .as_bytes(),
        );
        out.extend(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// hello's good deb, as entries: the control archive's and the data archive's.
fn hello_entries() -> (Vec<Ent>, Vec<Ent>) {
    let control = vec![
        dir("./"),
        file(
            "./control",
            0o644,
            b"Package: hello\nVersion: 1.0.0\nArchitecture: amd64\n",
        ),
    ];
    let mut data = vec![
        dir("./"),
        dir("./usr/"),
        dir("./usr/lib/"),
        dir("./usr/lib/hello/"),
        dir("./usr/lib/systemd/"),
        dir("./usr/lib/systemd/system/"),
    ];
    data.extend(
        hello::deb_files(b"the program".to_vec(), false)
            .into_iter()
            .map(|f| file(&format!(".{}", f.path), f.mode, &f.data)),
    );
    (control, data)
}

fn deb_of(control: &[Ent], data: &[Ent]) -> Vec<u8> {
    ar(&[
        ("debian-binary", b"2.0\n".to_vec()),
        ("control.tar.gz", gzip(&tar(control))),
        ("data.tar.gz", gzip(&tar(data))),
    ])
}

/// Checks `bytes` as hello's deb.
fn check(name: &str, bytes: &[u8]) -> Result<Vec<String>, Error> {
    let d = scratch(name);
    let path = d.join("p.deb");
    std::fs::write(&path, bytes).expect("write");
    check_service(&path, Service::Hello).map_err(|e| match e {
        Error::Refused(lines) => Error::Refused(
            lines
                .into_iter()
                .map(|l| l.replacen(&format!("{}: ", path.display()), "", 1))
                .collect(),
        ),
        other => other,
    })
}

fn refused(name: &str, bytes: &[u8]) -> Vec<String> {
    match check(name, bytes) {
        Err(Error::Refused(lines)) => lines,
        other => panic!("{name}: not refused: {other:?}"),
    }
}

#[test]
fn hellos_and_mdms_debs_as_release_deb_writes_them_pass() {
    // Catches: a check that refuses the packages this repository builds.
    let (control, data) = hello_entries();
    check("good", &deb_of(&control, &data)).expect("the fixture passes");
    let d = scratch("built");
    let files = hello::deb_files(b"p".to_vec(), true);
    let bytes = deb::build("hello", "1.0.0-fail-ready", "arm64", "d", &files, 7).expect("built");
    std::fs::write(d.join("h.deb"), bytes).expect("write");
    check_service(&d.join("h.deb"), Service::Hello).expect("hello's passes");
    let tools = [
        "caddy",
        "depserver",
        "depsyncer",
        "lego",
        "nanohub",
        "scepserver",
    ]
    .map(|name| (name.to_owned(), name.as_bytes().to_vec()))
    .to_vec();
    let files = mdm::deb_files(b"m".to_vec(), tools[1..].to_vec());
    let bytes = deb::build("mdm", "1.0.0", "amd64", "d", &files, 7).expect("built");
    std::fs::write(d.join("m-no-caddy.deb"), bytes).expect("write");
    assert_eq!(
        check_service(&d.join("m-no-caddy.deb"), Service::Mdm),
        Err(Error::Refused(vec![format!(
            "{}: /usr/lib/systemd/system/mdm-proxy.service: ExecStart= runs /usr/lib/mdm/caddy, which is not a file of the package",
            d.join("m-no-caddy.deb").display()
        )]))
    );
    let files = mdm::deb_files(b"m".to_vec(), tools);
    let bytes = deb::build("mdm", "1.0.0", "amd64", "d", &files, 7).expect("built");
    std::fs::write(d.join("m.deb"), bytes).expect("write");
    check_service(&d.join("m.deb"), Service::Mdm).expect("mdm's passes, its eight units with it");
}

#[test]
fn a_deb_with_a_postinst_or_any_other_maintainer_script_is_refused() {
    // Catches: a deb whose control archive runs code as root at install, upgrade or removal.
    for script in [
        "postinst", "preinst", "prerm", "postrm", "config", "triggers",
    ] {
        let (mut control, data) = hello_entries();
        control.push(file(
            &format!("./{script}"),
            0o755,
            b"#!/bin/sh\nsystemctl enable hello@a\n",
        ));
        assert_eq!(
            refused(script, &deb_of(&control, &data)),
            [format!(
                "control.tar.gz holds {script}: a service's .deb holds only control, md5sums and conffiles there, and no maintainer script"
            )]
        );
    }
    let (mut control, data) = hello_entries();
    control.push(file("./md5sums", 0o644, b""));
    control.push(file("./conffiles", 0o644, b""));
    check("md5sums", &deb_of(&control, &data)).expect("md5sums and conffiles are allowed");
}

#[test]
fn an_enablement_link_a_drop_in_or_a_unit_of_another_name_is_refused() {
    // Catches: a deb that enables its own unit at install, changes a unit of the host through a
    // drop-in, shadows one, or installs a link in the unit directory.
    let cases: Vec<(Ent, &str)> = vec![
        (
            Ent {
                kind: tar::EntryType::Symlink,
                link: Some("/usr/lib/systemd/system/hello@a.service"),
                ..file(
                    "./etc/systemd/system/multi-user.target.wants/hello@a.service",
                    0o777,
                    b"",
                )
            },
            "/etc/systemd/system/multi-user.target.wants/hello@a.service: outside the paths",
        ),
        (
            dir("./usr/lib/systemd/system/ssh.service.d/"),
            "/usr/lib/systemd/system/ssh.service.d: in /usr/lib/systemd/system/",
        ),
        (
            file("./usr/lib/systemd/system/ssh.service", 0o644, b""),
            "/usr/lib/systemd/system/ssh.service: in /usr/lib/systemd/system/",
        ),
        (
            Ent {
                kind: tar::EntryType::Symlink,
                link: Some("hello@a.service"),
                ..file("./usr/lib/systemd/system/hello.service", 0o777, b"")
            },
            "/usr/lib/systemd/system/hello.service: in /usr/lib/systemd/system/",
        ),
        (
            file("./etc/cron.d/hello", 0o644, b""),
            "/etc/cron.d/hello: outside the paths",
        ),
    ];
    for (n, (entry, words)) in cases.into_iter().enumerate() {
        let (control, mut data) = hello_entries();
        data.push(entry);
        let lines = refused(&format!("link-{n}"), &deb_of(&control, &data));
        assert!(
            lines.iter().any(|l| l.starts_with(words)),
            "{words}: {lines:?}"
        );
    }
}

#[test]
fn a_missing_or_unrunnable_hook_or_a_unit_starting_a_program_not_packaged_is_refused() {
    // Catches: a deb whose hook the daemon cannot run (missing, a directory, not executable), or
    // whose unit starts a program at a path the package does not install (the archive's
    // current, say).
    let hook = "./usr/lib/hello/hostkeeper-hook";
    let (control, data) = hello_entries();
    let without: Vec<Ent> = data.iter().filter(|e| e.path != hook).cloned().collect();
    assert_eq!(
        refused("no-hook", &deb_of(&control, &without)),
        ["/usr/lib/hello/hostkeeper-hook: not in the package: the hook is at that path"]
    );
    let mut as_dir = without.clone();
    as_dir.push(dir(&format!("{hook}/")));
    assert_eq!(
        refused("hook-dir", &deb_of(&control, &as_dir)),
        ["/usr/lib/hello/hostkeeper-hook: not a regular file"]
    );
    let mut not_exec = without.clone();
    not_exec.push(file(hook, 0o644, b"#!/bin/sh\n"));
    assert_eq!(
        refused("hook-mode", &deb_of(&control, &not_exec)),
        ["/usr/lib/hello/hostkeeper-hook: mode 644, not 755"]
    );
    let mut stale = data.clone();
    let unit = stale
        .iter_mut()
        .find(|e| e.path.ends_with("hello@a.service"))
        .expect("a unit");
    unit.data =
        b"[Service]\nExecStart=-/var/lib/hostkeeper/packages/hello/current/bin/hello\n".to_vec();
    assert_eq!(
        refused("stale-unit", &deb_of(&control, &stale)),
        [
            "/usr/lib/systemd/system/hello@a.service: ExecStart= runs /var/lib/hostkeeper/packages/hello/current/bin/hello, which is not a file of the package"
        ]
    );
}

#[test]
fn an_entry_not_roots_writable_by_others_or_setuid_and_a_device_are_refused() {
    // Catches: a file another account owns or may change, a setuid program, and a fifo.
    let (control, data) = hello_entries();
    let mut changed = data.clone();
    let program = changed
        .iter_mut()
        .find(|e| e.path == "./usr/lib/hello/hello")
        .expect("program");
    program.uid = 1000;
    program.owner = "build";
    program.mode = 0o4775;
    changed.push(Ent {
        kind: tar::EntryType::Fifo,
        ..file("./usr/lib/hello/fifo", 0o644, b"")
    });
    assert_eq!(
        refused("owners", &deb_of(&control, &changed)),
        [
            "/usr/lib/hello/fifo: neither a directory, a regular file nor a link",
            "/usr/lib/hello/hello: owned by id 1000:root, not root:root",
            "/usr/lib/hello/hello: mode 4775 has a setuid, setgid or sticky bit",
            "/usr/lib/hello/hello: mode 4775 lets a group or others write it",
        ]
    );
}

#[test]
fn another_package_name_or_a_dependency_is_refused() {
    // Catches: a deb of another package checked as the service's, and one that pulls in other
    // packages, whose scripts would run at install.
    let (_, data) = hello_entries();
    let control = vec![
        dir("./"),
        file(
            "./control",
            0o644,
            b"Package: hello2\nVersion: 1\nDepends: libc6, curl\n",
        ),
    ];
    assert_eq!(
        refused("name", &deb_of(&control, &data)),
        [
            "control: Package is \"hello2\", not \"hello\"",
            "declares the dependency libc6",
            "declares the dependency curl",
        ]
    );
}

#[test]
fn a_package_that_is_not_a_deb_and_hostkeeper_are_refused_by_name() {
    // Catches: --service applied to an rpm, a tarball or hostkeeper's packages.
    let d = scratch("not-deb");
    std::fs::write(d.join("t.tar.gz"), gzip(&tar(&[dir("./")]))).expect("write");
    let e = check_service(&d.join("t.tar.gz"), Service::Hello).expect_err("refused");
    assert_eq!(
        e,
        Error::Failed(
            "not a package this check reads: --service reads a .deb, and this is not one"
                .to_owned()
        )
    );
    assert_eq!(
        check_service(&d.join("t.tar.gz"), Service::Hostkeeper),
        Err(Error::Failed(HOSTKEEPER.to_owned()))
    );
    let e = check_service(&d.join("absent.deb"), Service::Hello).expect_err("missing");
    assert!(matches!(e, Error::Io { .. }), "{e:?}");
}
