use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::*;
use crate::release::package::elf::fixture::Spec;
use crate::testdir::TestDir;

/// The trusted root `gh attestation trusted-root` prints, cut down: one line per instance.
const ROOT: &str = concat!(
    r#"{"mediaType":"application/vnd.dev.sigstore.trustedroot+json;version=0.1","tlogs":[]}"#,
    "\n",
    r#"{"mediaType":"application/vnd.dev.sigstore.trustedroot+json;version=0.1","certificateAuthorities":[]}"#,
    "\n",
);

fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-nfpm-stage-unit"),
        name,
    )
}

/// The first 20 bytes of an ELF header of `class`, `data` and `machine`, then a body.
fn elf_with(class: u8, data: u8, machine: u16) -> Vec<u8> {
    let mut bytes = b"\x7fELF".to_vec();
    bytes.extend([class, data, 1]);
    bytes.resize(16, 0);
    bytes.extend([2, 0]);
    bytes.extend(machine.to_le_bytes());
    bytes.extend(b" the rest of a program");
    bytes
}

/// A 64-bit little-endian ELF program for `arch`.
fn elf(arch: Arch) -> Vec<u8> {
    elf_with(2, 1, arch.elf_machine())
}

/// A static-pie program for `arch`, as the musl build makes one.
fn static_program(arch: Arch) -> Vec<u8> {
    Spec::static_pie(arch.elf_machine()).build()
}

/// A gzip tar like gh's release archive: a decoy, then `member` holding `program`.
fn archive(member: &str, program: &[u8]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, data) in [
        ("gh_2.0.0_linux_amd64/LICENSE", &b"decoy"[..]),
        (member, program),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_mode(0o755);
        h.set_size(data.len() as u64);
        b.append_data(&mut h, path, data).expect("file");
    }
    let tar = b.into_inner().expect("tar");
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(&tar).expect("gzip");
    e.finish().expect("gzip")
}

/// A scratch directory holding `gh.tar.gz` and `trusted_root.jsonl`, and a request to stage them
/// for `arch` into `out` under it, pinned to the archive's own SHA-256.
struct Case {
    dir: TestDir,
    request: Request,
}

impl Case {
    fn new(name: &str, arch: Arch, archive_bytes: &[u8], root: &[u8]) -> Case {
        let dir = scratch(name);
        let gh_archive = dir.join("gh.tar.gz");
        let trusted_root = dir.join("trusted_root.jsonl");
        std::fs::write(&gh_archive, archive_bytes).expect("archive");
        std::fs::write(&trusted_root, root).expect("root");
        let binaries = dir.join("target/release");
        std::fs::create_dir_all(&binaries).expect("binaries");
        for file in [HOSTKEEPER_FILE, PXE_FILE] {
            std::fs::write(binaries.join(file), static_program(arch)).expect("program");
        }
        let request = Request {
            arch,
            gh_version: "2.0.0".parse().expect("version"),
            gh_archive,
            gh_sha256: install::Sha256::of(archive_bytes),
            trusted_root,
            binaries,
            out: dir.join("target/nfpm"),
        };
        Case { dir, request }
    }

    /// A case whose archive holds `program` at the member of `arch`.
    fn of(name: &str, arch: Arch, program: &[u8], root: &str) -> Case {
        let member = format!("gh_2.0.0_linux_{arch}/bin/gh");
        Case::new(name, arch, &archive(&member, program), root.as_bytes())
    }

    fn refused(&self) -> String {
        match stage(&self.request) {
            Ok(lines) => panic!("staged {lines:?}"),
            Err(e) => e.to_string(),
        }
    }

    /// The message of a refusal, and that nothing was written.
    fn refused_writing_nothing(&self) -> String {
        let message = self.refused();
        assert!(
            !self.request.out.exists(),
            "{message}: wrote {:?}",
            self.request.out
        );
        message
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777
}

#[test]
fn stages_the_gh_of_the_package_arch_and_the_trusted_root_with_their_modes() {
    for arch in [Arch::Amd64, Arch::Arm64] {
        let case = Case::of("stages", arch, &elf(arch), ROOT);
        let lines = stage(&case.request).expect("staged");
        let out = &case.request.out;
        assert_eq!(std::fs::read(out.join("gh")).expect("gh"), elf(arch));
        assert_eq!(mode(&out.join("gh")), 0o755);
        assert_eq!(
            std::fs::read_to_string(out.join("trusted_root.json")).expect("root"),
            ROOT
        );
        assert_eq!(mode(&out.join("trusted_root.json")), 0o644);
        let mut names: Vec<_> = std::fs::read_dir(out)
            .expect("out")
            .map(|e| e.expect("entry").file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["gh", "hostkeeper", "hostkeeper-pxe", "trusted_root.json"]
        );
        for file in [HOSTKEEPER_FILE, PXE_FILE] {
            assert_eq!(
                std::fs::read(out.join(file)).expect("program"),
                static_program(arch)
            );
            assert_eq!(mode(&out.join(file)), 0o755);
        }
        assert_eq!(
            lines,
            [
                format!(
                    "{}: gh 2.0.0 for {arch} from gh_2.0.0_linux_{arch}/bin/gh, sha256 {}",
                    out.join("gh").display(),
                    install::Sha256::of(&elf(arch))
                ),
                format!(
                    "{}: from {}, sha256 {}",
                    out.join("trusted_root.json").display(),
                    case.request.trusted_root.display(),
                    install::Sha256::of(ROOT.as_bytes())
                ),
                format!(
                    "{}: static {arch} program from {}, sha256 {}",
                    out.join("hostkeeper").display(),
                    case.request.binaries.join("hostkeeper").display(),
                    install::Sha256::of(&static_program(arch))
                ),
                format!(
                    "{}: static {arch} program from {}, sha256 {}",
                    out.join("hostkeeper-pxe").display(),
                    case.request.binaries.join("hostkeeper-pxe").display(),
                    install::Sha256::of(&static_program(arch))
                ),
            ]
        );
    }
}

/// Catches a glibc build, a program of the other architecture, a 32-bit or big-endian build, or
/// a file that is no program, staged in a package: each is refused naming the file, and nothing
/// is written.
#[test]
fn a_program_that_is_not_a_static_64_bit_little_endian_one_of_the_arch_is_refused() {
    let glibc = Spec::glibc();
    let cases: Vec<(&str, Vec<u8>, String)> = vec![
        (
            HOSTKEEPER_FILE,
            glibc.build(),
            "it asks for the interpreter /lib64/ld-linux-x86-64.so.2".into(),
        ),
        (
            PXE_FILE,
            Spec {
                interpreter: None,
                ..glibc.clone()
            }
            .build(),
            "it is linked to libc.so.6".into(),
        ),
        (
            HOSTKEEPER_FILE,
            static_program(Arch::Arm64),
            "ELF machine 0xb7, not 0x3e".into(),
        ),
        (
            PXE_FILE,
            Spec {
                class: 1,
                ..Spec::static_pie(0x3e)
            }
            .build(),
            "ELF class 1 and data 1, not 2 (64-bit) and 1 (little-endian)".into(),
        ),
        (
            PXE_FILE,
            Spec {
                big_endian: true,
                ..Spec::static_pie(0x3e)
            }
            .build(),
            "ELF class 2 and data 2, not 2 (64-bit) and 1 (little-endian)".into(),
        ),
        (
            HOSTKEEPER_FILE,
            b"#!/bin/sh\n".to_vec(),
            "no ELF magic".into(),
        ),
        (
            HOSTKEEPER_FILE,
            static_program(Arch::Amd64)[..100].to_vec(),
            "a program header (8 bytes at offset 96) is outside the file".into(),
        ),
    ];
    for (file, program, why) in cases {
        let case = Case::of("program", Arch::Amd64, &elf(Arch::Amd64), ROOT);
        let path = case.request.binaries.join(file);
        std::fs::write(&path, program).expect("program");
        let message = case.refused_writing_nothing();
        assert_eq!(
            message,
            format!(
                "{} is not a static linux amd64 program: {why}",
                path.display()
            )
        );
    }
}

#[test]
fn a_missing_program_is_refused_and_nothing_is_written() {
    for file in [HOSTKEEPER_FILE, PXE_FILE] {
        let case = Case::of("absent", Arch::Amd64, &elf(Arch::Amd64), ROOT);
        let path = case.request.binaries.join(file);
        std::fs::remove_file(&path).expect("remove");
        let message = case.refused_writing_nothing();
        assert!(
            message.starts_with(&format!("read {}: ", path.display())),
            "{message}"
        );
    }
}

/// Catches a staged `gh` whose archive was not the pinned one.
#[test]
fn an_archive_whose_sha256_is_not_the_pin_is_refused_and_nothing_is_written() {
    let mut case = Case::of("pin", Arch::Amd64, &elf(Arch::Amd64), ROOT);
    let actual = case.request.gh_sha256;
    let pin = install::Sha256::of(b"another archive");
    case.request.gh_sha256 = pin;
    assert_eq!(
        case.refused_writing_nothing(),
        format!(
            "{}: the download's SHA-256 is {actual}, not the pinned {pin}; nothing was installed",
            case.request.gh_archive.display()
        )
    );
}

#[test]
fn a_missing_archive_or_trusted_root_is_refused_and_nothing_is_written() {
    let mut case = Case::of("missing", Arch::Amd64, &elf(Arch::Amd64), ROOT);
    let archive = case.request.gh_archive.clone();
    case.request.gh_archive = case.dir.join("absent.tar.gz");
    let message = case.refused_writing_nothing();
    let want = format!("read {}: ", case.request.gh_archive.display());
    assert!(message.starts_with(&want), "{message}");

    case.request.gh_archive = archive;
    std::fs::remove_file(&case.request.trusted_root).expect("remove");
    let message = case.refused_writing_nothing();
    let want = format!("read {}: ", case.request.trusted_root.display());
    assert!(message.starts_with(&want), "{message}");
}

/// Catches an archive of another version than the pinned one: its top directory differs.
#[test]
fn an_archive_without_gh_at_the_pinned_version_and_arch_is_refused() {
    let program = elf(Arch::Amd64);
    let case = Case::new(
        "member",
        Arch::Amd64,
        &archive("gh_1.9.0_linux_amd64/bin/gh", &program),
        ROOT.as_bytes(),
    );
    assert_eq!(
        case.refused_writing_nothing(),
        format!(
            "{}: the archive has no \"gh_2.0.0_linux_amd64/bin/gh\"",
            case.request.gh_archive.display()
        )
    );
}

/// Catches a deb of one architecture carrying the `gh` of the other, or a file that is no
/// program.
#[test]
fn a_gh_that_is_not_a_64_bit_little_endian_program_of_the_arch_is_refused() {
    let cases = [
        (
            Arch::Amd64,
            elf(Arch::Arm64),
            "gh is not a linux amd64 program: ELF machine 0xb7, not 0x3e",
        ),
        (
            Arch::Arm64,
            elf(Arch::Amd64),
            "gh is not a linux arm64 program: ELF machine 0x3e, not 0xb7",
        ),
        (
            Arch::Amd64,
            elf_with(1, 1, 0x3e),
            "gh is not a linux amd64 program: ELF class 1 and data 1, not 2 (64-bit) and 1 (little-endian)",
        ),
        (
            Arch::Amd64,
            elf_with(2, 2, 0x3e),
            "gh is not a linux amd64 program: ELF class 2 and data 2, not 2 (64-bit) and 1 (little-endian)",
        ),
        (
            Arch::Amd64,
            b"#!/bin/sh\necho a script, not gh\n".to_vec(),
            "gh is not a linux amd64 program: no ELF magic",
        ),
        (
            Arch::Arm64,
            elf(Arch::Arm64)[..19].to_vec(),
            "gh is not a linux arm64 program: 19 bytes, too short for an ELF header",
        ),
    ];
    for (arch, program, want) in cases {
        let case = Case::of("elf", arch, &program, ROOT);
        assert_eq!(case.refused_writing_nothing(), want);
    }
}

/// The 20 bytes of the header are enough: the shortest program accepted.
#[test]
fn a_gh_of_exactly_an_elf_header_is_taken() {
    let program = elf(Arch::Arm64)[..20].to_vec();
    let case = Case::of("header", Arch::Arm64, &program, ROOT);
    stage(&case.request).expect("staged");
    assert_eq!(
        std::fs::read(case.request.out.join("gh")).expect("gh"),
        program
    );
}

/// Catches an empty capture, an error page or any other file in place of the trusted root.
#[test]
fn a_trusted_root_that_is_not_one_on_every_line_is_refused() {
    let good = r#"{"mediaType":"application/vnd.dev.sigstore.trustedroot+json;version=0.1"}"#;
    let third =
        format!("{good}\n\n{{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+jsonx\"}}\n");
    let cases: [(&[u8], &str); 9] = [
        (b"", "it is empty"),
        (b"\n  \n\n", "it is empty"),
        (b"\xff\xfe", "not UTF-8 text"),
        (
            b"<html>rate limited</html>\n",
            "line 1 is not JSON: expected value at line 1 column 1",
        ),
        (b"[1, 2]\n", "line 1 is not a JSON object"),
        (br#"{"tlogs":[]}"#, "line 1 has no mediaType"),
        (br#"{"mediaType":7}"#, "line 1 has no mediaType"),
        (
            br#"{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json"}"#,
            "line 1 has mediaType \"application/vnd.dev.sigstore.bundle.v0.3+json\", not application/vnd.dev.sigstore.trustedroot+json",
        ),
        (
            third.as_bytes(),
            "line 3 has mediaType \"application/vnd.dev.sigstore.trustedroot+jsonx\", not application/vnd.dev.sigstore.trustedroot+json",
        ),
    ];
    for (root, why) in cases {
        let member = "gh_2.0.0_linux_amd64/bin/gh";
        let case = Case::new(
            "root",
            Arch::Amd64,
            &archive(member, &elf(Arch::Amd64)),
            root,
        );
        assert_eq!(
            case.refused_writing_nothing(),
            format!(
                "{} is not a Sigstore trusted root: {why}",
                case.request.trusted_root.display()
            )
        );
    }
}

/// Blank lines are skipped, and a media type with no `;version=` part is the media type.
#[test]
fn a_trusted_root_with_blank_lines_or_no_version_parameter_is_taken() {
    let root = "\n{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json\"}\n\n";
    let case = Case::of("blank", Arch::Amd64, &elf(Arch::Amd64), root);
    stage(&case.request).expect("staged");
    assert_eq!(
        std::fs::read_to_string(case.request.out.join("trusted_root.json")).expect("root"),
        root
    );
}

/// A second run replaces the files of the first, and sets the modes whatever they were.
#[test]
fn files_staged_before_are_replaced_and_given_their_modes() {
    let case = Case::of("again", Arch::Amd64, &elf(Arch::Amd64), ROOT);
    let out = &case.request.out;
    std::fs::create_dir_all(out).expect("out");
    for (name, mode) in [
        ("gh", 0o600),
        ("trusted_root.json", 0o777),
        ("hostkeeper", 0o666),
        ("hostkeeper-pxe", 0o600),
    ] {
        let path = out.join(name);
        std::fs::write(
            &path,
            b"an older, longer file than the one staged now ...........",
        )
        .expect("old");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
    }
    stage(&case.request).expect("staged");
    assert_eq!(std::fs::read(out.join("gh")).expect("gh"), elf(Arch::Amd64));
    assert_eq!(mode(&out.join("gh")), 0o755);
    assert_eq!(
        std::fs::read_to_string(out.join("trusted_root.json")).expect("root"),
        ROOT
    );
    assert_eq!(mode(&out.join("trusted_root.json")), 0o644);
    for file in [HOSTKEEPER_FILE, PXE_FILE] {
        assert_eq!(
            std::fs::read(out.join(file)).expect("program"),
            static_program(Arch::Amd64)
        );
        assert_eq!(mode(&out.join(file)), 0o755);
    }
}

#[test]
fn an_out_that_cannot_be_made_or_written_is_an_error() {
    let mut case = Case::of("write", Arch::Amd64, &elf(Arch::Amd64), ROOT);
    let file = case.dir.join("a-file");
    std::fs::write(&file, b"").expect("file");
    case.request.out = file.join("nfpm");
    let message = case.refused();
    assert!(
        message.starts_with(&format!("create {}: ", case.request.out.display())),
        "{message}"
    );

    for (blocked, staged) in [
        ("gh", None),
        ("trusted_root.json", Some("gh")),
        ("hostkeeper", Some("trusted_root.json")),
        ("hostkeeper-pxe", Some("hostkeeper")),
    ] {
        let case = Case::of("write", Arch::Amd64, &elf(Arch::Amd64), ROOT);
        let out: PathBuf = case.request.out.clone();
        std::fs::create_dir_all(out.join(blocked)).expect("a directory in the way");
        let message = case.refused();
        assert!(
            message.starts_with(&format!("write {}: ", out.join(blocked).display())),
            "{message}"
        );
        if let Some(staged) = staged {
            assert!(out.join(staged).is_file());
        }
    }
}

#[test]
fn the_member_is_the_archive_path_of_gh_for_the_version_and_arch() {
    let version: Version = "2.102.1".parse().expect("version");
    assert_eq!(
        gh_member(&version, Arch::Amd64),
        PathBuf::from("gh_2.102.1_linux_amd64/bin/gh")
    );
    assert_eq!(
        gh_member(&version, Arch::Arm64),
        PathBuf::from("gh_2.102.1_linux_arm64/bin/gh")
    );
}

/// The argv of the `release` command, as clap reads it.
#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    verb: crate::release::Verb,
}

/// `release nfpm-stage` with the flags of `case`'s request, as a workflow step spells them.
fn verb(case: &Case) -> crate::release::Verb {
    use clap::Parser as _;
    let r = &case.request;
    let (version, sha) = (r.gh_version.to_string(), r.gh_sha256.to_string());
    let argv = [
        "release",
        "nfpm-stage",
        "--arch",
        "arm64",
        "--gh-version",
        &version,
        "--gh-archive",
        r.gh_archive.to_str().expect("UTF-8"),
        "--gh-sha256",
        &sha,
        "--trusted-root",
        r.trusted_root.to_str().expect("UTF-8"),
        "--binaries",
        r.binaries.to_str().expect("UTF-8"),
        "--out",
        r.out.to_str().expect("UTF-8"),
    ];
    Args::try_parse_from(argv).expect("parse the argv").verb
}

/// Catches a flag of `nfpm-stage` that does not reach the staging step, the verb building the
/// real adapters though it reads no port, or `run_with` failing it instead of handing it back.
#[test]
fn the_verb_gets_its_flags_and_asks_for_no_adapters() {
    use std::cell::Cell;

    use crate::release::fakes::{FakeHistory, Fakes};
    use crate::release::ports::Adapters as _;
    let case = Case::of("verb", Arch::Arm64, &elf(Arch::Arm64), ROOT);
    let connected = Cell::new(false);
    let out = crate::release::run(verb(&case), || {
        connected.set(true);
        Fakes::over(FakeHistory::default())
    });
    assert!(!connected.get());
    assert_eq!(out, stage(&case.request));
    assert_eq!(
        std::fs::read(case.request.out.join("gh")).expect("gh"),
        elf(Arch::Arm64)
    );

    let case = Case::of("verb-lent", Arch::Arm64, &elf(Arch::Arm64), ROOT);
    let fakes = Fakes::over(FakeHistory::default());
    let out = crate::release::run_with(verb(&case), &fakes.ports());
    assert_eq!(out, stage(&case.request));
    assert!(case.request.out.join("trusted_root.json").is_file());
    assert!(case.request.out.join("hostkeeper-pxe").is_file());
}
