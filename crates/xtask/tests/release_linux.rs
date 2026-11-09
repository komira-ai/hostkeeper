//! `cargo xtask release check-linux-pkg` and `release nfpm-stage --binaries`, run as the built
//! program on packages and programs crafted byte by byte: a deb, an rpm and a tarball, holding a static-pie program (accepted), a program linked to `libssl.so.3` or built
//! for glibc (refused), or declaring a dependency (refused). The exit status is 0 for an
//! accepted package, 1 for a refused one and 2 for a usage error. No toolchain, no network.

#[path = "../src/testdir.rs"]
mod testdir;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::Digest;
use testdir::TestDir;

const GLIBC_INTERPRETER: &str = "/lib64/ld-linux-x86-64.so.2";

fn dir(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("release-linux-{name}"),
    )
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run xtask")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

/// A 64-bit little-endian x86-64 ELF program: a `PT_LOAD` over the whole file, a `PT_DYNAMIC`
/// with a string table, `needed` as its `DT_NEEDED` libraries and `interpreter` as its
/// `PT_INTERP`. With neither it is the shape of a static-pie.
fn elf(interpreter: Option<&str>, needed: &[&str]) -> Vec<u8> {
    let phnum = 2 + usize::from(interpreter.is_some());
    let interp_at = 64 + 56 * phnum;
    let mut interp = interpreter
        .map(|i| i.as_bytes().to_vec())
        .unwrap_or_default();
    if !interp.is_empty() {
        interp.push(0);
    }
    let mut tail = interp.clone();
    let strtab_at = interp_at + tail.len();
    let mut strtab = vec![0];
    let mut names = Vec::new();
    for name in needed {
        names.push(strtab.len() as u64);
        strtab.extend(name.as_bytes());
        strtab.push(0);
    }
    tail.extend(&strtab);
    let dynamic_at = interp_at + tail.len();
    let mut dynamic = Vec::new();
    for (tag, value) in names
        .iter()
        .map(|n| (1u64, *n))
        .chain([(5, 0x1000 + strtab_at as u64), (0, 0)])
    {
        dynamic.extend(tag.to_le_bytes());
        dynamic.extend(value.to_le_bytes());
    }
    let total = dynamic_at + dynamic.len();
    let mut out = b"\x7fELF\x02\x01\x01".to_vec();
    out.resize(16, 0);
    out.extend([3, 0]);
    out.extend(0x3eu16.to_le_bytes());
    out.extend(1u32.to_le_bytes());
    out.extend(0u64.to_le_bytes());
    out.extend(64u64.to_le_bytes());
    out.extend(0u64.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(64u16.to_le_bytes());
    out.extend(56u16.to_le_bytes());
    out.extend((phnum as u16).to_le_bytes());
    out.extend([0; 6]);
    let mut segment = |kind: u32, offset: usize, size: usize| {
        out.extend(kind.to_le_bytes());
        out.extend(5u32.to_le_bytes());
        for value in [offset, 0x1000 + offset, 0x1000 + offset, size, size, 8] {
            out.extend((value as u64).to_le_bytes());
        }
    };
    segment(1, 0, total);
    if interpreter.is_some() {
        segment(3, interp_at, interp.len());
    }
    segment(2, dynamic_at, dynamic.len());
    out.extend(tail);
    out.extend(dynamic);
    out
}

/// What the musl build makes.
fn static_pie() -> Vec<u8> {
    elf(None, &[])
}

/// One regular file of a package: its installed path, mode and bytes.
struct Entry {
    path: &'static str,
    mode: u32,
    data: Vec<u8>,
}

fn entry(path: &'static str, mode: u32, data: Vec<u8>) -> Entry {
    Entry { path, mode, data }
}

/// The two programs, as given.
fn programs(hostkeeper: Vec<u8>, pxe: Vec<u8>) -> Vec<Entry> {
    vec![
        entry("usr/bin/hostkeeper", 0o755, hostkeeper),
        entry("usr/bin/hostkeeper-pxe", 0o755, pxe),
    ]
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).expect("gzip");
    e.finish().expect("gzip")
}

/// A tar of `entries` owned by uid and gid 0 with the names `owner` and `group`, and `link`
/// as the console's issue link.
fn tar(entries: &[Entry], owner: &str, link: bool) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let mut h = tar::Header::new_gnu();
        h.set_mode(e.mode);
        h.set_uid(0);
        h.set_gid(0);
        h.set_username(owner).expect("owner");
        h.set_groupname(owner).expect("group");
        h.set_size(e.data.len() as u64);
        b.append_data(&mut h, format!("./{}", e.path), e.data.as_slice())
            .expect("entry");
    }
    if link {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_mode(0o777);
        h.set_uid(0);
        h.set_gid(0);
        h.set_username(owner).expect("owner");
        h.set_groupname(owner).expect("group");
        h.set_size(0);
        h.set_link_name("/run/hostkeeper/enrollment.issue")
            .expect("link");
        b.append_data(&mut h, "./etc/issue.d/hostkeeper.issue", &b""[..])
            .expect("link");
    }
    b.into_inner().expect("tar")
}

/// A deb of `entries` whose `control` is `control`.
fn deb(entries: &[Entry], control: &str, link: bool) -> Vec<u8> {
    let control_file = [entry("control", 0o644, control.as_bytes().to_vec())];
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in [
        ("debian-binary", b"2.0\n".to_vec()),
        ("control.tar.gz", gzip(&tar(&control_file, "root", false))),
        ("data.tar.gz", gzip(&tar(entries, "root", link))),
    ] {
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
        out.extend(&data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// A gzip tarball of `entries`.
fn tarball(entries: &[Entry]) -> Vec<u8> {
    gzip(&tar(entries, "", false))
}

/// One tag of an rpm header: number, type, count and data.
type Tag = (u32, u32, u32, Vec<u8>);

fn strings(tag: u32, items: &[String]) -> Tag {
    let mut data = Vec::new();
    for item in items {
        data.extend(item.as_bytes());
        data.push(0);
    }
    (tag, 8, items.len() as u32, data)
}

fn header(tags: &[Tag]) -> Vec<u8> {
    let (mut index, mut store) = (Vec::new(), Vec::<u8>::new());
    for (tag, kind, count, data) in tags {
        for n in [*tag, *kind, store.len() as u32, *count] {
            index.extend(n.to_be_bytes());
        }
        store.extend(data);
    }
    let mut out = vec![0x8e, 0xad, 0xe8, 0x01, 0, 0, 0, 0];
    out.extend((tags.len() as u32).to_be_bytes());
    out.extend((store.len() as u32).to_be_bytes());
    out.extend(index);
    out.extend(store);
    out
}

/// A newc cpio of `entries`, with its trailer.
fn cpio(entries: &[(String, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries
        .iter()
        .map(|(n, d)| (n.as_str(), *d))
        .chain([("TRAILER!!!", &b""[..])])
    {
        out.extend(b"070701");
        for field in [
            0,
            0o100_644,
            0,
            0,
            1,
            0,
            data.len(),
            0,
            0,
            0,
            0,
            name.len() + 1,
            0,
        ] {
            out.extend(format!("{field:08x}").as_bytes());
        }
        out.extend(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

/// An rpm of `entries` (and the issue link when `link`) requiring `requires`.
fn rpm(entries: &[Entry], requires: &[&str], link: bool) -> Vec<u8> {
    let mut all: Vec<(String, u32, Vec<u8>)> = entries
        .iter()
        .map(|e| (format!("/{}", e.path), 0o100_000 | e.mode, e.data.clone()))
        .collect();
    if link {
        let target = b"/run/hostkeeper/enrollment.issue".to_vec();
        all.push(("/etc/issue.d/hostkeeper.issue".into(), 0o120_777, target));
    }
    let mut dirs: Vec<String> = Vec::new();
    let (mut bases, mut indexes, mut modes, mut targets) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (path, mode, data) in &all {
        let (dir, base) = path.split_at(path.rfind('/').expect("a slash") + 1);
        let at = dirs.iter().position(|d| d == dir).unwrap_or_else(|| {
            dirs.push(dir.to_owned());
            dirs.len() - 1
        });
        bases.push(base.to_owned());
        indexes.extend((at as u32).to_be_bytes());
        modes.extend((*mode as u16).to_be_bytes());
        targets.push(if mode & 0o170_000 == 0o120_000 {
            String::from_utf8_lossy(data).into_owned()
        } else {
            String::new()
        });
    }
    let roots = vec!["root".to_owned(); all.len()];
    let mut tags = vec![
        strings(1036, &targets),
        (1030, 3, all.len() as u32, modes),
        strings(1039, &roots),
        strings(1040, &roots),
        (1116, 4, all.len() as u32, indexes),
        strings(1117, &bases),
        strings(1118, &dirs),
        (1125, 6, 1, b"gzip\0".to_vec()),
    ];
    if !requires.is_empty() {
        let items: Vec<String> = requires.iter().map(|r| (*r).to_owned()).collect();
        tags.push(strings(1049, &items));
    }
    let payload: Vec<(String, &[u8])> = all
        .iter()
        .map(|(path, _, data)| (format!(".{path}"), data.as_slice()))
        .collect();
    let mut out = vec![0xed, 0xab, 0xee, 0xdb];
    out.resize(96, 0);
    out.extend(header(&[(1000, 7, 4, b"sign".to_vec())]));
    while out.len() % 8 != 0 {
        out.push(0);
    }
    out.extend(header(&tags));
    out.extend(gzip(&cpio(&payload)));
    out
}

fn write(dir: &TestDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write");
    path
}

/// The three formats of a package holding `entries`, declaring nothing, as `(name, bytes)`.
fn formats(entries: &[Entry]) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hostkeeper.deb",
            deb(entries, "Package: hostkeeper\n", false),
        ),
        ("hostkeeper.rpm", rpm(entries, &[], false)),
        ("hostkeeper.tar.gz", tarball(entries)),
    ]
}

#[test]
fn a_static_build_is_accepted_in_a_deb_an_rpm_and_a_tarball() {
    let d = dir("accept");
    for (name, bytes) in formats(&programs(static_pie(), static_pie())) {
        let package = write(&d, name, &bytes);
        let o = run(&d, &["release", "check-linux-pkg", path_str(&package)]);
        assert_eq!(o.status.code(), Some(0), "{name}: {}", stderr(&o));
        let out = stdout(&o);
        for program in ["/usr/bin/hostkeeper", "/usr/bin/hostkeeper-pxe"] {
            assert!(
                out.contains(&format!("{program} is a static ELF program")),
                "{name}: {out}"
            );
        }
        assert_eq!(
            out.contains("declares no dependency"),
            name != "hostkeeper.tar.gz",
            "{name}: {out}"
        );
    }
}

#[test]
fn a_program_linked_to_libssl_is_refused_in_each_format() {
    let d = dir("libssl");
    let linked = elf(None, &["libssl.so.3"]);
    for (name, bytes) in formats(&programs(linked, static_pie())) {
        let package = write(&d, name, &bytes);
        let o = run(&d, &["release", "check-linux-pkg", path_str(&package)]);
        assert_eq!(o.status.code(), Some(1), "{name}: {}", stdout(&o));
        let err = stderr(&o);
        assert!(
            err.contains(&format!(
                "{}: /usr/bin/hostkeeper: ELF program linked to libssl.so.3",
                package.display()
            )),
            "{name}: {err}"
        );
        assert!(!err.contains("hostkeeper-pxe"), "{name}: {err}");
    }
}

#[test]
fn a_tarball_holding_a_glibc_build_is_refused() {
    let d = dir("glibc");
    let glibc = elf(Some(GLIBC_INTERPRETER), &["libc.so.6"]);
    let package = write(
        &d,
        "hostkeeper.tar.gz",
        &tarball(&programs(glibc, static_pie())),
    );
    let o = run(&d, &["release", "check-linux-pkg", path_str(&package)]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    let err = stderr(&o);
    assert!(
        err.contains(&format!(
            "/usr/bin/hostkeeper: ELF program with the interpreter {GLIBC_INTERPRETER}, so not static"
        )),
        "{err}"
    );
    assert!(
        err.contains("/usr/bin/hostkeeper: ELF program linked to libc.so.6"),
        "{err}"
    );
}

#[test]
fn a_package_that_declares_a_dependency_is_refused() {
    let d = dir("depends");
    let entries = programs(static_pie(), static_pie());
    let cases = [
        (
            "hostkeeper.deb",
            deb(
                &entries,
                "Package: hostkeeper\nDepends: libc6 (>= 2.34)\n",
                false,
            ),
            "declares the dependency libc6 (>= 2.34)",
        ),
        (
            "hostkeeper.rpm",
            rpm(
                &entries,
                &["rpmlib(CompressedFileNames)", "glibc >= 2.34"],
                false,
            ),
            "declares the dependency glibc >= 2.34",
        ),
    ];
    for (name, bytes, line) in cases {
        let package = write(&d, name, &bytes);
        let o = run(&d, &["release", "check-linux-pkg", path_str(&package)]);
        assert_eq!(o.status.code(), Some(1), "{name}: {}", stdout(&o));
        let want = format!("{}: {line}", package.display());
        assert!(stderr(&o).contains(&want), "{name}: {}", stderr(&o));
    }
}

#[test]
fn staged_with_a_tarball_is_a_failure_and_without_a_package_a_usage_error() {
    let d = dir("usage");
    let package = write(
        &d,
        "hostkeeper.tar.gz",
        &tarball(&programs(static_pie(), static_pie())),
    );
    let o = run(
        &d,
        &[
            "release",
            "check-linux-pkg",
            path_str(&package),
            "--staged",
            path_str(&d),
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains("--staged does not apply to a tarball"),
        "{}",
        stderr(&o)
    );
    let o = run(&d, &["release", "check-linux-pkg"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    let o = run(&d, &["release", "check-linux-pkg", "absent.deb"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A gzip tar like `gh`'s release archive holding `gh` at its member for version 2.0.0.
fn gh_archive(gh: &[u8]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_mode(0o755);
    h.set_size(gh.len() as u64);
    b.append_data(&mut h, "gh_2.0.0_linux_amd64/bin/gh", gh)
        .expect("gh");
    gzip(&b.into_inner().expect("tar"))
}

const TRUSTED_ROOT: &str =
    "{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json;version=0.1\"}\n";

/// A directory holding gh's archive, a trusted root and the two programs, and the arguments of
/// `release nfpm-stage` for them.
struct Stage {
    dir: TestDir,
    binaries: PathBuf,
    args: Vec<String>,
}

impl Stage {
    fn new(name: &str, hostkeeper: &[u8], pxe: &[u8]) -> Stage {
        let dir = dir(name);
        let gh = elf(None, &[]);
        let archive_bytes = gh_archive(&gh);
        let archive = write(&dir, "gh.tar.gz", &archive_bytes);
        let root = write(&dir, "trusted_root.jsonl", TRUSTED_ROOT.as_bytes());
        let binaries = dir.join("musl");
        std::fs::create_dir(&binaries).expect("binaries");
        std::fs::write(binaries.join("hostkeeper"), hostkeeper).expect("hostkeeper");
        std::fs::write(binaries.join("hostkeeper-pxe"), pxe).expect("pxe");
        let args = [
            "release",
            "nfpm-stage",
            "--arch",
            "amd64",
            "--gh-version",
            "2.0.0",
            "--gh-archive",
            path_str(&archive),
            "--gh-sha256",
            &sha256(&archive_bytes),
            "--trusted-root",
            path_str(&root),
            "--binaries",
            path_str(&binaries),
            "--out",
            path_str(&dir.join("target/nfpm")),
        ]
        .map(str::to_owned)
        .to_vec();
        Stage {
            dir,
            binaries,
            args,
        }
    }

    fn run(&self) -> Output {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        run(&self.dir, &args)
    }

    fn out(&self) -> PathBuf {
        self.dir.join("target/nfpm")
    }
}

#[test]
fn nfpm_stage_takes_the_static_programs_of_binaries_and_a_checked_package_carries_them() {
    let stage = Stage::new("stage", &static_pie(), &static_pie());
    let o = stage.run();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stage.out();
    let mut names: Vec<String> = std::fs::read_dir(&out)
        .expect("out")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["gh", "hostkeeper", "hostkeeper-pxe", "trusted_root.json"]
    );
    // The package of exactly the staged files passes the check against them.
    let staged = |name: &str| std::fs::read(out.join(name)).expect("staged file");
    let entries = vec![
        entry("usr/lib/hostkeeper/gh", 0o755, staged("gh")),
        entry(
            "usr/lib/hostkeeper/trusted_root.json",
            0o644,
            staged("trusted_root.json"),
        ),
        entry("usr/bin/hostkeeper", 0o755, staged("hostkeeper")),
        entry("usr/bin/hostkeeper-pxe", 0o755, staged("hostkeeper-pxe")),
    ];
    let packages = [
        (
            "hostkeeper.deb",
            deb(&entries, "Package: hostkeeper\n", true),
        ),
        ("hostkeeper.rpm", rpm(&entries, &[], true)),
    ];
    for (name, bytes) in packages {
        let package = write(&stage.dir, name, &bytes);
        let o = run(
            &stage.dir,
            &[
                "release",
                "check-linux-pkg",
                path_str(&package),
                "--staged",
                path_str(&out),
            ],
        );
        assert_eq!(o.status.code(), Some(0), "{name}: {}", stderr(&o));
        assert!(
            stdout(&o).contains("/usr/bin/hostkeeper is 755 root:root and the staged bytes"),
            "{name}: {}",
            stdout(&o)
        );
    }
}

#[test]
fn nfpm_stage_refuses_a_program_that_is_not_static_and_writes_nothing() {
    let glibc = elf(Some(GLIBC_INTERPRETER), &["libc.so.6"]);
    let stage = Stage::new("stage-glibc", &static_pie(), &glibc);
    let o = stage.run();
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!(
            "{} is not a static linux amd64 program: it asks for the interpreter {GLIBC_INTERPRETER}",
            stage.binaries.join("hostkeeper-pxe").display()
        )),
        "{}",
        stderr(&o)
    );
    assert!(!stage.out().exists());
}

#[test]
fn nfpm_stage_without_binaries_is_a_usage_error() {
    let stage = Stage::new("stage-usage", &static_pie(), &static_pie());
    let position = stage
        .args
        .iter()
        .position(|a| a == "--binaries")
        .expect("flag");
    let mut args: Vec<&str> = stage.args.iter().map(String::as_str).collect();
    args.drain(position..position + 2);
    let o = run(&stage.dir, &args);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(!stage.out().exists());
}

const COORDINATOR_UI: &str = "usr/share/hostkeeper-coordinator/ui";

/// The files the coordinator's nfpm.yaml installs, with a two-file web app, the program being
/// `program`.
fn coordinator_entries(program: Vec<u8>) -> Vec<Entry> {
    vec![
        entry("usr/bin/hostkeeper-coordinator", 0o755, program),
        entry(
            "usr/share/hostkeeper-coordinator/ui/index.html",
            0o644,
            b"<html>".to_vec(),
        ),
        entry(
            "usr/share/hostkeeper-coordinator/ui/pages/host.js",
            0o644,
            b"host page".to_vec(),
        ),
        entry(
            "usr/lib/systemd/system/hostkeeper-coordinator.service",
            0o644,
            b"[Unit]".to_vec(),
        ),
        entry(
            "usr/lib/sysusers.d/hostkeeper-coordinator.conf",
            0o644,
            b"u".to_vec(),
        ),
        entry(
            "usr/lib/tmpfiles.d/hostkeeper-coordinator.conf",
            0o644,
            b"d".to_vec(),
        ),
        entry(
            "usr/share/hostkeeper-coordinator/serve.toml.template",
            0o644,
            b"#".to_vec(),
        ),
    ]
}

/// The coordinator's deb and rpm of `entries`, each needing systemd alone.
fn coordinator_packages(entries: &[Entry]) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hostkeeper-coordinator.deb",
            deb(
                entries,
                "Package: hostkeeper-coordinator\nDepends: systemd\n",
                false,
            ),
        ),
        (
            "hostkeeper-coordinator.rpm",
            rpm(entries, &["systemd"], false),
        ),
    ]
}

/// A directory `d/staged` holding the coordinator's program and web app as the build job
/// stages them.
fn stage_coordinator(d: &TestDir) -> PathBuf {
    let staged = d.join("staged");
    std::fs::create_dir_all(staged.join("ui/pages")).expect("staged");
    std::fs::write(staged.join("hostkeeper-coordinator"), static_pie()).expect("program");
    std::fs::write(staged.join("ui/index.html"), b"<html>").expect("index");
    std::fs::write(staged.join("ui/pages/host.js"), b"host page").expect("page");
    staged
}

fn check_coordinator(d: &TestDir, package: &Path, staged: &Path) -> Output {
    run(
        d,
        &[
            "release",
            "check-linux-pkg",
            path_str(package),
            "--coordinator-staged",
            path_str(staged),
        ],
    )
}

#[test]
fn a_coordinator_package_that_installs_the_staged_files_is_accepted_in_a_deb_and_an_rpm() {
    let d = dir("coordinator-accept");
    let staged = stage_coordinator(&d);
    for (name, bytes) in coordinator_packages(&coordinator_entries(static_pie())) {
        let package = write(&d, name, &bytes);
        let o = check_coordinator(&d, &package, &staged);
        assert_eq!(o.status.code(), Some(0), "{name}: {}", stderr(&o));
        let out = stdout(&o);
        let at = package.display();
        for line in [
            "/usr/bin/hostkeeper-coordinator is 755 root:root and the staged bytes".to_owned(),
            format!("/{COORDINATOR_UI} holds the 2 staged files, 644 root:root"),
            "/usr/lib/systemd/system/hostkeeper-coordinator.service is 644 root:root".to_owned(),
            "/usr/bin/hostkeeper-coordinator is a static ELF program".to_owned(),
            "declares only systemd".to_owned(),
        ] {
            assert!(out.contains(&format!("{at}: {line}\n")), "{name}: {out}");
        }
        assert_eq!(stderr(&o), "", "{name}");
    }
}

#[test]
fn a_coordinator_package_with_a_changed_binary_is_refused_with_its_line() {
    let d = dir("coordinator-binary");
    let staged = stage_coordinator(&d);
    let changed = elf(None, &["libssl.so.3"]);
    for (name, bytes) in coordinator_packages(&coordinator_entries(changed)) {
        let package = write(&d, name, &bytes);
        let o = check_coordinator(&d, &package, &staged);
        assert_eq!(o.status.code(), Some(1), "{name}: {}", stdout(&o));
        let err = stderr(&o);
        let at = package.display();
        assert!(
            err.contains(&format!(
                "{at}: /usr/bin/hostkeeper-coordinator: its bytes are not those of {}\n",
                staged.join("hostkeeper-coordinator").display()
            )),
            "{name}: {err}"
        );
        assert!(
            err.contains(&format!(
                "{at}: /usr/bin/hostkeeper-coordinator: ELF program linked to libssl.so.3\n"
            )),
            "{name}: {err}"
        );
        assert_eq!(stdout(&o), "", "{name}");
    }
}

#[test]
fn a_coordinator_package_missing_a_ui_file_is_refused_with_its_line() {
    let d = dir("coordinator-ui");
    let staged = stage_coordinator(&d);
    let entries: Vec<Entry> = coordinator_entries(static_pie())
        .into_iter()
        .filter(|e| !e.path.ends_with("ui/pages/host.js"))
        .collect();
    for (name, bytes) in coordinator_packages(&entries) {
        let package = write(&d, name, &bytes);
        let o = check_coordinator(&d, &package, &staged);
        assert_eq!(o.status.code(), Some(1), "{name}: {}", stdout(&o));
        assert_eq!(
            stderr(&o),
            format!(
                "xtask: {}: /{COORDINATOR_UI}/pages/host.js: not in the package\n",
                package.display()
            ),
            "{name}"
        );
    }
}

#[test]
fn coordinator_staged_with_staged_is_a_usage_error() {
    let d = dir("coordinator-usage");
    let staged = stage_coordinator(&d);
    let (name, bytes) = coordinator_packages(&coordinator_entries(static_pie())).remove(0);
    let package = write(&d, name, &bytes);
    let o = run(
        &d,
        &[
            "release",
            "check-linux-pkg",
            path_str(&package),
            "--staged",
            path_str(&staged),
            "--coordinator-staged",
            path_str(&staged),
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stdout(&o));
    assert!(stderr(&o).contains("cannot be used with"), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
}

#[test]
fn coordinator_staged_with_service_is_a_usage_error() {
    let d = dir("coordinator-service-usage");
    let staged = stage_coordinator(&d);
    let (name, bytes) = coordinator_packages(&coordinator_entries(static_pie())).remove(0);
    let package = write(&d, name, &bytes);
    let o = run(
        &d,
        &[
            "release",
            "check-linux-pkg",
            path_str(&package),
            "--service",
            "hello",
            "--coordinator-staged",
            path_str(&staged),
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stdout(&o));
    assert!(stderr(&o).contains("cannot be used with"), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
}
