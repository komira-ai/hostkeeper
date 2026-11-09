//! `xtask channels apt|rpm|pkg` end to end: each publisher verb is run as a process, with the
//! `aws` and `curl` flags pointing at a fake (tests/bin/aws_fixture.rs: a bucket and a public host
//! in directories beside a copy of it, and a record of its calls), and its exit status, words,
//! files and the calls it made are read. The logic of each verb is tested beside it, in the
//! library; these tests hold the dispatch of the flags, the exit statuses (0, 1 for a failure, 2
//! for a usage error) and the requests the verbs send: the keys, and the `If-Match` and
//! `If-None-Match` conditions.

#[path = "../src/testdir.rs"]
mod testdir;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rpm::{BuildConfig, CompressionWithLevel, FileMode, FileOptions, PackageBuilder};
use sha2::Digest;
use testdir::TestDir;

const FAKE: &str = env!("CARGO_BIN_EXE_xtask-aws-fixture");

/// 2026-09-01T00:00:00Z.
const DATE: &str = "1788220800";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const BUCKET: &str = "packages-bucket";
const HOST: &str = "packages.example.com";
const INSTALLER: &str = "hostkeeper-1.2.3-macos-arm64.pkg";
const PACKAGES: [&str; 2] = ["hostkeeper", "hostkeeper-coordinator"];
/// The Debian and the RPM name of each architecture.
const ARCHES: [(&str, &str); 2] = [("amd64", "x86_64"), ("arm64", "aarch64")];

fn dir(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("channels-publish-{name}"),
    )
}

fn text(path: &Path) -> &str {
    path.to_str().expect("utf8 path")
}

fn xtask(cwd: &Path, args: &[&str]) -> Output {
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

/// Runs `args` and returns what it printed; it must exit 0.
fn ok(cwd: &Path, args: &[&str]) -> String {
    let o = xtask(cwd, args);
    assert_eq!(o.status.code(), Some(0), "{args:?}: {}", stderr(&o));
    stdout(&o)
}

/// Runs `args`, which must exit with `code`, and returns what it printed on standard error.
fn exits(cwd: &Path, code: i32, args: &[&str]) -> String {
    let o = xtask(cwd, args);
    assert_eq!(o.status.code(), Some(code), "{args:?}: {}", stderr(&o));
    assert_eq!(stdout(&o), "", "{args:?}");
    stderr(&o)
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// The ETag the fake prints for an object.
fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", &sha256(bytes)[..12])
}

/// The fake `aws` and `curl`: a link to the program, with the bucket, the public host and the
/// record of the calls beside it.
struct Fake {
    bin: PathBuf,
}

impl Fake {
    fn new(root: &Path) -> Fake {
        let bin = root.join("fake");
        std::fs::create_dir_all(&bin).expect("create the fake's directory");
        // A link, not a copy: a copy being written when another test starts a process is a program
        // that cannot be run ("text file busy").
        std::fs::hard_link(FAKE, bin.join("aws")).expect("link the fake");
        Fake { bin }
    }

    fn program(&self) -> PathBuf {
        self.bin.join("aws")
    }

    fn object(&self, key: &str) -> Vec<u8> {
        std::fs::read(self.bin.join("store").join(key)).unwrap_or_else(|e| panic!("{key}: {e}"))
    }

    fn has(&self, key: &str) -> bool {
        self.bin.join("store").join(key).is_file()
    }

    fn set(&self, area: &str, key: &str, bytes: &[u8]) {
        let path = self.bin.join(area).join(key);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create");
        std::fs::write(path, bytes).expect("write");
    }

    /// Puts an object in the bucket, as another writer would.
    fn put(&self, key: &str, bytes: &[u8]) {
        self.set("store", key, bytes);
    }

    /// Serves `bytes` at `path` of the public host.
    fn serve(&self, path: &str, bytes: &[u8]) {
        self.set("origin", path, bytes);
    }

    /// Every call so far, the arguments of each.
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.bin.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split('\t').map(str::to_owned).collect())
            .collect()
    }

    /// The `put-object` calls: the key, and the condition it carries.
    fn puts(&self) -> Vec<(String, Vec<String>)> {
        self.calls()
            .into_iter()
            .filter(|c| c[..2] == ["s3api", "put-object"])
            .map(|c| {
                let at = c.iter().position(|a| a == "--key").expect("a key") + 1;
                let cond = c
                    .iter()
                    .position(|a| a.starts_with("--if-"))
                    .map(|i| c[i..i + 2].to_vec())
                    .expect("a condition");
                (c[at].clone(), cond)
            })
            .collect()
    }

    fn clear_calls(&self) {
        let _ = std::fs::remove_file(self.bin.join("calls"));
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(bytes).expect("gzip");
    gz.finish().expect("finish")
}

/// An `ar` archive of the members `(name, content)`.
fn ar(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, content) in members {
        let header = format!(
            "{name:<16}{DATE:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0,
            0,
            "100644",
            content.len()
        );
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(content);
        if content.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// A Debian package whose control file says `package`, `version` and `arch`.
fn deb(package: &str, version: &str, arch: &str) -> Vec<u8> {
    let control = format!(
        "Package: {package}\nVersion: {version}\nSection: admin\nPriority: optional\nArchitecture: {arch}\n\
         Maintainer: The tests <tests@example.com>\nInstalled-Size: 1\nDescription: a fixture\n"
    );
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_ustar();
    header.set_path("./control").expect("a path");
    header.set_size(control.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, control.as_bytes()).expect("append");
    let control_tar = gzip(&tar.into_inner().expect("tar"));
    ar(&[
        ("debian-binary", b"2.0\n"),
        ("control.tar.gz", &control_tar),
    ])
}

/// An unsigned RPM named `package`, at `version` for `arch`.
fn rpm(package: &str, version: &str, arch: &str) -> Vec<u8> {
    let config = BuildConfig::v4()
        .compression(CompressionWithLevel::Gzip(9))
        .source_date(1_788_220_800_u32);
    let mut builder = PackageBuilder::new(package, version, "Apache-2.0", arch, "a fixture");
    builder
        .using_config(config)
        .release("1")
        .description("A fixture.")
        .url("https://example.com/fixture")
        .packager("The tests <tests@example.com>")
        .vendor("example")
        .build_host("localhost")
        .with_file_contents(
            package.as_bytes().to_vec(),
            FileOptions::new(format!("/usr/bin/{package}"))
                .mode(FileMode::regular(0o755))
                .user("root")
                .group("root"),
        )
        .expect("add a file");
    let mut bytes = Vec::new();
    builder
        .build()
        .expect("build")
        .write(&mut bytes)
        .expect("write");
    bytes
}

/// A release directory of every package of `version`, as `kind` files them (`deb` or `rpm`),
/// with the `SHA256SUMS` of them. The names of the files, sorted.
fn release(root: &Path, name: &str, version: &str, kind: &str) -> Vec<String> {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create");
    let mut names = Vec::new();
    let mut sums = String::new();
    for package in PACKAGES {
        for (deb_arch, rpm_arch) in ARCHES {
            let (file, bytes) = if kind == "deb" {
                (
                    format!("{package}_{version}_{deb_arch}.deb"),
                    deb(package, version, deb_arch),
                )
            } else {
                (
                    format!("{package}-{version}-1.{rpm_arch}.rpm"),
                    rpm(package, version, rpm_arch),
                )
            };
            std::fs::write(dir.join(&file), &bytes).expect("write");
            sums.push_str(&format!("{}  {file}\n", sha256(&bytes)));
            names.push(file);
        }
    }
    std::fs::write(dir.join("SHA256SUMS"), sums).expect("write sums");
    names.sort();
    names
}

/// A `current` directory as `read` writes it for a repository with nothing in it.
fn empty_current(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create");
    std::fs::write(dir.join("etags.txt"), "").expect("write");
    dir
}

fn pool_keys(prefix: &str, files: &[String]) -> Vec<String> {
    files
        .iter()
        .map(|f| format!("{prefix}/pool/hostkeeper/{f}"))
        .collect()
}

/// The files `rpm repo sign` adds to a new `repodata` (which this test does not run).
fn sign_rpm(next: &Path, suite: &str) {
    for (_, arch) in ARCHES {
        std::fs::write(
            next.join(suite).join(arch).join("repodata/repomd.xml.asc"),
            b"signed",
        )
        .expect("write");
    }
}

/// A release directory with the installer of `version`, its bundle and a `SHA256SUMS` that lists
/// the installer.
fn pkg_release(root: &Path, name: &str, version: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create");
    let installer = format!("hostkeeper-{version}-macos-arm64.pkg");
    let bytes = format!("installer {version}").into_bytes();
    std::fs::write(dir.join(&installer), &bytes).expect("write");
    std::fs::write(
        dir.join(format!("{installer}.sigstore.json")),
        format!("bundle {version}"),
    )
    .expect("write");
    std::fs::write(
        dir.join("SHA256SUMS"),
        format!("{}  {installer}\n", sha256(&bytes)),
    )
    .expect("write");
    dir
}

#[test]
fn the_fake_programs_exit_2_for_a_call_they_do_not_know() {
    let d = dir("fake-unknown");
    let fake = Fake::new(&d);
    for args in [
        &["sts", "get-caller-identity"][..],
        &["s3api", "get-object"],
        &["s3api", "put-object"],
        &["--output"],
    ] {
        let o = Command::new(fake.program())
            .args(args)
            .output()
            .expect("run");
        assert_eq!(o.status.code(), Some(2), "{args:?}");
    }
}

#[path = "channels_publish_cli/apt.rs"]
mod apt_verbs;
#[path = "channels_publish_cli/pkg.rs"]
mod pkg_verbs;
#[path = "channels_publish_cli/programs.rs"]
mod programs;
#[path = "channels_publish_cli/rpm.rs"]
mod rpm_verbs;
