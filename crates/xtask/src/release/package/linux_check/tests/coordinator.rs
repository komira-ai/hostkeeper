//! The check of the coordinator's deb and rpm against what the build job staged for it: the
//! program, the web app file for file, the four files of `packaging/coordinator/`, and the one
//! dependency it may declare.

use super::*;
use crate::release::package::elf::fixture::{AARCH64, Spec, X86_64};

const BINARY: &str = "./usr/bin/hostkeeper-coordinator";
const INDEX: &str = "./usr/share/hostkeeper-coordinator/ui/index.html";
const PAGE: &str = "./usr/share/hostkeeper-coordinator/ui/pages/host.js";
const UNIT: &str = "./usr/lib/systemd/system/hostkeeper-coordinator.service";

fn static_pie() -> Vec<u8> {
    Spec::static_pie(X86_64).build()
}

/// The files the coordinator's nfpm.yaml installs, with a two-file web app.
fn coordinator_files() -> Vec<File> {
    let file = |path, mode, data: Vec<u8>| File {
        path,
        mode,
        owner: "root",
        uid: 0,
        group: "root",
        directory: false,
        data,
        link: None,
    };
    vec![
        file(BINARY, 0o755, static_pie()),
        file(INDEX, 0o644, b"<html>".to_vec()),
        file(PAGE, 0o644, b"host page".to_vec()),
        file(UNIT, 0o644, b"[Unit]".to_vec()),
        file(
            "./usr/lib/sysusers.d/hostkeeper-coordinator.conf",
            0o644,
            b"u".to_vec(),
        ),
        file(
            "./usr/lib/tmpfiles.d/hostkeeper-coordinator.conf",
            0o644,
            b"d".to_vec(),
        ),
        file(
            "./usr/share/hostkeeper-coordinator/serve.toml.template",
            0o644,
            b"#".to_vec(),
        ),
    ]
}

/// `files` with the one at `path` changed by `change`.
fn changed(path: &str, change: impl FnOnce(&mut File)) -> Vec<File> {
    let mut all = coordinator_files();
    change(all.iter_mut().find(|f| f.path == path).expect("a file"));
    all
}

/// The control of a deb that declares systemd, as nfpm writes it.
const SYSTEMD: &str = "Package: hostkeeper-coordinator\nDepends: systemd\n";

fn deb_systemd(files: &[File]) -> Vec<u8> {
    deb_with_control(files, SYSTEMD)
}

fn rpm_requiring(files: &[File], requires: &[&str]) -> Vec<u8> {
    let mut tags = rpm_tags(files, "gzip");
    tags.push(strings(REQUIRENAME, requires));
    rpm_with(&tags, &gzip(&cpio_of(files)))
}

/// A directory with the staged program and web app, and `package` as `pkg`.
fn staged_case(package: &[u8]) -> TestDir {
    let dir = scratch("coordinator");
    std::fs::create_dir_all(dir.join("staged/ui/pages")).expect("staged");
    std::fs::write(dir.join("staged/hostkeeper-coordinator"), static_pie()).expect("binary");
    std::fs::write(dir.join("staged/ui/index.html"), b"<html>").expect("index");
    std::fs::write(dir.join("staged/ui/pages/host.js"), b"host page").expect("page");
    std::fs::write(dir.join("pkg"), package).expect("pkg");
    dir
}

fn run(dir: &TestDir) -> Result<Vec<String>, Error> {
    check(
        &dir.join("pkg"),
        Some(Staged::Coordinator(&dir.join("staged"))),
    )
}

fn problems(dir: &TestDir) -> Vec<String> {
    let prefix = format!("{}: ", dir.join("pkg").display());
    match run(dir) {
        Err(Error::Refused(lines)) => lines
            .iter()
            .map(|l| l.strip_prefix(&prefix).expect("named").to_owned())
            .collect(),
        other => panic!("not refused with problems: {other:?}"),
    }
}

#[test]
fn a_coordinator_deb_and_rpm_that_install_the_staged_files_and_need_systemd_pass() {
    for package in [
        deb_systemd(&coordinator_files()),
        rpm_requiring(
            &coordinator_files(),
            &["systemd", "rpmlib(CompressedFileNames)"],
        ),
    ] {
        let dir = staged_case(&package);
        let name = dir.join("pkg").display().to_string();
        assert_eq!(
            run(&dir),
            Ok(vec![
                format!(
                    "{name}: /usr/bin/hostkeeper-coordinator is 755 root:root and the staged bytes"
                ),
                format!(
                    "{name}: /usr/share/hostkeeper-coordinator/ui holds the 2 staged files, 644 root:root"
                ),
                format!(
                    "{name}: /usr/lib/systemd/system/hostkeeper-coordinator.service is 644 root:root"
                ),
                format!("{name}: /usr/lib/sysusers.d/hostkeeper-coordinator.conf is 644 root:root"),
                format!("{name}: /usr/lib/tmpfiles.d/hostkeeper-coordinator.conf is 644 root:root"),
                format!(
                    "{name}: /usr/share/hostkeeper-coordinator/serve.toml.template is 644 root:root"
                ),
                format!("{name}: /usr/bin/hostkeeper-coordinator is a static ELF program"),
                format!("{name}: declares only systemd"),
            ])
        );
    }
    // Declaring nothing passes too.
    let dir = staged_case(&deb_of(&coordinator_files()));
    let lines = run(&dir).expect("passes");
    assert_eq!(
        lines.last().map(String::as_str),
        Some(format!("{}: declares no dependency", dir.join("pkg").display()).as_str())
    );
}

#[test]
fn each_way_a_coordinator_package_differs_is_one_line() {
    let mut extra = coordinator_files();
    extra.push(File {
        path: "./usr/share/hostkeeper-coordinator/ui/stale.js",
        ..coordinator_files()[1].clone()
    });
    let missing: Vec<File> = coordinator_files()
        .into_iter()
        .filter(|f| ![BINARY, PAGE, UNIT].contains(&f.path))
        .collect();
    let cases: Vec<(Vec<File>, Vec<&str>)> = vec![
        (
            changed(BINARY, |f| f.mode = 0o775),
            vec!["/usr/bin/hostkeeper-coordinator: mode 775, not 755"],
        ),
        (
            changed(BINARY, |f| f.data = Spec::static_pie(AARCH64).build()),
            vec![
                "/usr/bin/hostkeeper-coordinator: its bytes are not those of STAGED/hostkeeper-coordinator",
            ],
        ),
        (
            changed(INDEX, |f| {
                f.owner = "nobody";
                f.mode = 0o600;
            }),
            vec![
                "/usr/share/hostkeeper-coordinator/ui/index.html: mode 600, not 644",
                "/usr/share/hostkeeper-coordinator/ui/index.html: owned by nobody:root, not root:root",
            ],
        ),
        (
            changed(PAGE, |f| f.data = b"another page".to_vec()),
            vec![
                "/usr/share/hostkeeper-coordinator/ui/pages/host.js: its bytes are not those of the staged ui/pages/host.js",
            ],
        ),
        (
            changed(UNIT, |f| f.mode = 0o755),
            vec!["/usr/lib/systemd/system/hostkeeper-coordinator.service: mode 755, not 644"],
        ),
        (
            changed(UNIT, |f| f.link = Some("/dev/null")),
            vec!["/usr/lib/systemd/system/hostkeeper-coordinator.service: not a regular file"],
        ),
        (
            extra,
            vec!["/usr/share/hostkeeper-coordinator/ui/stale.js: not a file of the staged ui/"],
        ),
        (
            missing,
            vec![
                "/usr/bin/hostkeeper-coordinator: not in the package",
                "/usr/lib/systemd/system/hostkeeper-coordinator.service: not in the package",
                "/usr/share/hostkeeper-coordinator/ui/pages/host.js: not in the package",
            ],
        ),
    ];
    for (files, want) in cases {
        for package in [deb_systemd(&files), rpm_requiring(&files, &["systemd"])] {
            let dir = staged_case(&package);
            let staged = dir.join("staged").display().to_string();
            let got = problems(&dir);
            let want: Vec<String> = want.iter().map(|w| w.replace("STAGED", &staged)).collect();
            assert_eq!(got, want);
        }
    }
}

#[test]
fn a_coordinator_package_that_declares_more_than_systemd_is_refused_one_line_per_name() {
    for package in [
        deb_with_control(
            &coordinator_files(),
            "Package: hostkeeper-coordinator\nDepends: systemd, libc6 (>= 2.39)\n",
        ),
        rpm_requiring(&coordinator_files(), &["systemd", "libc.so.6()(64bit)"]),
    ] {
        let dir = staged_case(&package);
        let got = problems(&dir);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("declares the dependency libc"),
            "{got:?}"
        );
    }
    // The daemon's packages may still declare nothing, systemd included.
    let case = Case::new(&deb_with_control(&files(), SYSTEMD));
    assert_eq!(case.problems(), ["declares the dependency systemd"]);
}

#[test]
fn a_missing_staged_program_or_web_app_is_an_error_naming_it() {
    let dir = staged_case(&deb_systemd(&coordinator_files()));
    std::fs::remove_dir_all(dir.join("staged/ui")).expect("remove");
    let message = run(&dir).expect_err("no ui").to_string();
    assert!(
        message.starts_with(&format!("read {}: ", dir.join("staged/ui").display())),
        "{message}"
    );
    std::fs::remove_file(dir.join("staged/hostkeeper-coordinator")).expect("remove");
    let message = run(&dir).expect_err("no program").to_string();
    assert!(
        message.starts_with(&format!(
            "read {}: ",
            dir.join("staged/hostkeeper-coordinator").display()
        )),
        "{message}"
    );
}
