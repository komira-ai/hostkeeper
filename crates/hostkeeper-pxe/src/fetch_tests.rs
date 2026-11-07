//! The fetch end to end, over a release made in memory ([`crate::testing::release`]): the daemon's
//! verifier (`GhVerifier`) over a fake `gh`, a fake network, a fake clock, and a scratch
//! directory. Each refusal is held to its exact text, and to keeping nothing.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;

use hostkeeper_package::ports::NetError;
use serde_json::json;

use super::*;
use crate::testing::bundle::{GhMode, Signer};
use crate::testing::deb::{Kind, deb};
use crate::testing::release::{
    MAX_DEB_BYTES, PlatformFiles, RELEASE, Release, deb_url, grub_deb_path, grub_path, iso_name,
    iso_url, shim_deb_path, shim_path,
};
use crate::testing::{Serve, scratch, sha256_hex};

const ARM: BootPlatform = BootPlatform::LinuxArm64;
const X86: BootPlatform = BootPlatform::LinuxX86_64;
const BOTH: [BootPlatform; 2] = [ARM, X86];

/// The files of a directory and its subdirectories, by path, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).expect("the directory") {
        let entry = entry.expect("an entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.file_type().expect("its type").is_dir() {
            true => found.extend(
                listing(&entry.path())
                    .into_iter()
                    .map(|inner| format!("{name}/{inner}")),
            ),
            false => found.push(name),
        }
    }
    found.sort();
    found
}

/// The sealed line of `bytes` named `name`.
fn sealed(name: &str, bytes: &[u8]) -> String {
    format!(
        "{{\"bytes\":{},\"name\":\"{name}\",\"sha256\":\"{}\"}}",
        bytes.len(),
        sha256_hex(bytes)
    )
}

/// The seal's text for the platforms `platforms` of `release`.
fn seal_text(release: &Release, platforms: &[BootPlatform]) -> String {
    let commit = crate::testing::bundle::COMMIT;
    let mut sets = Vec::new();
    for platform in platforms {
        let files: &PlatformFiles = &release.files[platform];
        let (p, efi, arch) = (platform.as_str(), platform.efi_arch(), platform.deb_arch());
        sets.push(format!(
            "\"{p}\":{{\"deb\":{{\"bytes\":{},\"commit\":\"{commit}\",\"name\":\"deb/hostkeeper_{arch}.deb\",\
             \"sha256\":\"{}\",\"source_ref\":\"refs/heads/main\"}},\"grub\":{},\"initrd\":{},\"iso\":{},\
             \"kernel\":{},\"shim\":{}}}",
            files.hostkeeper_deb.len(),
            sha256_hex(&files.hostkeeper_deb),
            sealed(&format!("hostkeeper/grub{efi}.efi"), &files.grub),
            sealed(&format!("initrd-{p}"), &files.initrd),
            sealed(&iso_name(*platform), &files.iso),
            sealed(&format!("vmlinuz-{p}"), &files.kernel),
            sealed(&format!("hostkeeper/shim{efi}.efi"), &files.shim),
        ));
    }
    format!(
        "{{\"netboot\":{{\"commit\":\"{commit}\",\"sha256\":\"{}\",\"source_ref\":\"refs/heads/main\"}},\
         \"platforms\":{{{}}},\"seal\":1,\"ubuntu\":\"24.04.5\",\"version\":\"1.2.3\"}}\n",
        sha256_hex(&release.netboot_text()),
        sets.join(",")
    )
}

/// With no platform named the command asks for both (see the command's tests); a fetch of both
/// writes, for each, the ISO, the four boot files under their served names and the deb, each
/// byte for byte the release's, and the seal, whose text is exactly what each file measured.
/// Nothing else is kept: no bundle, no `netboot.json`, no `gh` home. `gh` ran three times
/// (`netboot.json` and two debs), and the assets were asked for in the order of the steps.
///
/// Catches a fetch that serves a file it did not check, a seal that names a file by another
/// name or digest than the one written, a verification directory left behind, and a fetch that
/// downloads an ISO before the debs are attested.
#[test]
fn both_platforms_are_fetched_checked_and_sealed() {
    let release = Release::new();
    let run = release.run("both", &BOTH, GhMode::Checks);
    let seal = run.result.as_ref().expect("sealed");
    assert_eq!(seal.to_json(), seal_text(&release, &BOTH));
    let on_disk = fs::read_to_string(run.dir.join("seal.json")).expect("the seal");
    assert_eq!(on_disk, seal_text(&release, &BOTH));
    assert_eq!(
        listing(&run.dir),
        [
            "deb/hostkeeper_amd64.deb",
            "deb/hostkeeper_arm64.deb",
            "hostkeeper/grubaa64.efi",
            "hostkeeper/grubx64.efi",
            "hostkeeper/shimaa64.efi",
            "hostkeeper/shimx64.efi",
            "initrd-linux-arm64",
            "initrd-linux-x86_64",
            "seal.json",
            "ubuntu-24.04.5-live-server-amd64.iso",
            "ubuntu-24.04.5-live-server-arm64.iso",
            "vmlinuz-linux-arm64",
            "vmlinuz-linux-x86_64",
        ]
    );
    for platform in BOTH {
        let files = &release.files[&platform];
        let (p, efi) = (platform.as_str(), platform.efi_arch());
        for (name, bytes) in [
            (format!("vmlinuz-{p}"), &files.kernel),
            (format!("initrd-{p}"), &files.initrd),
            (format!("hostkeeper/shim{efi}.efi"), &files.shim),
            (format!("hostkeeper/grub{efi}.efi"), &files.grub),
            (iso_name(platform), &files.iso),
            (
                format!("deb/hostkeeper_{}.deb", platform.deb_arch()),
                &files.hostkeeper_deb,
            ),
        ] {
            let path = run.dir.join(&name);
            assert_eq!(&fs::read(&path).expect("a sealed file"), bytes, "{name}");
            let mode = fs::metadata(&path)
                .expect("its metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o644, "{name}");
        }
    }
    let mode = fs::metadata(&run.dir)
        .expect("the directory")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(run.gh_runs, 3);
    assert_eq!(
        run.asked,
        [
            format!("{RELEASE}/netboot.json"),
            format!("{RELEASE}/netboot.json.sigstore.json"),
            deb_url(ARM),
            format!("{}.sigstore.json", deb_url(ARM)),
            deb_url(X86),
            format!("{}.sigstore.json", deb_url(X86)),
            iso_url(ARM),
            iso_url(X86),
        ]
    );
}

/// A fetch of one platform asks for nothing of the other, keeps nothing of it, and seals only
/// it; `netboot.json` may even lack the other's boot set, since the boot set of a platform not
/// asked for is not read.
///
/// Catches a farm of one architecture made to download and keep an ISO it never serves, and a
/// reader that needs every boot set to fetch one.
#[test]
fn only_the_platforms_asked_for_are_fetched() {
    let mut release = Release::new();
    release.netboot["platforms"]
        .as_object_mut()
        .expect("the boot sets")
        .remove("linux-arm64");
    let run = release.run("one", &[X86], GhMode::Checks);
    let seal = run.result.as_ref().expect("sealed");
    assert_eq!(seal.to_json(), seal_text(&release, &[X86]));
    assert!(
        run.asked.iter().all(|url| !url.contains("arm64")),
        "{:?}",
        run.asked
    );
    assert!(
        listing(&run.dir)
            .iter()
            .all(|name| !name.contains("arm64") & !name.contains("aa64"))
    );
}

/// `netboot.json` attested by another organisation, by another workflow of the organisation, or
/// on a ref the signer does not allow is refused, naming the attestation and why; nothing after
/// its bundle is downloaded, and the directory is gone. The ref is refused before `gh` runs.
///
/// Catches boot files pinned by an asset anyone could have attested, which `serve` would then
/// put on a new machine's disk.
#[test]
fn netboot_json_attested_by_another_signer_is_refused() {
    let trusted = Signer::trusted();
    let cases = [
        (
            Signer {
                organisation: "other-org".to_owned(),
                ..trusted.clone()
            },
            "netboot.json: not attested by the trusted signer: gh: verification failed: --owner",
            1,
        ),
        (
            Signer {
                workflow: ".github/workflows/release.yml".to_owned(),
                ..trusted.clone()
            },
            "netboot.json: not attested by the trusted signer: gh: verification failed: \
             --cert-identity-regex",
            1,
        ),
        (
            Signer {
                source_ref: "refs/heads/feature".to_owned(),
                ..trusted.clone()
            },
            "netboot.json: not attested by the trusted signer: the ref refs/heads/feature matches \
             none of the trusted signer's refs",
            0,
        ),
    ];
    for (signer, text, gh_runs) in cases {
        let mut release = Release::new();
        release.netboot_signer = signer;
        let run = release.run("netboot-signer", &BOTH, GhMode::Checks);
        assert_eq!(run.refused(), text);
        assert_eq!(run.gh_runs, gh_runs, "{text}");
        assert_eq!(
            run.asked,
            [
                format!("{RELEASE}/netboot.json"),
                format!("{RELEASE}/netboot.json.sigstore.json")
            ]
        );
    }
}

/// A deb whose attestation does not verify is refused, naming the deb: one whose bundle lists
/// other bytes (refused before `gh` runs), and one attested by another organisation. No ISO is
/// downloaded.
///
/// Catches a deb installed on every new machine that the trusted signer never built.
#[test]
fn a_deb_whose_attestation_does_not_verify_is_refused() {
    let mut release = Release::new();
    release.deb_subject = Some(sha256_hex(b"other bytes"));
    let run = release.run("deb-subject", &BOTH, GhMode::Checks);
    let deb = sha256_hex(&release.files[&ARM].hostkeeper_deb);
    assert_eq!(
        run.refused(),
        format!(
            "hostkeeper_1.2.3_arm64.deb: not attested: its bundle does not list its SHA-256 {deb}"
        )
    );
    assert_eq!(run.gh_runs, 1);

    let mut release = Release::new();
    release.deb_signer = Signer {
        organisation: "other-org".to_owned(),
        ..Signer::trusted()
    };
    let run = release.run("deb-signer", &[X86], GhMode::Checks);
    assert_eq!(
        run.refused(),
        "hostkeeper_1.2.3_amd64.deb: not attested by the trusted signer: gh: verification \
         failed: --owner"
    );
    assert!(
        run.asked.iter().all(|url| !url.ends_with(".iso")),
        "{:?}",
        run.asked
    );
}

/// A bundle that is not a bundle is refused as unreadable, naming what it lacks, before `gh`
/// runs; a `gh` that cannot be started, and one whose verdict comes after the deadline, are
/// "could not be checked", never a pass.
///
/// Catches an unreadable bundle taken as an attestation, a verifier that is down taken as a
/// yes, and a verdict that came too late taken at all.
#[test]
fn an_attestation_that_cannot_be_checked_is_refused() {
    let mut release = Release::new();
    release.overrides.push((
        format!("{RELEASE}/netboot.json.sigstore.json"),
        Serve::Body(b"{}".to_vec()),
    ));
    let run = release.run("bundle-unreadable", &BOTH, GhMode::Checks);
    assert_eq!(
        run.refused(),
        "netboot.json: not attested: its bundle is unreadable: the bundle has no DSSE payload"
    );
    assert_eq!(run.gh_runs, 0);
    for (mode, text) in [
        (GhMode::Missing, "netboot.json: the verifier is unavailable"),
        (GhMode::Slow, "netboot.json: the verifier ran out of time"),
    ] {
        let run = Release::new().run("gh-unavailable", &BOTH, mode);
        assert_eq!(run.refused(), text);
    }
}

/// A missing file is refused by name: `netboot.json`, a deb, a bundle and an ISO the server does
/// not have (404); a kernel whose path is not in the ISO; a shim whose path is not in its deb; a
/// GRUB whose deb is not in the ISO's pool.
///
/// Catches a fetch that seals a boot set with a file missing, which `serve` would then fail to
/// serve to a machine mid-boot.
#[test]
fn a_missing_file_is_refused_by_name() {
    let gone = |url: String| {
        let mut release = Release::new();
        release.overrides.push((url, Serve::Status(404)));
        release
    };
    for (release, text) in [
        (
            gone(format!("{RELEASE}/netboot.json")),
            format!("{RELEASE}/netboot.json: the server answered with status 404"),
        ),
        (
            gone(format!("{RELEASE}/netboot.json.sigstore.json")),
            format!("{RELEASE}/netboot.json.sigstore.json: the server answered with status 404"),
        ),
        (
            gone(deb_url(X86)),
            format!("{}: the server answered with status 404", deb_url(X86)),
        ),
        (
            gone(iso_url(ARM)),
            format!("{}: the server answered with status 404", iso_url(ARM)),
        ),
    ] {
        let run = release.run("missing-download", &BOTH, GhMode::Checks);
        assert_eq!(run.refused(), text);
    }

    let mut release = Release::new();
    release.netboot["platforms"]["linux-x86_64"]["kernel"]["iso_path"] =
        json!("casper/hwe-vmlinuz");
    let run = release.run("missing-kernel", &[X86], GhMode::Checks);
    assert_eq!(
        run.refused(),
        "linux-x86_64 kernel vmlinuz-linux-x86_64: casper/hwe-vmlinuz is not in \
         ubuntu-24.04.5-live-server-amd64.iso"
    );

    let mut release = Release::new();
    release.netboot["platforms"]["linux-arm64"]["shim"]["deb_path"] =
        json!("usr/lib/shim/shimaa64.efi.signed.previous");
    let run = release.run("missing-shim", &[ARM], GhMode::Checks);
    assert_eq!(
        run.refused(),
        format!(
            "linux-arm64 shim hostkeeper/shimaa64.efi: usr/lib/shim/shimaa64.efi.signed.previous \
             is not in {}",
            shim_deb_path(ARM)
        )
    );

    let mut release = Release::new();
    release.netboot["platforms"]["linux-arm64"]["grub"]["iso_path"] = json!("pool/main/g/none.deb");
    let run = release.run("missing-grub-deb", &[ARM], GhMode::Checks);
    assert_eq!(
        run.refused(),
        "linux-arm64 grub hostkeeper/grubaa64.efi: pool/main/g/none.deb is not in \
         ubuntu-24.04.5-live-server-arm64.iso"
    );
}

/// A size mismatch and a digest mismatch are refused, naming the file and both values: an ISO
/// longer than its pin (the download stops one byte past it), one shorter, one of the pinned
/// size and another digest; a kernel, a shim and a GRUB likewise once taken out.
///
/// Catches a tampered or stale boot file sealed as the release's.
#[test]
fn a_size_or_digest_mismatch_is_refused_by_name() {
    let files = PlatformFiles::of(X86);
    let iso_bytes = files.iso.len() as u64;
    let pin = |part: &str, key: &str, value: serde_json::Value| {
        let mut release = Release::new();
        release.netboot["platforms"]["linux-x86_64"][part][key] = value;
        release
    };
    let iso = "linux-x86_64 iso ubuntu-24.04.5-live-server-amd64.iso";
    let kernel_sha = sha256_hex(&files.kernel);
    let cases = [
        (
            pin("iso", "bytes", json!(iso_bytes - 1)),
            format!("{iso}: more than {} bytes, but netboot.json pins {} bytes", iso_bytes - 1, iso_bytes - 1),
        ),
        (
            pin("iso", "bytes", json!(iso_bytes + 1)),
            format!("{iso}: {iso_bytes} bytes, but netboot.json pins {} bytes", iso_bytes + 1),
        ),
        (
            pin("iso", "sha256", json!("0".repeat(64))),
            format!(
                "{iso}: SHA-256 {}, but netboot.json pins {}",
                sha256_hex(&files.iso),
                "0".repeat(64)
            ),
        ),
        (
            pin("kernel", "sha256", json!("a".repeat(64))),
            format!(
                "linux-x86_64 kernel vmlinuz-linux-x86_64: SHA-256 {kernel_sha}, but netboot.json pins {}",
                "a".repeat(64)
            ),
        ),
        (
            pin("kernel", "bytes", json!(3)),
            "linux-x86_64 kernel vmlinuz-linux-x86_64: more than 3 bytes, but netboot.json pins 3 bytes"
                .to_owned(),
        ),
        (
            pin("shim", "bytes", json!(files.shim.len() + 5)),
            format!(
                "linux-x86_64 shim hostkeeper/shimx64.efi: {} bytes, but netboot.json pins {} bytes",
                files.shim.len(),
                files.shim.len() + 5
            ),
        ),
        (
            pin("grub", "bytes", json!(2)),
            "linux-x86_64 grub hostkeeper/grubx64.efi: more than 2 bytes, but netboot.json pins 2 bytes"
                .to_owned(),
        ),
    ];
    for (release, text) in cases {
        let run = release.run("mismatch", &[X86], GhMode::Checks);
        assert_eq!(run.refused(), text);
    }
}

/// An ISO that is not an image, a path that names a directory of the ISO, a pool deb that is
/// not a deb and a shim that is a link in its deb are each refused by name.
///
/// Catches a wrong pin that would make the fetch seal a directory listing, garbage or a link
/// target as a boot file.
#[test]
fn a_file_that_cannot_be_taken_out_is_refused_by_name() {
    let mut release = Release::new();
    release.files.get_mut(&X86).expect("x86").iso = b"not an image".repeat(200);
    release.netboot = release.netboot();
    let run = release.run("not-an-iso", &[X86], GhMode::Checks);
    assert_eq!(
        run.refused(),
        "linux-x86_64 iso ubuntu-24.04.5-live-server-amd64.iso: \
         ubuntu-24.04.5-live-server-amd64.iso cannot be read: a read runs past the end of the image"
    );

    let mut release = Release::new();
    release.netboot["platforms"]["linux-x86_64"]["initrd"]["iso_path"] = json!("casper");
    let run = release.run("a-directory", &[X86], GhMode::Checks);
    assert_eq!(
        run.refused(),
        "linux-x86_64 initrd initrd-linux-x86_64: casper in ubuntu-24.04.5-live-server-amd64.iso \
         is not a regular file"
    );

    let mut release = Release::new();
    release.netboot["platforms"]["linux-x86_64"]["grub"]["iso_path"] = json!("md5sum.txt");
    let run = release.run("not-a-deb", &[X86], GhMode::Checks);
    assert_eq!(
        run.refused(),
        "linux-x86_64 grub hostkeeper/grubx64.efi: md5sum.txt cannot be read: not an ar archive"
    );

    let mut release = Release::new();
    let files = release.files.get_mut(&X86).expect("x86");
    files.shim_deb = deb("data.tar.xz", &[(&shim_path(X86), b"", Kind::Symlink)]);
    files.iso = files.image().build();
    release.netboot = release.netboot();
    let run = release.run("a-link", &[X86], GhMode::Checks);
    assert_eq!(
        run.refused(),
        format!(
            "linux-x86_64 shim hostkeeper/shimx64.efi: {} in {} is not a regular file",
            shim_path(X86),
            shim_deb_path(X86)
        )
    );
}

/// A `netboot.json` the reader refuses, once attested, is refused naming its problem; so is one
/// with no boot set for a platform asked for. No deb is downloaded.
///
/// Catches a fetch that guesses at an asset of another form.
#[test]
fn a_netboot_json_of_another_form_is_refused() {
    let mut release = Release::new();
    release.netboot["version"] = json!("1.2");
    let run = release.run("netboot-form", &BOTH, GhMode::Checks);
    assert_eq!(run.refused(), "netboot.json: version \"1.2\" is not X.Y.Z");
    assert_eq!(run.asked.len(), 2);

    let mut release = Release::new();
    release.netboot["platforms"]
        .as_object_mut()
        .expect("the boot sets")
        .remove("linux-arm64");
    let run = release.run("netboot-no-set", &BOTH, GhMode::Checks);
    assert_eq!(
        run.refused(),
        "netboot.json: has no boot set for linux-arm64"
    );
}

/// `netboot.json`, a bundle and a deb over their bounds are refused naming the URL and the bound;
/// a body cut off mid-way and no connection are refused naming the URL.
///
/// Catches an unbounded download of an asset that has no pin.
#[test]
fn an_asset_over_its_bound_or_cut_short_is_refused() {
    let with = |url: String, serve: Serve| {
        let mut release = Release::new();
        release.overrides.push((url, serve));
        release
    };
    let cases = [
        (
            with(
                format!("{RELEASE}/netboot.json"),
                Serve::Body(vec![b' '; (1 << 20) + 1]),
            ),
            format!("{RELEASE}/netboot.json: more than 1048576 bytes"),
        ),
        (
            with(
                format!("{RELEASE}/netboot.json.sigstore.json"),
                Serve::Body(vec![b' '; (1 << 20) + 1]),
            ),
            format!("{RELEASE}/netboot.json.sigstore.json: more than 1048576 bytes"),
        ),
        (
            with(
                deb_url(ARM),
                Serve::Body(vec![0; MAX_DEB_BYTES as usize + 1]),
            ),
            format!("{}: more than {MAX_DEB_BYTES} bytes", deb_url(ARM)),
        ),
        (
            with(deb_url(ARM), Serve::Broken(b"part".to_vec())),
            format!(
                "{}: reading the body failed: the connection was reset",
                deb_url(ARM)
            ),
        ),
        (
            with(iso_url(X86), Serve::Fail(NetError::Connect)),
            format!(
                "{}: could not connect, or the request was not answered",
                iso_url(X86)
            ),
        ),
    ];
    for (release, text) in cases {
        let run = release.run("bounds", &BOTH, GhMode::Checks);
        assert_eq!(run.refused(), text);
    }
}

/// A directory that already exists is refused and left as it was; a directory whose parent is
/// missing is refused naming it. Nothing is downloaded either way.
///
/// Catches a fetch that writes into, or removes, a directory it did not make.
#[test]
fn the_directory_must_be_new() {
    let scratch = scratch("exists");
    let dir = scratch.path().join("boot");
    fs::create_dir(&dir).expect("make it");
    fs::write(dir.join("kept"), b"the operator's").expect("a file in it");
    let transport = Release::new().routes();
    let clock = crate::testing::FakeClock::at(1);
    let verifier = crate::testing::bundle::verifier(
        crate::testing::bundle::FakeGh {
            mode: GhMode::Checks,
            runs: Default::default(),
            clock: std::rc::Rc::new(crate::testing::FakeClock::at(1)),
        },
        &dir,
    );
    let ports = Ports {
        transport: &transport,
        verifier: &verifier,
        clock: &clock,
    };
    let refused =
        fetch(&ports, &crate::testing::release::request(&dir, &BOTH)).expect_err("exists");
    assert_eq!(
        refused.to_string(),
        format!(
            "{} already exists: fetch writes a new directory",
            dir.display()
        )
    );
    assert_eq!(fs::read(dir.join("kept")).expect("kept"), b"the operator's");

    let orphan = scratch.path().join("no-parent").join("boot");
    let refused =
        fetch(&ports, &crate::testing::release::request(&orphan, &BOTH)).expect_err("no parent");
    assert!(
        matches!(&refused, FetchError::Io { path, source } if *path == orphan
            && source.kind() == io::ErrorKind::NotFound),
        "{refused:?}"
    );
    assert!(transport.asked().is_empty());
}

/// A refusal after which the directory cannot be removed says both.
///
/// Catches a refused fetch that leaves files behind and says nothing of them.
#[test]
fn a_directory_that_cannot_be_removed_is_named() {
    let scratch = scratch("unremovable");
    let missing = scratch.path().join("never-made");
    let refusal = discard(
        &missing,
        FetchError::Netboot(NetbootInvalid("broken".to_owned())),
    );
    assert!(
        refusal.to_string().starts_with(&format!(
            "netboot.json: broken; and {} could not be removed: ",
            missing.display()
        )),
        "{refusal}"
    );
}

/// Each way a download or a copy out of a deb or the ISO can fail maps to the refusal that
/// names it: a write that fails is the file's I/O error, a read that fails is the URL's or the
/// container's, and a deb that is too large or unreadable names the deb.
///
/// Catches a full disk reported as a bad release, and the reverse.
#[test]
fn every_failure_maps_to_the_refusal_that_names_it() {
    let path = Path::new("/the-dir/x");
    let full = || io::Error::from(io::ErrorKind::StorageFull);
    let refusal = download_failure(
        "https://u.example",
        path,
        DownloadError::Body(CopyError::Write(full())),
    );
    assert!(
        matches!(&refusal, FetchError::Io { path: p, .. } if p == path),
        "{refusal:?}"
    );
    let refusal = download_failure(
        "https://u.example",
        path,
        DownloadError::Body(CopyError::TooLarge(7)),
    );
    assert_eq!(refusal.to_string(), "https://u.example: more than 7 bytes");

    let id = FileId {
        platform: X86,
        part: Part::Shim,
        name: "hostkeeper/shimx64.efi".to_owned(),
    };
    let file = InDeb {
        iso_path: "pool/s.deb".to_owned(),
        deb_path: "usr/s".to_owned(),
        pin: Pin {
            name: "hostkeeper/shimx64.efi".to_owned(),
            bytes: 9,
            sha256: "0".repeat(64),
        },
    };
    let refusal = deb_failure(&id, &file, path, DebError::Copy(CopyError::Write(full())));
    assert!(
        matches!(&refusal, FetchError::Io { path: p, .. } if p == path),
        "{refusal:?}"
    );
    let refusal = deb_failure(
        &id,
        &file,
        path,
        DebError::Copy(CopyError::Read(io::Error::other("bad xz"))),
    );
    assert_eq!(
        refusal.to_string(),
        "linux-x86_64 shim hostkeeper/shimx64.efi: pool/s.deb cannot be read: bad xz"
    );
    let refusal = iso_failure(
        &id,
        "pool/s.deb",
        "u.iso",
        IsoError::Io(io::Error::other("eio")),
    );
    assert_eq!(
        refusal.to_string(),
        "linux-x86_64 shim hostkeeper/shimx64.efi: u.iso cannot be read: eio"
    );
    assert_eq!(Part::Deb.to_string(), "deb");
}

/// The shim and GRUB are each taken out of the right deb even when the two debs hold files of
/// the same name; the deb paths and pool paths of both platforms differ only by architecture.
///
/// Catches an arm64 machine sealed with the x86-64 shim.
#[test]
fn each_platform_takes_its_own_loaders() {
    let release = Release::new();
    let run = release.run("loaders", &BOTH, GhMode::Checks);
    run.result.as_ref().expect("sealed");
    for platform in BOTH {
        let efi = platform.efi_arch();
        let shim = fs::read(run.dir.join(format!("hostkeeper/shim{efi}.efi"))).expect("the shim");
        assert_eq!(shim, format!("the shim of {platform}\n").into_bytes());
        let grub = fs::read(run.dir.join(format!("hostkeeper/grub{efi}.efi"))).expect("GRUB");
        assert_eq!(
            grub,
            format!("the network GRUB of {platform}\n").into_bytes()
        );
        assert!(grub_deb_path(platform).contains(platform.deb_arch()));
        assert!(grub_path(platform).contains(efi));
    }
}

/// A copy out of the ISO or a deb that fails maps to the refusal naming it: a file past its pin
/// is a size mismatch, a read that fails names the container, a write that fails is the file's
/// I/O error, and a deb that is not of its form names the deb.
///
/// Catches a full disk while a kernel is copied out reported as a bad ISO.
#[test]
fn a_failed_copy_maps_to_the_refusal_that_names_it() {
    let id = FileId {
        platform: ARM,
        part: Part::Kernel,
        name: "vmlinuz-linux-arm64".to_owned(),
    };
    let pin = Pin {
        name: id.name.clone(),
        bytes: 5,
        sha256: "0".repeat(64),
    };
    let path = Path::new("/the-dir/vmlinuz-linux-arm64");
    let iso = "u.iso";
    assert_eq!(
        copy_failure(&id, &pin, iso, path, CopyError::TooLarge(5)).to_string(),
        "linux-arm64 kernel vmlinuz-linux-arm64: more than 5 bytes, but netboot.json pins 5 bytes"
    );
    assert_eq!(
        copy_failure(
            &id,
            &pin,
            iso,
            path,
            CopyError::Read(io::Error::other("eio"))
        )
        .to_string(),
        "linux-arm64 kernel vmlinuz-linux-arm64: u.iso cannot be read: eio"
    );
    let full = copy_failure(
        &id,
        &pin,
        iso,
        path,
        CopyError::Write(io::Error::from(io::ErrorKind::StorageFull)),
    );
    assert_eq!(
        full.to_string(),
        format!(
            "/the-dir/vmlinuz-linux-arm64: {}",
            io::Error::from(io::ErrorKind::StorageFull)
        )
    );
    let file = InDeb {
        iso_path: "pool/s.deb".to_owned(),
        deb_path: "usr/s".to_owned(),
        pin,
    };
    assert_eq!(
        deb_failure(
            &id,
            &file,
            path,
            DebError::Malformed("not an ar archive".to_owned())
        )
        .to_string(),
        "linux-arm64 kernel vmlinuz-linux-arm64: pool/s.deb cannot be read: not an ar archive"
    );
}

/// Two boot sets that name one ISO file are refused when the second is made: a file is never
/// written twice or replaced.
///
/// Catches a fetch that overwrites one platform's ISO with the other's after checking it.
#[test]
fn a_file_name_taken_twice_is_refused() {
    let mut release = Release::new();
    release.netboot["platforms"]["linux-arm64"]["iso"]["name"] = json!(iso_name(X86));
    let run = release.run("one-name", &BOTH, GhMode::Checks);
    let refused = run.refused();
    let path = run.dir.join(iso_name(X86));
    assert_eq!(
        refused,
        format!("{}: File exists (os error 17)", path.display())
    );
}
