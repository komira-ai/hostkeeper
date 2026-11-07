//! `mdm-signing-test build`: the kit, on a Mac other than the one under test.
//!
//! The farm identity is a test CA the build makes, so no secret is in the repository and none
//! outlives the build. In a scratch directory `work/` inside the kit, through the [`Exec`] port:
//!
//! 1. `/usr/bin/openssl` (the LibreSSL macOS ships, whose PKCS#12 files `security import`
//!    reads) makes two CAs, `A` and `B`, each a self-signed RSA 2048 root (`CA:TRUE`,
//!    `keyCertSign`), and under each an installer identity (`CA:FALSE`, `digitalSignature`,
//!    extended key usage `codeSigning`), each from its own config file ([`root_config`],
//!    [`leaf_config`]); both identities are exported as PKCS#12.
//! 2. `pkgbuild` builds each pkg's component from a payload of one marker file, installed in
//!    [`INSTALL_LOCATION`] under the pkg's own identifier; `productbuild --package` wraps pkgs
//!    2 to 4 as distribution pkgs.
//! 3. A keychain of its own, `work/signing-test.keychain-db`, holds the two identities for
//!    `productsign`, which signs pkg 3 with `A`'s identity and pkg 4 with `B`'s, with no
//!    timestamp. The keychain is put first in the user's search list for the signing and the
//!    list is put back, and the keychain deleted, whether the signing worked or not. Its
//!    password and the PKCS#12 files' are 16 random bytes from `openssl rand`, new each build;
//!    they are on the command lines of `openssl` and `security` while those run, so the build
//!    Mac is one no other person uses during the build.
//! 4. `pkgutil --check-signature` reads each signed pkg; one it does not read as signed is
//!    refused. What it prints goes into `kit.toml`.
//! 5. The profile that installs `A`'s root ([`super::commands::root_profile`]) is written to the
//!    kit, and each pkg to `pkg/<sha256>.pkg`, with `kit.toml` describing them.
//!
//! Then `work/` is deleted, with every key and `B`'s root: `B`'s root is installed nowhere,
//! which is what pkg 4 tests. It is deleted when the build fails too.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::commands::{ProfileIdentifier, root_profile};
use super::{
    Error, Format, INSTALL_LOCATION, KIT_FILE, Kit, KitPkg, PKG_DIR, PKG_VERSION, PKGS,
    PROFILE_FILE, PkgSpec, RootProfile, Signer, file_sha256,
};
use crate::release::ports::{Exec, PortError, Ran};

/// The `openssl` the build runs: macOS's own.
pub(crate) const OPENSSL: &str = "/usr/bin/openssl";

/// The `productsign` the keychain grants its keys to.
const PRODUCTSIGN_PATH: &str = "/usr/bin/productsign";

/// How long the test certificates are valid, in days.
const DAYS: &str = "90";

/// What `pkgutil --check-signature` prints for a signed pkg, trusted or not.
const SIGNED: &str = "Status: signed";

/// The scratch directory of the build, inside the kit.
const WORK: &str = "work";

/// One of the two test CAs.
struct Ca {
    /// `a` or `b`, in file names.
    tag: &'static str,
    root_cn: &'static str,
    leaf_cn: &'static str,
}

/// `A`, whose root the profile installs, and `B`, whose root is installed nowhere.
const CAS: [Ca; 2] = [
    Ca {
        tag: "a",
        root_cn: "hostkeeper pkg signing test root A",
        leaf_cn: "hostkeeper pkg signing test installer A",
    },
    Ca {
        tag: "b",
        root_cn: "hostkeeper pkg signing test root B",
        leaf_cn: "hostkeeper pkg signing test installer B",
    },
];

/// The config `openssl req -x509` makes a root with.
pub(crate) fn root_config(cn: &str) -> String {
    format!(
        "[ req ]\ndistinguished_name = dn\nprompt = no\n\n[ dn ]\nCN = {cn}\n\n[ root_ext ]\n\
         basicConstraints = critical, CA:TRUE\nkeyUsage = critical, keyCertSign, cRLSign\n\
         subjectKeyIdentifier = hash\n"
    )
}

/// The config `openssl req` makes an identity's request with, and `openssl x509` its
/// certificate's extensions.
pub(crate) fn leaf_config(cn: &str) -> String {
    format!(
        "[ req ]\ndistinguished_name = dn\nprompt = no\n\n[ dn ]\nCN = {cn}\n\n[ leaf_ext ]\n\
         basicConstraints = critical, CA:FALSE\nkeyUsage = critical, digitalSignature\n\
         extendedKeyUsage = codeSigning\nsubjectKeyIdentifier = hash\n\
         authorityKeyIdentifier = keyid\n"
    )
}

/// The marker file each pkg installs.
pub(crate) fn marker(spec: &PkgSpec) -> String {
    format!(
        "hostkeeper pkg signing test: pkg {}, {}\n",
        spec.number, spec.what
    )
}

/// `path` as UTF-8, for a command line.
fn utf8(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Refused(format!("{} is not UTF-8", path.display())))
}

/// Runs `program` with `args`; a failure to start or a failed status is an error naming the
/// command line (with the passwords it holds replaced by `<password>`).
fn run(exec: &dyn Exec, program: &str, args: &[String], password: &str) -> Result<Ran, Error> {
    let mut what = format!("{program} {}", args.join(" "));
    if !password.is_empty() {
        what = what.replace(password, "<password>");
    }
    let ran = exec
        .spawn(program, args)
        .map_err(|e| PortError::new(&what, e.to_string()))?;
    if ran.success {
        Ok(ran)
    } else {
        Err(PortError::new(what, String::from_utf8_lossy(&ran.stderr).trim().to_owned()).into())
    }
}

/// The words of a command line.
fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

/// Builds the kit into the new directory `out`; the lines to print.
pub(crate) fn build(
    exec: &dyn Exec,
    out: &Path,
    profile: &ProfileIdentifier,
) -> Result<Vec<String>, Error> {
    if std::fs::symlink_metadata(out).is_ok() {
        return Err(Error::Refused(format!(
            "{} exists: the kit is built into a new directory",
            out.display()
        )));
    }
    let work = out.join(WORK);
    for dir in [out.to_path_buf(), out.join(PKG_DIR), work.clone()] {
        std::fs::create_dir(&dir).map_err(Error::io(format!("create {}", dir.display())))?;
    }
    // The keys in `work/` are not encrypted: only the build's user may read them.
    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700))
        .map_err(Error::io(format!("set the mode of {}", work.display())))?;
    let built = make(exec, out, &work, profile);
    let removed = std::fs::remove_dir_all(&work).map_err(Error::io(format!(
        "delete {} and the keys in it",
        work.display()
    )));
    let lines = built?;
    removed?;
    Ok(lines)
}

/// The files of one CA in `work`.
struct CaFiles {
    root_key: String,
    root_pem: String,
    leaf_key: String,
    leaf_csr: String,
    leaf_pem: String,
    p12: String,
}

impl CaFiles {
    fn of(work: &Path, tag: &str) -> Result<CaFiles, Error> {
        let at = |name: String| utf8(&work.join(name));
        Ok(CaFiles {
            root_key: at(format!("root-{tag}.key"))?,
            root_pem: at(format!("root-{tag}.pem"))?,
            leaf_key: at(format!("installer-{tag}.key"))?,
            leaf_csr: at(format!("installer-{tag}.csr"))?,
            leaf_pem: at(format!("installer-{tag}.pem"))?,
            p12: at(format!("installer-{tag}.p12"))?,
        })
    }
}

/// Writes `text` to `path`.
fn write(path: &Path, text: &[u8]) -> Result<(), Error> {
    std::fs::write(path, text).map_err(Error::io(format!("write {}", path.display())))
}

/// Makes the CA `ca`'s root and identity in `work`.
fn make_ca(exec: &dyn Exec, work: &Path, ca: &Ca) -> Result<CaFiles, Error> {
    let files = CaFiles::of(work, ca.tag)?;
    let root_cnf = work.join(format!("root-{}.cnf", ca.tag));
    let leaf_cnf = work.join(format!("installer-{}.cnf", ca.tag));
    write(&root_cnf, root_config(ca.root_cn).as_bytes())?;
    write(&leaf_cnf, leaf_config(ca.leaf_cn).as_bytes())?;
    let (root_cnf, leaf_cnf) = (utf8(&root_cnf)?, utf8(&leaf_cnf)?);
    let req_root = [
        "req",
        "-x509",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-sha256",
        "-days",
        DAYS,
        "-config",
        &root_cnf,
        "-extensions",
        "root_ext",
        "-keyout",
        &files.root_key,
        "-out",
        &files.root_pem,
    ];
    run(exec, OPENSSL, &words(&req_root), "")?;
    let req_leaf = [
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-sha256",
        "-config",
        &leaf_cnf,
        "-keyout",
        &files.leaf_key,
        "-out",
        &files.leaf_csr,
    ];
    run(exec, OPENSSL, &words(&req_leaf), "")?;
    let sign_leaf = [
        "x509",
        "-req",
        "-sha256",
        "-days",
        DAYS,
        "-in",
        &files.leaf_csr,
        "-CA",
        &files.root_pem,
        "-CAkey",
        &files.root_key,
        "-set_serial",
        "2",
        "-extfile",
        &leaf_cnf,
        "-extensions",
        "leaf_ext",
        "-out",
        &files.leaf_pem,
    ];
    run(exec, OPENSSL, &words(&sign_leaf), "")?;
    Ok(files)
}

/// The password of the build's keychain and PKCS#12 files: 16 random bytes from
/// `openssl rand`, in hex, new each build and written to no file.
fn password(exec: &dyn Exec) -> Result<String, Error> {
    let ran = run(exec, OPENSSL, &words(&["rand", "-hex", "16"]), "")?;
    let text = String::from_utf8_lossy(&ran.stdout).trim().to_owned();
    let hex = text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if hex {
        Ok(text)
    } else {
        Err(Error::Refused(format!(
            "{OPENSSL} rand -hex 16 printed {} bytes that are not 32 hex digits",
            text.len()
        )))
    }
}

/// The keychains of the user's search list, as `security list-keychains` prints them (one
/// quoted path a line).
pub(crate) fn search_list(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| line.trim().trim_matches('"').to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

fn make(
    exec: &dyn Exec,
    out: &Path,
    work: &Path,
    profile: &ProfileIdentifier,
) -> Result<Vec<String>, Error> {
    let password = password(exec)?;
    let a = make_ca(exec, work, &CAS[0])?;
    let b = make_ca(exec, work, &CAS[1])?;
    for (ca, files) in CAS.iter().zip([&a, &b]) {
        let pass = format!("pass:{password}");
        let export = [
            "pkcs12",
            "-export",
            "-inkey",
            &files.leaf_key,
            "-in",
            &files.leaf_pem,
            "-certfile",
            &files.root_pem,
            "-name",
            ca.leaf_cn,
            "-passout",
            &pass,
            "-out",
            &files.p12,
        ];
        run(exec, OPENSSL, &words(&export), &password)?;
    }
    let der = utf8(&work.join("root-a.cer"))?;
    let to_der = ["x509", "-in", &a.root_pem, "-outform", "DER", "-out", &der];
    run(exec, OPENSSL, &words(&to_der), "")?;
    let der = std::fs::read(&der).map_err(Error::io(format!("read {der}")))?;
    let identifier = profile.root_profile();
    let profile_bytes = root_profile(&identifier, &der);
    write(&out.join(PROFILE_FILE), &profile_bytes)?;

    let mut unsigned = Vec::new();
    for spec in &PKGS {
        unsigned.push(package(exec, work, spec)?);
    }
    let keychain = utf8(&work.join("signing-test.keychain-db"))?;
    let signed = sign(exec, work, &keychain, &password, &[&a, &b], &unsigned)?;

    let mut kit = Kit {
        profile_identifier: profile.as_str().to_owned(),
        root_profile: RootProfile {
            identifier,
            sha256: hex::encode(Sha256::digest(&profile_bytes)),
            root_sha256: hex::encode(Sha256::digest(&der)),
        },
        pkgs: Vec::new(),
    };
    let mut lines = vec![format!(
        "{PROFILE_FILE}: the profile {} installs root A (sha256 {})",
        kit.root_profile.identifier, kit.root_profile.root_sha256
    )];
    for (spec, (file, check)) in PKGS.iter().zip(signed) {
        let sha256 = file_sha256(&file)?;
        let dest = out.join(PKG_DIR).join(format!("{sha256}.pkg"));
        std::fs::rename(&file, &dest).map_err(Error::io(format!("move {}", file.display())))?;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644))
            .map_err(Error::io(format!("set the mode of {}", dest.display())))?;
        lines.push(format!(
            "{}: {}: {PKG_DIR}/{sha256}.pkg",
            spec.step.name(),
            spec.what
        ));
        kit.pkgs.push(KitPkg {
            step: spec.step,
            identifier: spec.identifier(),
            version: PKG_VERSION.to_owned(),
            sha256,
            build_check_signature: check,
        });
    }
    let text = toml::to_string(&kit).map_err(|e| Error::Refused(format!("kit.toml: {e}")))?;
    write(&out.join(KIT_FILE), text.as_bytes())?;
    lines.push(format!(
        "{KIT_FILE}: written; every key and root B are deleted with {WORK}/"
    ));
    Ok(lines)
}

/// Builds the component of `spec` and, for a distribution pkg, the product around it; the
/// path of the pkg as built (unsigned).
fn package(exec: &dyn Exec, work: &Path, spec: &PkgSpec) -> Result<PathBuf, Error> {
    let n = spec.number;
    let payload = work.join(format!("payload-{n}"));
    std::fs::create_dir(&payload).map_err(Error::io(format!("create {}", payload.display())))?;
    let file = payload.join(format!("pkg-{n}"));
    write(&file, marker(spec).as_bytes())?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
        .map_err(Error::io(format!("set the mode of {}", file.display())))?;
    let identifier = spec.identifier();
    let component = work.join(format!("component-{n}.pkg"));
    let (payload_s, component_s) = (utf8(&payload)?, utf8(&component)?);
    let pkgbuild = [
        "--root",
        &payload_s,
        "--identifier",
        &identifier,
        "--version",
        PKG_VERSION,
        "--install-location",
        INSTALL_LOCATION,
        "--ownership",
        "recommended",
        &component_s,
    ];
    run(exec, "pkgbuild", &words(&pkgbuild), "")?;
    if spec.format == Format::Component {
        return Ok(component);
    }
    let product = work.join(format!("product-{n}.pkg"));
    let product_s = utf8(&product)?;
    let productbuild = [
        "--package",
        &component_s,
        "--identifier",
        &identifier,
        "--version",
        PKG_VERSION,
        &product_s,
    ];
    run(exec, "productbuild", &words(&productbuild), "")?;
    Ok(product)
}

/// Signs the pkgs of `built` that have a signer, in a keychain of their own; each pkg's final
/// path and what `pkgutil --check-signature` printed of it (empty for an unsigned one).
fn sign(
    exec: &dyn Exec,
    work: &Path,
    keychain: &str,
    password: &str,
    cas: &[&CaFiles; 2],
    built: &[PathBuf],
) -> Result<Vec<(PathBuf, String)>, Error> {
    let listed = run(
        exec,
        "security",
        &words(&["list-keychains", "-d", "user"]),
        "",
    )?;
    let original = search_list(&listed.stdout);
    run(
        exec,
        "security",
        &words(&["create-keychain", "-p", password, keychain]),
        password,
    )?;
    let signed = sign_in(exec, work, keychain, password, cas, built, &original);
    let mut restore = words(&["list-keychains", "-d", "user", "-s"]);
    restore.extend(original);
    let restored = run(exec, "security", &restore, "");
    let deleted = run(exec, "security", &words(&["delete-keychain", keychain]), "");
    let signed = signed?;
    restored?;
    deleted?;
    Ok(signed)
}

/// The signing inside the keychain `keychain`, which [`sign`] made and deletes.
fn sign_in(
    exec: &dyn Exec,
    work: &Path,
    keychain: &str,
    password: &str,
    cas: &[&CaFiles; 2],
    built: &[PathBuf],
    original: &[String],
) -> Result<Vec<(PathBuf, String)>, Error> {
    let mut list = words(&["list-keychains", "-d", "user", "-s", keychain]);
    list.extend(original.iter().cloned());
    run(exec, "security", &list, "")?;
    let unlock = ["unlock-keychain", "-p", password, keychain];
    run(exec, "security", &words(&unlock), password)?;
    run(
        exec,
        "security",
        &words(&["set-keychain-settings", keychain]),
        "",
    )?;
    for files in cas {
        let import = [
            "import",
            &files.p12,
            "-k",
            keychain,
            "-P",
            password,
            "-T",
            PRODUCTSIGN_PATH,
        ];
        run(exec, "security", &words(&import), password)?;
    }
    let partition = [
        "set-key-partition-list",
        "-S",
        "apple-tool:,apple:",
        "-s",
        "-k",
        password,
        keychain,
    ];
    run(exec, "security", &words(&partition), password)?;
    let mut done = Vec::new();
    for (spec, unsigned) in PKGS.iter().zip(built) {
        let ca = match spec.signer {
            Signer::Unsigned => {
                done.push((unsigned.clone(), String::new()));
                continue;
            }
            Signer::RootInProfile => &CAS[0],
            Signer::RootNowhere => &CAS[1],
        };
        let signed = work.join(format!("signed-{}.pkg", spec.number));
        let (unsigned_s, signed_s) = (utf8(unsigned)?, utf8(&signed)?);
        let productsign = [
            "--sign",
            ca.leaf_cn,
            "--keychain",
            keychain,
            "--timestamp=none",
            &unsigned_s,
            &signed_s,
        ];
        run(exec, "productsign", &words(&productsign), "")?;
        let check = words(&["--check-signature", &signed_s]);
        let what = format!("pkgutil {}", check.join(" "));
        let ran = exec
            .spawn("pkgutil", &check)
            .map_err(|e| PortError::new(&what, e.to_string()))?;
        let printed = String::from_utf8_lossy(&ran.stdout).trim().to_owned();
        if !printed.contains(SIGNED) {
            return Err(Error::Refused(format!(
                "{what} does not read pkg {} as signed: {printed}",
                spec.number
            )));
        }
        done.push((signed, printed));
    }
    Ok(done)
}
