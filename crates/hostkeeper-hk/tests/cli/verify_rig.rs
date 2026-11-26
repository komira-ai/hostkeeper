//! What the tests of `hk xcode verify` share: stand-ins for the Mac's programs (the test binary `hk-fake-mac-tool`
//! under each name, which records its arguments, the bytes of a file it is given and its
//! environment), a clock that moves 1.5 seconds a reading, and a root certificate of the tests'
//! own making. Nothing here reaches a network but a loopback server of port 0.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_hk::xcode_push::Mac;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::rig::{Opts, Ran, Rig, STAMP, args};

/// The bytes of the archive of the tests: the four bytes of an xip, then filler.
pub const ARCHIVE: &[u8] = b"xar!, the bytes of an Xcode archive as Apple ships them";

/// What `pkgutil --check-signature` says of an archive Apple signed.
pub const SIGNED_BY_APPLE: &str = "\
Package \"Xcode_27.2.xip\":
   Status: signed Apple Software
   Notarization: trusted by the Apple notary service
   Signed with a trusted timestamp on: 2026-11-18 17:00:00 +0000
   Certificate Chain:
    1. Software Signing
       Expires: 2030-10-24 17:39:41 +0000
       SHA256 Fingerprint:
           D8 4D B9 6A F8 C2 E6 0A C4 C8 51 A2 1E C4 60 F6 F8 4E 02 35 BE B1
           7D 24 A7 87 12 B9 B0 21 ED 57
       ------------------------------------------------------------------------
    2. Apple Code Signing Certification Authority
       Expires: 2026-10-24 17:39:41 +0000
       SHA256 Fingerprint:
           5B DA B1 28 8F C1 68 92 FE F5 0C 65 8D B5 4F 1E 2E 19 CF 8F 71 CC
           55 F7 7D E2 B9 5E 05 1E 25 62
       ------------------------------------------------------------------------
    3. Apple Root CA
       Expires: 2035-02-09 21:40:36 +0000
       SHA256 Fingerprint:
           B0 B1 73 0E CB C7 FF 45 05 14 2C 49 F1 29 5E 6E DA 6B CA ED 7E 2C
           68 C5 BE 91 B5 A1 10 01 F0 24
";

/// What `df -Pk` prints for a volume with `available` KiB free.
pub fn df(available: u64) -> String {
    format!(
        "Filesystem    1024-blocks      Used Available Capacity  Mounted on\n\
         /dev/disk3s5    971350180 400000000 {available}    30%    /System/Volumes/Data\n"
    )
}

thread_local! {
    /// The reading of the fake clock, in milliseconds; each test runs on a thread of its own.
    static NOW: Cell<u64> = const { Cell::new(0) };
}

/// A clock that moves 1.5 seconds with every reading.
pub fn clock() -> Duration {
    NOW.with(|now| {
        let reading = now.get();
        now.set(reading + 1500);
        Duration::from_millis(reading)
    })
}

/// The lower-case hex SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The upper-case hex SHA-256 of `bytes`: a certificate's fingerprint.
pub fn fingerprint(bytes: &[u8]) -> String {
    sha256(bytes).to_ascii_uppercase()
}

/// A self-signed certificate named `Apple Root CA`: its DER and its PEM.
pub fn root_certificate() -> (Vec<u8>, String) {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Apple Root CA");
    let key = rcgen::KeyPair::generate().expect("a key");
    let cert = params.self_signed(&key).expect("a certificate");
    (cert.der().to_vec(), cert.pem())
}

/// A test's directory with the stand-ins in `bin`, set up for an archive Apple signed whose app is
/// Xcode 27.2 (27B52), on a Mac with room, and Apple's root certificate in `apple-root.cer`.
pub struct Proof {
    pub rig: Rig,
    pub mac: Mac,
    pub bin: PathBuf,
    /// The fingerprint of `apple-root.cer`.
    pub root: String,
}

impl Proof {
    pub fn new(name: &str) -> Proof {
        let rig = Rig::new(&format!("xcode-verify-{name}"));
        let bin = rig.path("bin");
        std::fs::create_dir(&bin).expect("the bin directory");
        let fake = Path::new(env!("CARGO_BIN_EXE_hk-fake-mac-tool"));
        for tool in [
            "pkgutil", "xip", "plutil", "codesign", "spctl", "xar", "sw_vers", "uname", "cp", "df",
            "aa",
        ] {
            // A link, not a copy: a copy is a file open for writing, which a process another test
            // forks meanwhile could hold when this one runs it.
            std::fs::hard_link(fake, bin.join(tool)).expect("a stand-in");
        }
        let (der, _) = root_certificate();
        let root = fingerprint(&der);
        rig.put("apple-root.cer", &der, 0o644);
        std::fs::create_dir(rig.path("clt")).expect("a Command Line Tools directory");
        std::fs::create_dir(rig.path("downloads")).expect("a downloads directory");
        let mac = Mac {
            os: "macos",
            xcodes: bin.join("xcodes"),
            pkgutil: bin.join("pkgutil"),
            xip: bin.join("xip"),
            plutil: bin.join("plutil"),
            codesign: bin.join("codesign"),
            spctl: bin.join("spctl"),
            xar: bin.join("xar"),
            sw_vers: bin.join("sw_vers"),
            uname: bin.join("uname"),
            cp: bin.join("cp"),
            df: bin.join("df"),
            archiveutil: bin.join("archiveutil"),
            aa: bin.join("aa"),
            aea: bin.join("aea"),
            command_line_tools: rig.path("clt"),
            apple_root_url: "https://127.0.0.1:1/AppleIncRootCertificate.cer".to_owned(),
            apple_root_sha256: root.clone(),
            clock,
        };
        let proof = Proof {
            rig,
            mac,
            bin,
            root,
        };
        proof.tool("pkgutil.out", SIGNED_BY_APPLE.as_bytes());
        // A damaged archive and a file that is no archive: pkgutil and xip refuse both.
        for target in ["damaged.xip", "non-archive.xip"] {
            proof.tool(&format!("pkgutil.{target}.exit"), b"1");
            proof.tool(
                &format!("pkgutil.{target}.out"),
                b"Error: the package is damaged\n",
            );
            proof.tool(&format!("xip.{target}.exit"), b"1");
            proof.tool(&format!("xip.{target}.err"), b"xip: not an archive\n");
        }
        proof.tool("xip.version", b"27.2\n");
        proof.tool("xip.product-build", b"27B52\n");
        proof.tool(
            "sw_vers.out",
            b"ProductName:\t\tmacOS\nProductVersion:\t\t26.1\nBuildVersion:\t\t25B78\n",
        );
        proof.tool("uname.out", b"Darwin arm64\n");
        proof.tool(
            "xip.usage",
            b"Usage: xip --expand <file>\n  Expands an archive.\n",
        );
        proof.tool("xip.list", b"Metadata\nContent\n");
        proof.tool("xar.list", b"Metadata\nContent\n");
        proof.tool(
            "xar.toc",
            b"<xar><toc><file><name>Metadata</name></file></toc></xar>",
        );
        proof.tool(
            "xar.metadata",
            b"<plist><dict><key>Name</key><string>Xcode</string></dict></plist>",
        );
        proof.tool("spctl.out", b"Xcode.app: accepted\n");
        // Two expansions of damaged files, then the expansion of the archive: before and after.
        // A later run, of a test that runs more than one, has no number of its own.
        proof.tool("df.out", df(900_000).as_bytes());
        proof.tool("df.1.out", df(900_000).as_bytes());
        proof.tool("df.2.out", df(900_000).as_bytes());
        proof.tool("df.3.out", df(800_000).as_bytes());
        proof.tool("df.4.out", df(700_000).as_bytes());
        proof
    }

    /// Writes `name` in `bin`, beside the stand-ins.
    pub fn tool(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.bin.join(name), bytes).expect("written");
    }

    /// Removes `name` from `bin`.
    pub fn untool(&self, name: &str) {
        std::fs::remove_file(self.bin.join(name)).expect("removed");
    }

    /// What the stand-in wrote to `name` in `bin`; empty when it wrote nothing.
    pub fn recorded(&self, name: &str) -> String {
        std::fs::read_to_string(self.bin.join(name)).unwrap_or_default()
    }

    /// The archive of the tests with `bytes`: its path.
    pub fn archive(&self, bytes: &[u8]) -> String {
        self.rig.put("downloads/Xcode_27.2.xip", bytes, 0o644)
    }

    /// `hk xcode verify <archive> --apple-root <the certificate>` on the stand-ins.
    pub fn verify(&self, archive: &str) -> Ran {
        let root = self.rig.path("apple-root.cer").display().to_string();
        self.run(&["xcode", "verify", archive, "--apple-root", &root])
    }

    /// `hk` with `words` on the stand-ins.
    pub fn run(&self, words: &[&str]) -> Ran {
        self.run_with(Opts::real(), words)
    }

    /// [`Proof::run`] with `opts`.
    pub fn run_with<'a>(&'a self, opts: Opts<'a>, words: &[&str]) -> Ran {
        self.rig.run(opts.mac(&self.mac), &args(words))
    }

    /// The record `ran` printed.
    pub fn printed(ran: &Ran) -> Value {
        serde_json::from_str(&ran.stdout).expect("the record is JSON")
    }

    /// The path of the record of the archive of `bytes`.
    pub fn record_path(&self, bytes: &[u8]) -> PathBuf {
        self.rig
            .path(&format!("home/.hostkeeper/xcode/{}.json", sha256(bytes)))
    }

    /// The verdict of a run of the archive of `bytes`, which exited 1 if it was not a pass.
    pub fn verdict(&self, bytes: &[u8]) -> String {
        let ran = self.verify(&self.archive(bytes));
        let record = Proof::printed(&ran);
        let verdict = record["verdict"].as_str().expect("a verdict").to_owned();
        assert_eq!(ran.code, u8::from(verdict != "pass"), "{ran:?}");
        verdict
    }

    /// The entries beside the archive: what a run left behind.
    pub fn beside(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.rig.path("downloads"))
            .expect("the directory")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

/// The record of a run over the default stand-ins, as `hk xcode verify` must print it.
pub fn expected(proof: &Proof) -> Value {
    let bin = proof.bin.display();
    let required =
        |name: &str| json!({"name": name, "path": format!("{bin}/{name}"), "file": true});
    let wrote = |exit: i32, stdout: &str, stderr: &str| json!({"exit": exit, "stdout": stdout, "stderr": stderr});
    let damage = json!({
        "pkgutil": wrote(1, "Error: the package is damaged\n", ""),
        "xip": wrote(1, "", "xip: not an archive\n"),
    });
    json!({
        "record": "hk-xcode-verify-v1",
        "hk": STAMP,
        "macos": {
            "sw_vers": wrote(
                0,
                "ProductName:\t\tmacOS\nProductVersion:\t\t26.1\nBuildVersion:\t\t25B78\n",
                ""
            ),
            "uname": wrote(0, "Darwin arm64\n", ""),
        },
        "programs": {
            "required": [
                required("pkgutil"), required("xip"), required("plutil"), required("codesign"),
                required("spctl"), required("xar"), required("sw_vers"), required("uname"),
                required("cp"), required("df"),
            ],
            "optional": [
                {"name": "archiveutil", "path": format!("{bin}/archiveutil"), "file": false},
                {"name": "aa", "path": format!("{bin}/aa"), "file": true},
                {"name": "aea", "path": format!("{bin}/aea"), "file": false},
            ],
            "xcodes": {
                "exit": null,
                "stdout": "",
                "stderr": format!("cannot run {bin}/xcodes: No such file or directory (os error 2)"),
            },
            "xip_help": "Usage: xip --expand <file>\n  Expands an archive.",
        },
        "archive": {
            "name": "Xcode_27.2.xip",
            "size": ARCHIVE.len(),
            "sha256": sha256(ARCHIVE),
            "kind": "xar!",
        },
        "signature": {
            "exit": 0,
            "stdout": SIGNED_BY_APPLE,
            "stderr": "",
            "status": "signed Apple Software",
            "chain": [
                {"name": "Software Signing",
                 "sha256": "D84DB96AF8C2E60AC4C851A21EC460F6F84E0235BEB17D24A78712B9B021ED57"},
                {"name": "Apple Code Signing Certification Authority",
                 "sha256": "5BDAB1288FC16892FEF50C658DB54F1E2E19CF8F71CC55F77DE2B95E051E2562"},
                {"name": "Apple Root CA",
                 "sha256": "B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024"},
            ],
            "apple_root": {
                "source": proof.rig.path("apple-root.cer").display().to_string(),
                "sha256": proof.root,
                "subject": "CN=Apple Root CA",
                "constant": proof.root,
                "equal": true,
                "error": null,
            },
        },
        "tamper": {
            "damaged_clone": "run",
            "clone": wrote(0, "", ""),
            "damaged": damage,
            "non_archive": damage,
            "command_line_tools": null,
        },
        "toc": {
            "xip_list": wrote(0, "Metadata\nContent\n", ""),
            "xar_list": wrote(0, "Metadata\nContent\n", ""),
            "dump_toc": wrote(0, "", ""),
            "toc": "<xar><toc><file><name>Metadata</name></file></toc></xar>",
            "metadata": {
                "extract": wrote(0, "", ""),
                "kind": "file",
                "text": "<plist><dict><key>Name</key><string>Xcode</string></dict></plist>",
            },
            "names_version": false,
        },
        "expansion": {
            "free_before": 800_000 * 1024,
            "free_after": 700_000 * 1024,
            "exit": 0,
            "output": "",
            "seconds": 1.5,
            "apps": ["Xcode.app"],
            "contents": ["Info.plist", "version.plist"],
            "version_plist": true,
            "plutil": {
                "version": wrote(0, "27.2\n", ""),
                "build": wrote(0, "27B52\n", ""),
            },
            "version": "27.2",
            "build": "27B52",
            "codesign": wrote(0, "", ""),
            "spctl": wrote(0, "Xcode.app: accepted\n", ""),
        },
        "verdict": "pass",
    })
}
