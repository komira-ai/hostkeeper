//! The two files the daemon's attestation check runs on, in the deb and the rpm: `nfpm.yaml`
//! installs them at the daemon's own paths (`hostkeeper_daemon::cli::GH_BINARY` and
//! `SIGSTORE_TRUSTED_ROOT`, imported, not repeated), with their modes, from the directory
//! `release nfpm-stage` writes, and that step refuses a `gh` archive that is not the pinned one
//! and a missing trusted root. Fixture files only: no network, no nfpm, no `gh`.

use std::io::Write;
use std::path::{Path, PathBuf};

use hostkeeper_daemon::cli::{GH_BINARY, SIGSTORE_TRUSTED_ROOT};
use sha2::Digest;
use xtask::test_support::{NFPM_STAGED, gh_member, nfpm_stage};

use crate::nfpm::File;
use crate::testdir::TestDir;
use crate::{NFPM, config, read};

/// The directory, from the repository root, that nfpm.yaml takes the staged files from and the
/// workflow passes as `--out`.
const STAGE_DIR: &str = "target/nfpm";

/// The pinned version the fixtures are named for.
const GH_VERSION: &str = "2.102.1";

/// One line of a trusted root as `gh attestation trusted-root` prints it, cut down.
const ROOT: &str =
    "{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json;version=0.1\",\"tlogs\":[]}\n";

/// The `src` nfpm.yaml gives the staged file `name`.
pub fn staged(name: &str) -> String {
    format!("{STAGE_DIR}/{name}")
}

fn scratch(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("attestation-files-{name}"),
    )
}

/// An ELF header of a 64-bit little-endian program for `arch` and a body: what the staging
/// step looks at in a `gh`.
fn program(arch: &str) -> Vec<u8> {
    let machine: u16 = if arch == "amd64" { 0x3e } else { 0xb7 };
    let mut bytes = b"\x7fELF\x02\x01\x01".to_vec();
    bytes.resize(16, 0);
    bytes.extend([2, 0]);
    bytes.extend(machine.to_le_bytes());
    bytes.extend(format!(" a fixture gh for {arch}").as_bytes());
    bytes
}

/// gh's release archive for `arch`, holding `program(arch)` at its member.
fn archive(arch: &str) -> Vec<u8> {
    let member = gh_member(GH_VERSION, arch).expect("member");
    let gh = program(arch);
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o755);
    header.set_size(gh.len() as u64);
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_data(&mut header, member, gh.as_slice())
        .expect("tar");
    let tar = builder.into_inner().expect("tar");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).expect("gzip");
    gz.finish().expect("gzip")
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A repository root of a test: its `gh` archive for `arch` and trusted root, outside the stage
/// directory.
struct Root {
    dir: TestDir,
    archive: PathBuf,
    trusted_root: PathBuf,
    pin: String,
}

impl Root {
    fn new(name: &str, arch: &str) -> Root {
        let dir = scratch(name);
        let archive = dir.join(format!("gh_{GH_VERSION}_linux_{arch}.tar.gz"));
        let bytes = self::archive(arch);
        std::fs::write(&archive, &bytes).expect("archive");
        let trusted_root = dir.join("trusted_root.jsonl");
        std::fs::write(&trusted_root, ROOT).expect("root");
        Root {
            dir,
            archive,
            trusted_root,
            pin: sha256(&bytes),
        }
    }

    /// `release nfpm-stage --arch <arch> ... --out target/nfpm`, run at this root, pinned to
    /// `pin`.
    fn stage(&self, arch: &str, pin: &str) -> Result<Vec<String>, String> {
        nfpm_stage(
            arch,
            GH_VERSION,
            &self.archive,
            pin,
            &self.trusted_root,
            &self.dir.join(STAGE_DIR),
        )
    }

    /// The source of `file` as nfpm reads it from this root.
    fn source(&self, file: &File) -> PathBuf {
        self.dir.join(&file.src)
    }
}

/// The file of `files` installed at `dst`.
fn installed<'a>(files: &'a [File], dst: &str) -> &'a File {
    files
        .iter()
        .find(|f| f.dst == dst)
        .unwrap_or_else(|| panic!("no file is installed at {dst}: {files:?}"))
}

/// Catches a deb or an rpm that does not install `gh` or the trusted root where the daemon
/// starts and reads them, with the modes and owner it trusts, or that takes either from a place
/// the staging step does not write.
#[test]
fn deb_and_rpm_install_the_staged_gh_and_trusted_root_at_the_daemon_paths() {
    assert_eq!(GH_BINARY, "/usr/lib/hostkeeper/gh");
    assert_eq!(
        SIGSTORE_TRUSTED_ROOT,
        "/usr/lib/hostkeeper/trusted_root.json"
    );
    let config = config();
    for arch in ["amd64", "arm64"] {
        let root = Root::new("installed", arch);
        root.stage(arch, &root.pin.clone()).expect("staged");
        for packager in ["deb", "rpm"] {
            let files = config.files(packager).expect("files");
            let gh = installed(&files, GH_BINARY);
            let trusted = installed(&files, SIGSTORE_TRUSTED_ROOT);
            for (file, mode) in [(gh, "0755"), (trusted, "0644")] {
                assert_eq!(
                    (file.mode.as_str(), file.owner.as_str(), file.group.as_str()),
                    (mode, "root", "root"),
                    "{packager} {}",
                    file.dst
                );
            }
            assert_eq!(
                std::fs::read(root.source(gh)).expect("staged gh"),
                program(arch),
                "{packager} {arch}"
            );
            assert_eq!(
                std::fs::read_to_string(root.source(trusted)).expect("staged trusted root"),
                ROOT,
                "{packager} {arch}"
            );
        }
    }
}

/// Every file the step stages is a source of the packages, and nothing else under the stage
/// directory is: the two lists are the same names.
#[test]
fn the_staged_files_are_the_sources_nfpm_takes_from_the_stage_directory() {
    let root = Root::new("sources", "amd64");
    root.stage("amd64", &root.pin.clone()).expect("staged");
    let mut written: Vec<String> = std::fs::read_dir(root.dir.join(STAGE_DIR))
        .expect("stage directory")
        .map(|e| staged(&e.expect("entry").file_name().to_string_lossy()))
        .collect();
    written.sort();
    let mut wanted: Vec<String> = NFPM_STAGED.iter().map(|n| staged(n)).collect();
    wanted.sort();
    assert_eq!(written, wanted);
    for packager in ["deb", "rpm"] {
        let mut sources: Vec<String> = config()
            .files(packager)
            .expect("files")
            .into_iter()
            .map(|f| f.src)
            .filter(|src| src.starts_with(&format!("{STAGE_DIR}/")))
            .collect();
        sources.sort();
        assert_eq!(sources, wanted, "{packager}");
    }
}

/// Catches a package step that goes on with a `gh` archive other than the pinned one: nothing
/// is staged, so nfpm finds no source for `gh`.
#[test]
fn the_package_step_fails_when_the_gh_archive_is_not_the_pinned_one() {
    let root = Root::new("pin", "arm64");
    let pin = sha256(b"the archive of another gh");
    let actual = root.pin.clone();
    assert_eq!(
        root.stage("arm64", &pin),
        Err(format!(
            "{}: the download's SHA-256 is {actual}, not the pinned {pin}; nothing was installed",
            root.archive.display()
        ))
    );
    assert!(!root.dir.join(STAGE_DIR).exists());
    // The archive of the other architecture has another digest: it is refused under the pin of
    // this one.
    let other = Root::new("pin-other-arch", "amd64");
    let message = other.stage("amd64", &root.pin).expect_err("refused");
    assert!(message.contains("not the pinned"), "{message}");
}

/// Catches a package step that goes on without the trusted root.
#[test]
fn the_package_step_fails_when_the_trusted_root_is_absent() {
    let root = Root::new("absent", "amd64");
    std::fs::remove_file(&root.trusted_root).expect("remove");
    let message = root.stage("amd64", &root.pin.clone()).expect_err("refused");
    assert!(
        message.starts_with(&format!("read {}: ", root.trusted_root.display())),
        "{message}"
    );
    assert!(!root.dir.join(STAGE_DIR).exists());
}

/// The test helpers refuse what the verb's flags refuse.
#[test]
fn the_staging_flags_refuse_an_unknown_arch_a_bad_version_or_a_bad_pin() {
    let root = Root::new("flags", "amd64");
    let out = root.dir.join(STAGE_DIR);
    let pin = root.pin.clone();
    let cases = [
        (
            "x86_64",
            GH_VERSION,
            pin.as_str(),
            "invalid variant: x86_64",
        ),
        ("amd64", "2.102", pin.as_str(), "2.102"),
        ("amd64", GH_VERSION, "ABC", "not a SHA-256: \"ABC\""),
    ];
    for (arch, version, sha, words) in cases {
        let message = nfpm_stage(arch, version, &root.archive, sha, &root.trusted_root, &out)
            .expect_err("refused");
        assert!(message.contains(words), "{message:?} lacks {words:?}");
    }
    assert!(gh_member("2.102", "amd64").is_err());
    assert_eq!(
        gh_member(GH_VERSION, "x86_64"),
        Err("invalid variant: x86_64".to_owned())
    );
    assert!(!out.exists());
    // nfpm.yaml names the step that writes the stage directory.
    assert!(read(NFPM).contains("cargo xtask release nfpm-stage --arch"));
}
