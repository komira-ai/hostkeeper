//! The seal check at start: a sealed directory opens, and every difference from its seal is
//! refused, naming the file.

use std::io::ErrorKind;
use std::path::Path;

use hostkeeper_pxe_seed::inputs::LinuxPlatform;

use super::kit::{ISO_BYTES, SHIM_BYTES, content, iso_name, seal_dir};
use crate::seal::{Seal, SealedFile};
use crate::serve::sealed::{SealError, Sealed};
use crate::testing::scratch;

/// Seals both platforms at `dir`, changes the seal with `change`, writes it back, and opens.
fn open_with(dir: &Path, change: impl FnOnce(&mut Seal)) -> Result<Sealed, SealError> {
    let mut seal = seal_dir(dir, &LinuxPlatform::ALL);
    change(&mut seal);
    std::fs::write(dir.join("seal.json"), seal.to_json()).expect("write the seal");
    Sealed::open(dir)
}

/// The refusal's text.
fn refusal(result: Result<Sealed, SealError>) -> String {
    result.expect_err("refused").to_string()
}

/// A directory sealed for both platforms opens: every sealed file by its served name, each
/// set with its ISO and loader names, the release's version, and a platform key this release
/// does not serve skipped and named. A sealed file reads back by offset, and a read past its end
/// is an error, not a short answer.
///
/// Catches a served name that differs from the one the menus and seeds name, and a truncated
/// file sent as if whole.
#[test]
fn a_sealed_directory_opens_with_every_file_by_its_served_name() {
    let scratch = scratch("sealed-open");
    let dir = scratch.path().join("boot");
    let sealed = open_with(&dir, |seal| {
        let mut extra = seal.platforms["linux-arm64"].clone();
        extra.iso.name = "riscv.iso".to_owned();
        seal.platforms.insert("linux-riscv64".to_owned(), extra);
    })
    .expect("opens");
    let names: Vec<&str> = sealed.names().collect();
    assert_eq!(
        names,
        [
            "deb/hostkeeper_amd64.deb",
            "deb/hostkeeper_arm64.deb",
            "hostkeeper/grubaa64.efi",
            "hostkeeper/grubx64.efi",
            "hostkeeper/shimaa64.efi",
            "hostkeeper/shimx64.efi",
            "initrd-linux-arm64",
            "initrd-linux-x86_64",
            "ubuntu-24.04.5-live-server-amd64.iso",
            "ubuntu-24.04.5-live-server-arm64.iso",
            "vmlinuz-linux-arm64",
            "vmlinuz-linux-x86_64",
        ]
    );
    assert_eq!(sealed.skipped(), ["linux-riscv64"]);
    assert_eq!(sealed.version(), "1.2.3");
    assert_eq!(
        sealed.platforms().collect::<Vec<_>>(),
        LinuxPlatform::ALL.to_vec()
    );
    let sets: Vec<(LinuxPlatform, String, String, String)> = sealed
        .sets()
        .map(|(p, set)| {
            (
                p,
                set.iso.as_str().to_owned(),
                set.shim.clone(),
                set.grub.clone(),
            )
        })
        .collect();
    assert_eq!(
        sets,
        [
            (
                LinuxPlatform::X86_64,
                iso_name(LinuxPlatform::X86_64),
                "hostkeeper/shimx64.efi".to_owned(),
                "hostkeeper/grubx64.efi".to_owned()
            ),
            (
                LinuxPlatform::Arm64,
                iso_name(LinuxPlatform::Arm64),
                "hostkeeper/shimaa64.efi".to_owned(),
                "hostkeeper/grubaa64.efi".to_owned()
            ),
        ]
    );
    assert_eq!(
        sealed.set(LinuxPlatform::Arm64).map(|s| s.shim.as_str()),
        Some("hostkeeper/shimaa64.efi")
    );
    let shim = sealed.file("hostkeeper/shimx64.efi").expect("the shim");
    assert_eq!(shim.name(), "hostkeeper/shimx64.efi");
    assert_eq!(shim.bytes(), SHIM_BYTES as u64);
    let whole = content(LinuxPlatform::X86_64, "shim", SHIM_BYTES);
    assert_eq!(shim.read(512, 512).expect("a block"), whole[512..1024]);
    assert_eq!(
        shim.read(SHIM_BYTES as u64 - 10, 11)
            .expect_err("past the end")
            .kind(),
        ErrorKind::UnexpectedEof
    );
    assert!(sealed.file("seal.json").is_none());
    let iso = sealed
        .file(&iso_name(LinuxPlatform::Arm64))
        .expect("the ISO");
    assert_eq!(iso.bytes(), ISO_BYTES as u64);
}

/// A directory with only one platform sealed holds only that set.
///
/// Catches a set reported for a platform that was never fetched.
#[test]
fn a_directory_of_one_platform_holds_only_its_set() {
    let scratch = scratch("sealed-one");
    let dir = scratch.path().join("boot");
    seal_dir(&dir, &[LinuxPlatform::Arm64]);
    let sealed = Sealed::open(&dir).expect("opens");
    assert_eq!(
        sealed.platforms().collect::<Vec<_>>(),
        [LinuxPlatform::Arm64]
    );
    assert!(sealed.set(LinuxPlatform::X86_64).is_none());
    assert!(sealed.skipped().is_empty());
}

/// The seal itself: none (never sealed), one that is not JSON, one of another format, and one
/// naming no platform this release serves are each refused.
///
/// Catches a directory served that `fetch` never verified, and a seal of a newer format read as
/// if this release understood it.
#[test]
fn a_missing_unreadable_newer_or_empty_seal_is_refused() {
    let scratch = scratch("sealed-seal");
    let dir = scratch.path().join("boot");
    std::fs::create_dir_all(&dir).expect("a directory");
    let none = refusal(Sealed::open(&dir));
    assert!(
        none.starts_with("seal.json cannot be read (")
            && none.ends_with("): the directory was not sealed by pxe prepare"),
        "{none}"
    );
    std::fs::write(dir.join("seal.json"), "{\"seal\":").expect("write");
    let unreadable = refusal(Sealed::open(&dir));
    assert!(
        unreadable.starts_with("seal.json is not a seal: "),
        "{unreadable}"
    );
    assert_eq!(
        refusal(open_with(&dir, |seal| seal.seal = 2)),
        "seal.json is of format 2; this release reads format 1"
    );
    assert_eq!(
        refusal(open_with(&dir, |seal| {
            let set = seal.platforms.remove("linux-x86_64").expect("a set");
            seal.platforms.clear();
            seal.platforms.insert("linux-riscv64".to_owned(), set);
        })),
        "seal.json names no platform this release serves (linux-x86_64, linux-arm64)"
    );
}

/// A part sealed under another name than its platform's, and an ISO whose sealed name is not a
/// file name ending in `.iso`, are refused, naming the part and both names.
///
/// Catches a mixed set: the arm64 shim served as the x86-64 one, or an ISO name that would
/// break the install menu.
#[test]
fn a_part_sealed_under_another_name_is_refused() {
    let scratch = scratch("sealed-names");
    let dir = scratch.path().join("boot");
    let renamed = |part: &str, name: &str| {
        let (part, name) = (part.to_owned(), name.to_owned());
        move |seal: &mut Seal| {
            let set = seal.platforms.get_mut("linux-x86_64").expect("a set");
            let entry: &mut SealedFile = match part.as_str() {
                "shim" => &mut set.shim,
                "grub" => &mut set.grub,
                "kernel" => &mut set.kernel,
                "initrd" => &mut set.initrd,
                _ => &mut set.iso,
            };
            entry.name = name;
        }
    };
    assert_eq!(
        refusal(open_with(&dir, renamed("shim", "hostkeeper/shimaa64.efi"))),
        "the shim of linux-x86_64 is sealed as \"hostkeeper/shimaa64.efi\", not \"hostkeeper/shimx64.efi\""
    );
    assert_eq!(
        refusal(open_with(&dir, renamed("kernel", "vmlinuz"))),
        "the kernel of linux-x86_64 is sealed as \"vmlinuz\", not \"vmlinuz-linux-x86_64\""
    );
    assert_eq!(
        refusal(open_with(&dir, |seal| {
            seal.platforms
                .get_mut("linux-arm64")
                .expect("a set")
                .deb
                .name = "deb/hostkeeper_amd64.deb".to_owned();
        })),
        "the deb of linux-arm64 is sealed as \"deb/hostkeeper_amd64.deb\", not \"deb/hostkeeper_arm64.deb\""
    );
    assert_eq!(
        refusal(open_with(&dir, renamed("iso", "../boot.img"))),
        "the ISO of linux-x86_64 is sealed as \"../boot.img\", which is not an ISO's file name"
    );
}

/// A sealed file that is missing, a link, a directory, of another size or of another digest is
/// refused, naming it.
///
/// Catches a tampered or stale download served, and a link followed out of the directory.
#[test]
fn a_file_that_differs_from_its_seal_is_refused() {
    let scratch = scratch("sealed-files");
    let dir = scratch.path().join("boot");
    let kernel = "vmlinuz-linux-x86_64";

    seal_dir(&dir, &LinuxPlatform::ALL);
    std::fs::remove_file(dir.join(kernel)).expect("remove");
    let missing = refusal(Sealed::open(&dir));
    assert_eq!(
        missing,
        format!("{kernel}: {}", std::io::Error::from_raw_os_error(2))
    );

    seal_dir(&dir, &LinuxPlatform::ALL);
    std::fs::rename(dir.join(kernel), dir.join("elsewhere")).expect("move");
    std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join(kernel)).expect("a link");
    let link = refusal(Sealed::open(&dir));
    assert_eq!(
        link,
        format!("{kernel}: {}", std::io::Error::from_raw_os_error(40))
    );
    std::fs::remove_file(dir.join(kernel)).expect("remove the link");

    seal_dir(&dir, &LinuxPlatform::ALL);
    std::fs::remove_file(dir.join(kernel)).expect("remove");
    std::fs::create_dir(dir.join(kernel)).expect("a directory in its place");
    assert_eq!(
        refusal(Sealed::open(&dir)),
        format!("{kernel}: not a regular file")
    );
    std::fs::remove_dir(dir.join(kernel)).expect("remove the directory");

    let seal = seal_dir(&dir, &LinuxPlatform::ALL);
    let sealed = &seal.platforms["linux-x86_64"].kernel;
    std::fs::write(dir.join(kernel), b"short").expect("write");
    assert_eq!(
        refusal(Sealed::open(&dir)),
        format!("{kernel}: 5 bytes, and the seal says {}", sealed.bytes)
    );

    let seal = seal_dir(&dir, &LinuxPlatform::ALL);
    let sealed = &seal.platforms["linux-x86_64"].kernel;
    let mut bytes = std::fs::read(dir.join(kernel)).expect("read");
    bytes[0] ^= 1;
    std::fs::write(dir.join(kernel), &bytes).expect("write");
    assert_eq!(
        refusal(Sealed::open(&dir)),
        format!(
            "{kernel}: its SHA-256 is {}, and the seal says {}",
            crate::testing::sha256_hex(&bytes),
            sealed.sha256
        )
    );
}
