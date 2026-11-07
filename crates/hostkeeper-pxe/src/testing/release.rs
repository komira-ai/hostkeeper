//! A whole release of both platforms, made in memory: for each, an ISO holding the kernel, the
//! initrd and the two pool debs (the shim's with an xz `data.tar`, GRUB's with zstd, as Ubuntu
//! 24.04's are), the hostkeeper deb, and `netboot.json` pinning it all, each asset with its
//! bundle by the trusted signer. A test changes what it needs ([`Release`]'s fields, then
//! [`Release::routes`]) and runs the fetch on it ([`Run`]).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

use hostkeeper_testkit::Scratch;
use serde_json::{Value, json};

use super::bundle::{FakeGh, GhMode, Signer, bundle, signers, verifier};
use super::deb::{Kind, deb};
use super::iso::Image;
use super::{FakeClock, FakeTransport, Serve, scratch, sha256_hex, timeouts};
use crate::fetch::{FetchError, GH_HOME, Ports, Request, VERIFY_DIR, fetch};
use crate::platform::BootPlatform;
use crate::seal::Seal;

/// The release's directory of assets.
pub(crate) const RELEASE: &str = "https://releases.example/v1.2.3";
/// The release's version.
pub(crate) const VERSION: &str = "1.2.3";
/// The most bytes of a deb the tests allow.
pub(crate) const MAX_DEB_BYTES: u64 = 1 << 20;

/// The files of one platform.
#[derive(Clone, Debug)]
pub(crate) struct PlatformFiles {
    pub kernel: Vec<u8>,
    pub initrd: Vec<u8>,
    pub shim: Vec<u8>,
    pub grub: Vec<u8>,
    pub shim_deb: Vec<u8>,
    pub grub_deb: Vec<u8>,
    pub iso: Vec<u8>,
    pub hostkeeper_deb: Vec<u8>,
}

/// The paths of a platform's files in its ISO and its debs.
pub(crate) fn shim_deb_path(platform: BootPlatform) -> String {
    format!(
        "pool/main/s/shim-signed/shim-signed_1.58+15.8-0ubuntu1_{}.deb",
        platform.deb_arch()
    )
}

pub(crate) fn grub_deb_path(platform: BootPlatform) -> String {
    format!(
        "pool/main/g/grub2-signed/grub-efi-{arch}-signed_1.202.5+2.12-1ubuntu7.3_{arch}.deb",
        arch = platform.deb_arch()
    )
}

pub(crate) fn shim_path(platform: BootPlatform) -> String {
    format!("usr/lib/shim/shim{}.efi.signed.latest", platform.efi_arch())
}

pub(crate) fn grub_path(platform: BootPlatform) -> String {
    let grub_arch = match platform {
        BootPlatform::LinuxArm64 => "arm64",
        BootPlatform::LinuxX86_64 => "x86_64",
    };
    format!(
        "usr/lib/grub/{grub_arch}-efi-signed/grubnet{}.efi.signed",
        platform.efi_arch()
    )
}

/// The ISO's name and URL.
pub(crate) fn iso_name(platform: BootPlatform) -> String {
    format!("ubuntu-24.04.5-live-server-{}.iso", platform.deb_arch())
}

pub(crate) fn iso_url(platform: BootPlatform) -> String {
    format!("https://isos.example/24.04/{}", iso_name(platform))
}

/// The hostkeeper deb's asset name and URL.
pub(crate) fn deb_asset(platform: BootPlatform) -> String {
    format!("hostkeeper_{VERSION}_{}.deb", platform.deb_arch())
}

pub(crate) fn deb_url(platform: BootPlatform) -> String {
    format!("{RELEASE}/{}", deb_asset(platform))
}

impl PlatformFiles {
    /// The files of `platform`, with the ISO built of the others.
    pub(crate) fn of(platform: BootPlatform) -> PlatformFiles {
        let p = platform.as_str();
        let shim = format!("the shim of {p}\n").into_bytes();
        let grub = format!("the network GRUB of {p}\n").into_bytes();
        let shim_deb = deb(
            "data.tar.xz",
            &[
                (
                    "usr/lib/shim/shim.efi.dualsigned",
                    b"another shim",
                    Kind::File,
                ),
                (&shim_path(platform), &shim, Kind::File),
            ],
        );
        let grub_deb = deb("data.tar.zst", &[(&grub_path(platform), &grub, Kind::File)]);
        let mut files = PlatformFiles {
            kernel: format!("the kernel of {p}\n").into_bytes(),
            initrd: format!("the initrd of {p}, somewhat longer\n").into_bytes(),
            shim,
            grub,
            shim_deb,
            grub_deb,
            iso: Vec::new(),
            hostkeeper_deb: format!("the hostkeeper deb of {p}\n").into_bytes(),
        };
        files.iso = files.image().build();
        files
    }

    /// The image of the ISO.
    pub(crate) fn image(&self) -> Image {
        Image::of(&[
            ("casper/vmlinuz", &self.kernel),
            ("casper/initrd", &self.initrd),
            ("md5sum.txt", b"sums"),
            (&shim_deb_path(platform_of(self)), &self.shim_deb),
            (&grub_deb_path(platform_of(self)), &self.grub_deb),
        ])
    }
}

/// The platform whose files these are, read back from the kernel's text.
fn platform_of(files: &PlatformFiles) -> BootPlatform {
    match String::from_utf8_lossy(&files.kernel).contains("arm64") {
        true => BootPlatform::LinuxArm64,
        false => BootPlatform::LinuxX86_64,
    }
}

/// A release, and what the tests change in it.
pub(crate) struct Release {
    pub files: BTreeMap<BootPlatform, PlatformFiles>,
    /// `netboot.json` as a value; [`Release::netboot`] fills it from the files.
    pub netboot: Value,
    /// Who signs `netboot.json`'s bundle.
    pub netboot_signer: Signer,
    /// Who signs each deb's bundle.
    pub deb_signer: Signer,
    /// The SHA-256 each deb's bundle lists, when not the deb's own.
    pub deb_subject: Option<String>,
    /// Routes that replace the release's own, applied last.
    pub overrides: Vec<(String, Serve)>,
}

impl Release {
    pub(crate) fn new() -> Release {
        let files: BTreeMap<BootPlatform, PlatformFiles> = BootPlatform::ALL
            .into_iter()
            .map(|platform| (platform, PlatformFiles::of(platform)))
            .collect();
        let mut release = Release {
            files,
            netboot: Value::Null,
            netboot_signer: Signer::trusted(),
            deb_signer: Signer::trusted(),
            deb_subject: None,
            overrides: Vec::new(),
        };
        release.netboot = release.netboot();
        release
    }

    /// `netboot.json` of the files, as `cargo xtask release netboot` writes it.
    pub(crate) fn netboot(&self) -> Value {
        let mut platforms = serde_json::Map::new();
        for (platform, files) in &self.files {
            let p = platform.as_str();
            let efi = platform.efi_arch();
            let pin = |name: String, bytes: &[u8]| json!({"name": name, "bytes": bytes.len(), "sha256": sha256_hex(bytes)});
            let mut iso = pin(iso_name(*platform), &files.iso);
            iso["url"] = json!(iso_url(*platform));
            iso["sums"] = json!("https://isos.example/24.04/SHA256SUMS");
            let mut shim = pin(format!("hostkeeper/shim{efi}.efi"), &files.shim);
            shim["iso_path"] = json!(shim_deb_path(*platform));
            shim["deb_path"] = json!(shim_path(*platform));
            let mut grub = pin(format!("hostkeeper/grub{efi}.efi"), &files.grub);
            grub["iso_path"] = json!(grub_deb_path(*platform));
            grub["deb_path"] = json!(grub_path(*platform));
            let mut kernel = pin(format!("vmlinuz-{p}"), &files.kernel);
            kernel["iso_path"] = json!("casper/vmlinuz");
            let mut initrd = pin(format!("initrd-{p}"), &files.initrd);
            initrd["iso_path"] = json!("casper/initrd");
            platforms.insert(
                p.to_owned(),
                json!({"iso": iso, "shim": shim, "grub": grub, "kernel": kernel, "initrd": initrd}),
            );
        }
        json!({"platforms": platforms, "ubuntu": "24.04.5", "version": VERSION})
    }

    /// The text of `netboot.json`.
    pub(crate) fn netboot_text(&self) -> Vec<u8> {
        self.netboot.to_string().into_bytes()
    }

    /// The network that serves this release.
    pub(crate) fn routes(&self) -> FakeTransport {
        let netboot = self.netboot_text();
        let mut transport = FakeTransport::new()
            .route(
                &format!("{RELEASE}/netboot.json"),
                Serve::Body(netboot.clone()),
            )
            .route(
                &format!("{RELEASE}/netboot.json.sigstore.json"),
                Serve::Body(bundle(&self.netboot_signer, &sha256_hex(&netboot))),
            );
        for (platform, files) in &self.files {
            let subject = self
                .deb_subject
                .clone()
                .unwrap_or_else(|| sha256_hex(&files.hostkeeper_deb));
            transport = transport
                .route(&iso_url(*platform), Serve::Body(files.iso.clone()))
                .route(
                    &deb_url(*platform),
                    Serve::Body(files.hostkeeper_deb.clone()),
                )
                .route(
                    &format!("{}.sigstore.json", deb_url(*platform)),
                    Serve::Body(bundle(&self.deb_signer, &subject)),
                );
        }
        for (url, serve) in &self.overrides {
            transport = transport.route(url, serve.clone());
        }
        transport
    }

    /// Runs a fetch of `platforms` of this release into a new directory of a new scratch
    /// directory, with the daemon's verifier over a fake `gh` behaving as `mode`.
    pub(crate) fn run(&self, name: &str, platforms: &[BootPlatform], mode: GhMode) -> Run {
        let scratch = scratch(name);
        let dir = scratch.path().join("boot");
        let transport = self.routes();
        let clock = Rc::new(FakeClock::at(100));
        let runs = Rc::new(Cell::new(0));
        let gh = FakeGh {
            mode,
            runs: Rc::clone(&runs),
            clock: Rc::clone(&clock),
        };
        let verifier = verifier(gh, &dir.join(VERIFY_DIR).join(GH_HOME));
        let ports = Ports {
            transport: &transport,
            verifier: &verifier,
            clock: &*clock,
        };
        let result = fetch(&ports, &request(&dir, platforms));
        Run {
            result,
            asked: transport.asked(),
            gh_runs: runs.get(),
            dir,
            _scratch: scratch,
        }
    }
}

/// The request of a fetch of `platforms` into `dir`.
pub(crate) fn request(dir: &std::path::Path, platforms: &[BootPlatform]) -> Request {
    Request {
        release: RELEASE.to_owned(),
        dir: dir.to_owned(),
        platforms: platforms.iter().copied().collect::<BTreeSet<_>>(),
        signers: signers(),
        attestation_timeout_secs: 60,
        max_deb_bytes: MAX_DEB_BYTES,
        timeouts: timeouts(),
    }
}

/// What a run did.
pub(crate) struct Run {
    pub result: Result<Seal, FetchError>,
    /// The URLs asked for, in order.
    pub asked: Vec<String>,
    pub gh_runs: u32,
    /// The directory the fetch was asked to make.
    pub dir: PathBuf,
    pub _scratch: Scratch,
}

impl Run {
    /// The refusal's text, after checking that the directory is gone.
    pub(crate) fn refused(&self) -> String {
        assert!(!self.dir.exists(), "a refused fetch keeps nothing");
        self.result.as_ref().expect_err("refused").to_string()
    }
}
