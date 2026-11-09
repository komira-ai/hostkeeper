//! The fixture repository, built at test time with the runner's own tools: `dpkg-deb` for the
//! packages, `gpg` for the keys and the signature, and the `Packages` index and the `Release`
//! file written here. The suite is `hk`, its one component `main`, its packages those of
//! [`PACKAGE`] at the versions of [`DEFAULT`].

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// The package, and the service: a name no runner image has.
pub const PACKAGE: &str = "hkaptfix";
/// The suite of the repository.
pub const SUITE: &str = "hk";

/// What a version of the package is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// The package a service ships: a program, a run unit, a timer that is not a run unit, files
    /// of its own under `/usr/share` and `/etc`, no maintainer script.
    Plain,
    /// `Plain` and a drop-in directory of the service's unit.
    DropIn,
    /// `Plain` and a unit of another name.
    OtherUnit,
    /// `Plain` and a `postinst`.
    Script,
    /// `Plain` and a dependency no repository has.
    UnmetDependency,
    /// `Plain` and one more file: other bytes under the version of the plain build.
    Reuploaded,
}

/// One version of the package a repository lists.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub version: &'static str,
    pub flavour: Flavour,
}

/// The versions a repository lists: `0.1-dropin` to `0.9-dep` are what `stage` refuses, `1.0-1`
/// and `2.0-1` the two versions a host moves between, and `3.0-1` the newer one an upgrade
/// would take.
pub const DEFAULT: [Spec; 7] = [
    Spec {
        version: "0.1-dropin",
        flavour: Flavour::DropIn,
    },
    Spec {
        version: "0.2-script",
        flavour: Flavour::Script,
    },
    Spec {
        version: "0.3-unit",
        flavour: Flavour::OtherUnit,
    },
    Spec {
        version: "0.9-dep",
        flavour: Flavour::UnmetDependency,
    },
    Spec {
        version: "1.0-1",
        flavour: Flavour::Plain,
    },
    Spec {
        version: "2.0-1",
        flavour: Flavour::Plain,
    },
    Spec {
        version: "3.0-1",
        flavour: Flavour::Plain,
    },
];

/// A package of a repository, as built.
#[derive(Clone, Debug)]
pub struct Built {
    pub version: &'static str,
    /// The SHA-256 of the file, lower-case hex: what an order pins.
    pub sha256: String,
}

/// A signing key in a home of its own. Its agent is stopped when it is dropped.
pub struct Key {
    home: PathBuf,
    fingerprint: String,
}

/// Runs `command`, which must exit 0, and answers its standard output.
pub fn run(command: &mut Command) -> Vec<u8> {
    let out = command
        .output()
        .unwrap_or_else(|e| panic!("run {command:?}: {e}"));
    assert!(
        out.status.success(),
        "{command:?} failed: {:?}\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

impl Key {
    /// A new key with no passphrase and no expiry, its home `home` (made here, mode 0700).
    pub fn new(home: &Path, name: &str) -> Key {
        fs::create_dir(home).expect("make the key's home");
        fs::set_permissions(home, fs::Permissions::from_mode(0o700)).expect("restrict the home");
        let gpg = |args: &[&str]| {
            let mut command = Command::new("gpg");
            command
                .args(["--batch", "--no-tty", "--homedir"])
                .arg(home)
                .args(args);
            command
        };
        run(&mut gpg(&[
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-gen-key",
            &format!("{name} <{name}@example.invalid>"),
            "default",
            "default",
            "never",
        ]));
        let listing = run(&mut gpg(&["--with-colons", "--list-keys"]));
        let fingerprint = String::from_utf8_lossy(&listing)
            .lines()
            .find_map(|line| line.strip_prefix("fpr:"))
            .and_then(|rest| rest.split(':').nth(8))
            .expect("the key's fingerprint")
            .to_owned();
        Key {
            home: home.to_path_buf(),
            fingerprint,
        }
    }

    /// The public key, binary, into `path` (mode 0644): a keyring `Signed-By` names.
    pub fn export(&self, path: &Path) {
        let key = run(Command::new("gpg")
            .args(["--batch", "--no-tty", "--homedir"])
            .arg(&self.home)
            .args(["--export", &self.fingerprint]));
        fs::write(path, key).expect("write the keyring");
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("keyring mode");
    }

    /// `release` clear-signed into `into`, the suite's `InRelease`.
    fn clearsign(&self, release: &Path, into: &Path) {
        run(Command::new("gpg")
            .args(["--batch", "--no-tty", "--homedir"])
            .arg(&self.home)
            .args(["--pinentry-mode", "loopback", "--passphrase", ""])
            .args(["--local-user", &self.fingerprint, "--yes", "--clearsign"])
            .arg("--output")
            .arg(into)
            .arg(release));
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(&self.home)
            .args(["--kill", "gpg-agent"])
            .output();
    }
}

/// How a suite is signed and dated.
#[derive(Clone, Copy)]
pub enum Signing<'a> {
    /// `InRelease`, signed by the key; valid for 30 days.
    Signed(&'a Key),
    /// `InRelease`, signed by the key; its `Valid-Until` passed a day ago.
    Expired(&'a Key),
    /// A `Release` and no signature.
    Unsigned,
}

/// The host's dpkg architecture.
pub fn architecture() -> String {
    let out = run(Command::new("dpkg").arg("--print-architecture"));
    String::from_utf8_lossy(&out).trim().to_owned()
}

/// The SHA-256 of `bytes`, lower-case hex.
fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `at` seconds after the epoch, as a `Release` file spells a date.
fn release_date(at: u64) -> String {
    let out = run(Command::new("date")
        .args(["-u", "-d"])
        .arg(format!("@{at}"))
        .arg("+%a, %d %b %Y %H:%M:%S UTC"));
    String::from_utf8_lossy(&out).trim().to_owned()
}

/// Writes `text` at `path` below `root` with `mode`, making its directories.
fn put(root: &Path, path: &str, text: &str, mode: u32) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().expect("a parent")).expect("make the directory");
    fs::write(&file, text).expect("write the file");
    fs::set_permissions(&file, fs::Permissions::from_mode(mode)).expect("set the mode");
}

/// The unit the package runs.
const SERVICE_UNIT: &str = "[Unit]\nDescription=hkaptfix fixture service\n\n[Service]\n\
    ExecStart=/usr/bin/hkaptfix serve\n\n[Install]\nWantedBy=multi-user.target\n";

/// The unit the package ships that nothing runs: a timer with no `[Install]`.
const TIMER_UNIT: &str =
    "[Unit]\nDescription=hkaptfix fixture timer\n\n[Timer]\nOnCalendar=daily\n";

/// The tree of the package at `version` in `src`, with its `DEBIAN/` directory.
fn write_tree(src: &Path, spec: &Spec, architecture: &str) {
    let version = spec.version;
    put(
        src,
        "usr/bin/hkaptfix",
        &format!("#!/bin/sh\n[ \"$1\" = serve ] && exec sleep infinity\necho hkaptfix {version}\n"),
        0o755,
    );
    put(
        src,
        "usr/lib/systemd/system/hkaptfix.service",
        SERVICE_UNIT,
        0o644,
    );
    put(
        src,
        "usr/lib/systemd/system/hkaptfix-extra.timer",
        TIMER_UNIT,
        0o644,
    );
    put(src, "usr/share/hkaptfix/version", version, 0o644);
    put(
        src,
        "usr/share/doc/hkaptfix/copyright",
        "public domain\n",
        0o644,
    );
    put(src, "etc/hkaptfix/hkaptfix.conf", "mode = fixture\n", 0o644);
    let mut control = format!(
        "Package: {PACKAGE}\nVersion: {version}\nArchitecture: {architecture}\n\
         Maintainer: hostkeeper tests <tests@example.invalid>\n\
         Description: the fixture package of the apt installer's device test\n"
    );
    match spec.flavour {
        Flavour::Plain => {}
        Flavour::DropIn => put(
            src,
            "usr/lib/systemd/system/hkaptfix.service.d/override.conf",
            "[Service]\nNice=5\n",
            0o644,
        ),
        Flavour::OtherUnit => put(
            src,
            "usr/lib/systemd/system/other.service",
            SERVICE_UNIT,
            0o644,
        ),
        Flavour::Script => put(src, "DEBIAN/postinst", "#!/bin/sh\nexit 0\n", 0o755),
        Flavour::UnmetDependency => control.push_str("Depends: hkaptfix-no-such-package\n"),
        Flavour::Reuploaded => put(src, "usr/share/hkaptfix/reuploaded", "other bytes\n", 0o644),
    }
    put(src, "DEBIAN/control", &control, 0o644);
    put(
        src,
        "DEBIAN/conffiles",
        "/etc/hkaptfix/hkaptfix.conf\n",
        0o644,
    );
    for dir in ["DEBIAN", ""] {
        fs::set_permissions(src.join(dir), fs::Permissions::from_mode(0o755))
            .expect("package directory mode");
    }
}

/// Builds `specs` into the repository `repo` (made when missing; what it holds is replaced), its
/// packages in `repo/pool/main/` and its suite signed as `signing` says, and answers each
/// package as built. `work` is a directory for the package trees.
pub fn publish(repo: &Path, work: &Path, specs: &[Spec], signing: Signing<'_>) -> Vec<Built> {
    let architecture = architecture();
    let pool = repo.join("pool/main");
    let binary = format!("main/binary-{architecture}");
    let index_dir = repo.join("dists").join(SUITE).join(&binary);
    for dir in [&pool, &index_dir] {
        fs::create_dir_all(dir).expect("make the repository's directories");
    }
    let mut built = Vec::new();
    let mut packages = String::new();
    for spec in specs {
        let src = work.join(format!("{PACKAGE}-{}", spec.version));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).expect("make a package tree");
        write_tree(&src, spec, &architecture);
        let file = format!("{PACKAGE}_{}_{architecture}.deb", spec.version);
        run(Command::new("dpkg-deb")
            .args(["--root-owner-group", "-Zxz", "--build"])
            .arg(&src)
            .arg(pool.join(&file)));
        let bytes = fs::read(pool.join(&file)).expect("read the package");
        let digest = sha256(&bytes);
        let control = fs::read_to_string(src.join("DEBIAN/control")).expect("the control file");
        packages.push_str(&format!(
            "{control}Filename: pool/main/{file}\nSize: {}\nSHA256: {digest}\n\n",
            bytes.len()
        ));
        built.push(Built {
            version: spec.version,
            sha256: digest,
        });
    }
    fs::write(index_dir.join("Packages"), &packages).expect("write the index");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs();
    let (date, valid_until) = match signing {
        Signing::Expired(_) => (now - 2 * 86_400, now - 86_400),
        _ => (now, now + 30 * 86_400),
    };
    let release = format!(
        "Origin: hkaptfix\nLabel: hkaptfix\nSuite: {SUITE}\nCodename: {SUITE}\nDate: {}\n\
         Valid-Until: {}\nArchitectures: {architecture}\nComponents: main\n\
         Description: the fixture repository of the apt installer's device test\nSHA256:\n \
         {} {} {binary}/Packages\n",
        release_date(date),
        release_date(valid_until),
        sha256(packages.as_bytes()),
        packages.len(),
    );
    let suite = repo.join("dists").join(SUITE);
    let release_file = suite.join("Release");
    fs::write(&release_file, release).expect("write the Release file");
    let in_release = suite.join("InRelease");
    let _ = fs::remove_file(&in_release);
    match signing {
        Signing::Signed(key) | Signing::Expired(key) => key.clearsign(&release_file, &in_release),
        Signing::Unsigned => {}
    }
    built
}
