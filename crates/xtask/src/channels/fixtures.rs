//! Test stand-ins shared by the tests of the channel verbs: an [`Exec`] that starts no process,
//! and the packages of a release as `nfpm` writes them for the daemon's and the coordinator's
//! packages and as `release deb` writes them for hello's and mdm's (no maintainer script).

use std::cell::RefCell;

use flate2::Compression;
use rpm::Scriptlet;
use rpm::{BuildConfig, CompressionWithLevel, Dependency, FileMode, FileOptions, PackageBuilder};

use crate::channels::spec::{Arch, HOSTKEEPER, Package, Product, RPM_RELEASE};
use crate::release::ports::{Exec, Ran};

/// The time of every member and entry of a fixture package: 2026-09-01T00:00:00Z.
pub(crate) const MTIME: u32 = 1_788_220_800;

/// An [`Exec`] that records the calls and answers one way: no process is started.
pub(crate) struct FakeExec {
    pub(crate) calls: RefCell<Vec<(String, Vec<String>)>>,
    pub(crate) answer: std::io::Result<Ran>,
}

impl FakeExec {
    /// A program that exits 0 and prints `stdout`.
    pub(crate) fn says(stdout: &str) -> FakeExec {
        FakeExec {
            calls: RefCell::default(),
            answer: Ok(Ran {
                success: true,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            }),
        }
    }

    /// A program that exits with a failure and prints `stderr`.
    pub(crate) fn fails(stderr: &str) -> FakeExec {
        FakeExec {
            calls: RefCell::default(),
            answer: Ok(Ran {
                success: false,
                stdout: Vec::new(),
                stderr: stderr.as_bytes().to_vec(),
            }),
        }
    }

    /// A program that cannot be started.
    pub(crate) fn missing() -> FakeExec {
        FakeExec {
            calls: RefCell::default(),
            answer: Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No such file or directory",
            )),
        }
    }
}

impl Exec for FakeExec {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        self.calls
            .borrow_mut()
            .push((program.to_owned(), args.to_vec()));
        match &self.answer {
            Ok(ran) => Ok(ran.clone()),
            Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
        }
    }
}

/// The files `nfpm` puts in the package, with the bytes of the fixture: the paths from the
/// package's `nfpm.yaml`; for a service's native package, the files its spec lists.
fn files(package: &Package, arch: &str) -> Vec<(&'static str, u32, Vec<u8>)> {
    let binary = format!("{} {arch}", package.name).into_bytes();
    if !package.rpm {
        return package
            .files
            .iter()
            .map(|&path| {
                if path == package.binary {
                    (path, 0o755, binary.clone())
                } else if path.ends_with(".service") || path.ends_with(".timer") {
                    (path, 0o644, b"[Service]\n".to_vec())
                } else {
                    (path, 0o755, format!("{path} {arch}").into_bytes())
                }
            })
            .collect();
    }
    if package.name == HOSTKEEPER.packages[0].name {
        vec![
            ("/usr/bin/hostkeeper", 0o755, binary),
            ("/usr/bin/hostkeeper-pxe", 0o755, b"pxe".to_vec()),
            (
                "/usr/lib/systemd/system/hostkeeper.service",
                0o644,
                b"[Service]\n".to_vec(),
            ),
            (
                "/usr/lib/tmpfiles.d/hostkeeper.conf",
                0o644,
                b"d /run/hostkeeper\n".to_vec(),
            ),
            (
                "/usr/share/hostkeeper/hostkeeper.toml.template",
                0o644,
                b"# template\n".to_vec(),
            ),
            ("/usr/lib/hostkeeper/gh", 0o755, b"gh".to_vec()),
            (
                "/usr/lib/hostkeeper/trusted_root.json",
                0o644,
                b"{}".to_vec(),
            ),
        ]
    } else {
        vec![
            ("/usr/bin/hostkeeper-coordinator", 0o755, binary),
            (
                "/usr/lib/systemd/system/hostkeeper-coordinator.service",
                0o644,
                b"[Service]\n".to_vec(),
            ),
            (
                "/usr/lib/sysusers.d/hostkeeper-coordinator.conf",
                0o644,
                b"u hostkeeper-coordinator -\n".to_vec(),
            ),
            (
                "/usr/lib/tmpfiles.d/hostkeeper-coordinator.conf",
                0o644,
                b"d /var/lib/hostkeeper-coordinator\n".to_vec(),
            ),
            (
                "/usr/share/hostkeeper-coordinator/serve.toml.template",
                0o644,
                b"# template\n".to_vec(),
            ),
            (
                "/usr/share/hostkeeper-coordinator/ui/index.html",
                0o644,
                b"<html></html>\n".to_vec(),
            ),
        ]
    }
}

/// The control file of the deb of `package`: the fields `nfpm` writes.
pub(crate) fn control(package: &Package, version: &str, arch: &str) -> String {
    let depends = if package.name == HOSTKEEPER.packages[1].name {
        "Depends: systemd\n"
    } else {
        ""
    };
    format!(
        "Package: {}\nVersion: {version}\nSection: admin\nPriority: optional\nArchitecture: {arch}\n\
         Maintainer: The hostkeeper project <https://github.com/komira-ai/hostkeeper>\n\
         Installed-Size: 20175\n{depends}Homepage: https://github.com/komira-ai/hostkeeper\n\
         Description: the fixture of {}\n a second line of the description.\n",
        package.name, package.name
    )
}

/// A gzip-compressed tar of `entries` (path, mode, content), each owned by root.
pub(crate) fn tar_gz(entries: &[(String, u32, Vec<u8>)]) -> Vec<u8> {
    let gzip = flate2::GzBuilder::new()
        .mtime(MTIME)
        .write(Vec::new(), Compression::best());
    let mut tar = tar::Builder::new(gzip);
    for (path, mode, content) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_path(path).expect("a short path");
        header.set_size(content.len() as u64);
        header.set_mode(*mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(u64::from(MTIME));
        header.set_cksum();
        tar.append(&header, content.as_slice()).expect("append");
    }
    tar.into_inner()
        .and_then(flate2::write::GzEncoder::finish)
        .expect("finish")
}

/// An `ar` archive of the members `(name, content)`.
pub(crate) fn ar(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, content) in members {
        out.extend_from_slice(
            format!(
                "{name:<16}{MTIME:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                "100644",
                content.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(content);
        if content.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// A deb with this `control` file and the files of `package`: with the `postinst` nfpm writes
/// for the daemon's and the coordinator's packages, and no script for a service's.
pub(crate) fn deb_with_control(package: &Package, arch: &str, control: &str) -> Vec<u8> {
    let mut control_entries = vec![("./control".to_owned(), 0o644, control.as_bytes().to_vec())];
    if package.rpm {
        control_entries.push((
            "./postinst".to_owned(),
            0o755,
            b"#!/bin/sh\nexit 0\n".to_vec(),
        ));
    }
    let control_tar = tar_gz(&control_entries);
    let data: Vec<(String, u32, Vec<u8>)> = files(package, arch)
        .into_iter()
        .map(|(path, mode, content)| (format!(".{path}"), mode, content))
        .collect();
    ar(&[
        ("debian-binary", b"2.0\n"),
        ("control.tar.gz", &control_tar),
        ("data.tar.gz", &tar_gz(&data)),
    ])
}

/// The deb of `package` at `version` for the Debian architecture `arch`.
pub(crate) fn deb(package: &Package, version: &str, arch: &Arch) -> Vec<u8> {
    deb_with_control(package, arch.deb, &control(package, version, arch.deb))
}

/// The deb of `package` at `version` for `arch` with other bytes: the same package, another
/// description.
pub(crate) fn deb_other(package: &Package, version: &str, arch: &Arch) -> Vec<u8> {
    let control = control(package, version, arch.deb).replace("the fixture", "another fixture");
    deb_with_control(package, arch.deb, &control)
}

/// The rpm of `package` at `version` for `arch`, as `nfpm` writes it: `%post` and `%preun`, and
/// `%postun` only when `postun` says so.
pub(crate) fn rpm(package: &Package, version: &str, arch: &Arch, postun: bool) -> Vec<u8> {
    let config = BuildConfig::v4()
        .compression(CompressionWithLevel::Gzip(9))
        .source_date(MTIME);
    let mut builder = PackageBuilder::new(
        package.name,
        version,
        "Apache-2.0",
        arch.rpm,
        &format!("the fixture of {}", package.name),
    );
    builder
        .using_config(config)
        .release(RPM_RELEASE)
        .description("The fixture package.\nA second line.")
        .url("https://github.com/komira-ai/hostkeeper")
        .packager("The hostkeeper project <https://github.com/komira-ai/hostkeeper>")
        .vendor("komira-ai")
        .build_host("localhost")
        .post_install_script(Scriptlet::new("exit 0\n"))
        .pre_uninstall_script(Scriptlet::new("exit 0\n"));
    if package.name == HOSTKEEPER.packages[1].name {
        builder.requires(Dependency::any("systemd"));
    }
    if postun {
        builder.post_uninstall_script(Scriptlet::new("exit 0\n"));
    }
    // `nfpm` lists the directories of a `tree` entry as well as its files.
    if package.name == HOSTKEEPER.packages[1].name {
        builder
            .with_dir_entry(
                FileOptions::dir("/usr/share/hostkeeper-coordinator/ui").permissions(0o755),
            )
            .expect("add a directory");
    }
    for (path, mode, content) in files(package, arch.rpm) {
        builder
            .with_file_contents(
                content,
                FileOptions::new(path)
                    .mode(FileMode::regular(mode as u16))
                    .user("root")
                    .group("root"),
            )
            .expect("add a file");
    }
    let mut bytes = Vec::new();
    builder
        .build()
        .expect("build")
        .write(&mut bytes)
        .expect("write");
    bytes
}

/// Every arch of each of the product's packages, for a test that walks them.
pub(crate) fn every() -> impl Iterator<Item = (&'static Package, &'static Arch)> {
    let product: &'static Product = &HOSTKEEPER;
    product
        .packages
        .iter()
        .flat_map(|p| p.arches.iter().map(move |a| (p, a)))
}

/// Every arch of each of the product's packages that is an rpm.
pub(crate) fn every_rpm() -> impl Iterator<Item = (&'static Package, &'static Arch)> {
    every().filter(|(p, _)| p.rpm)
}
