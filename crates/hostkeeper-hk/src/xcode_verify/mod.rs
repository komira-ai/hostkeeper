//! `hk xcode verify [--apple-root <file.cer>] <archive.xip>`: every check the Mac proof's
//! operator steps (MP-X1 to MP-X6, `docs/operations/mac-proof.md`) ask for, on the operator's own
//! Mac as the operator's user, and the record of what each found. It reads the archive only, makes
//! nothing outside a scratch directory beside the archive (removed afterwards) and
//! `~/.hostkeeper/xcode/`, and calls no coordinator. That is the run on macOS, where `--mac` is a
//! usage error.
//!
//! Off macOS (`Mac::os` is not `macos`) there are no programs of Apple's to run, and the verb is
//! `hk xcode verify --mac <node-id> <archive.xip>` ([`farm`]): the archive, which the coordinator's
//! Xcode store must hold, is checked on that farm Mac ([`crate::xcode_farm`]), and the record is of
//! the same kind, with the key `farm`. `--apple-root` is a usage error there. The parts below are
//! the checks on macOS.
//!
//! In order, each part ending the run at its first failure with the parts after it `null` in the
//! record:
//!
//! 1. the programs ([`programs`], MP-X1): `sw_vers`, `uname -sm`, whether each program's path is
//!    a file (a missing one fails), `xcodes version`, the first lines of `xip --help`, and which
//!    of `archiveutil`, `aa` and `aea` exist;
//! 2. the archive's kind (MP-X6): its first four bytes must be `xar!`; `AA01` and `AEA1` are the
//!    formats the farm Macs defer;
//! 3. the signature ([`sig`], MP-X2): `pkgutil --check-signature` read and judged by
//!    `xcode_push::signature`, and Apple's published root ([`root`]) whose SHA-256 fingerprint
//!    must be the code's;
//! 4. the damaged archives ([`tamper`], MP-X3): `xip --expand` must refuse a clone with a byte
//!    changed and a 16-byte file that is no archive, where `pkgutil`'s answer is only recorded;
//!    a volume that cannot clone says `not run, the volume cannot clone` and the verdict does not
//!    fail on it;
//! 5. what the archive says unexpanded ([`toc`], MP-X4), for the record only;
//! 6. the expansion ([`expansion`], MP-X5): one app with a version and a build, and `codesign
//!    --verify --deep --strict` passing.
//!
//! Before each expansion the volume must have three times the archive's size free
//! (`free_space_short`, [`free`]).
//!
//! The verdict is `pass`, or `fail: <the first failure>`. The record ([`crate::xcode_record`]) is
//! written to `~/.hostkeeper/xcode/<sha256>.json` and printed on stdout in both cases; the exit is
//! 0 on `pass` and 1 otherwise.

mod expansion;
mod farm;
pub(crate) mod free;
mod programs;
mod root;
mod run;
mod sig;
mod tamper;
#[cfg(test)]
mod tests;
mod toc;

use std::ffi::OsString;
use std::io::Read as _;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use crate::args::{Globals, flag_of, value_of};
use crate::help::XCODE_VERIFY_HELP;
use crate::home::Home;
use crate::xcode_push::archive::{Expansion, digest_of, open_file};
use crate::xcode_push::{Mac, say, target};
use crate::xcode_record::{self as record, Archive, PASS, VERIFY, Verify};
use crate::{Flow, PROGRAM, Stop, World};

/// The directory beside the archive that the checks work in.
const SCRATCH: &str = ".hk-xcode-verify-scratch";

/// What a check reads of the run.
struct Ctx<'a> {
    mac: &'a Mac,
    /// The archive, as an absolute path.
    archive: &'a Path,
    size: u64,
    scratch: &'a Path,
    /// `--apple-root`.
    apple_root: Option<&'a Path>,
    /// `--timeout-secs`, for the fetch of Apple's root.
    timeout_secs: u32,
    web_roots: &'a dyn Fn() -> Vec<Vec<u8>>,
}

/// Makes the new directory `dir` of the check `part`.
fn make_dir(dir: &Path, part: &str) -> Result<(), String> {
    std::fs::create_dir(dir)
        .map_err(|error| format!("{part}: cannot make {} ({error})", dir.display()))
}

/// Writes `bytes` as the file `path`, for the check `part`.
fn write_file(path: &Path, bytes: &[u8], part: &str) -> Result<(), String> {
    std::fs::write(path, bytes)
        .map_err(|error| format!("{part}: cannot write {} ({error})", path.display()))
}

/// The kind of an archive by its first bytes.
pub(crate) fn kind_of(first: &[u8]) -> &'static str {
    match first {
        b"xar!" => "xar!",
        b"AA01" => "AA01",
        b"AEA1" => "AEA1",
        _ => "unknown",
    }
}

/// The first four bytes of the file at `path`.
pub(crate) fn first_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(4).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot read {} ({error})", path.display()))?;
    Ok(bytes)
}

/// The failure that the kind of the archive is not `xar!`.
fn kind_check(rec: &Verify, first: &[u8]) -> Result<(), String> {
    match rec.archive.kind.as_str() {
        "xar!" => Ok(()),
        "unknown" => Err(format!(
            "kind: the archive starts with the bytes {}, not xar!: it is not an xip",
            first.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )),
        other => Err(format!(
            "kind: the archive is {other}, not xar!: the farm Macs do not take it, an Xcode archive here is an xip"
        )),
    }
}

/// The checks in order, each filling its section of `rec`; the first failure ends them.
fn checks(w: &mut World<'_>, ctx: &Ctx<'_>, rec: &mut Verify, first: &[u8]) -> Result<(), String> {
    let (found, missing) = programs::check(ctx.mac);
    rec.programs = Some(found);
    missing?;
    kind_check(rec, first)?;
    say(
        w,
        &format!("{}: checking Apple's signature", ctx.archive.display()),
    );
    sig::check(ctx, rec)?;
    say(
        w,
        "damaging a clone of the archive and a file that is no archive",
    );
    tamper::check(ctx, rec)?;
    toc::read(ctx, rec)?;
    say(
        w,
        &format!("expanding the archive in {}", ctx.scratch.display()),
    );
    expansion::expand(ctx, rec)
}

/// The path words of `args`, `--apple-root` and `--mac`: what `hk xcode verify` was given.
struct Given {
    path: Vec<u8>,
    apple_root: Option<Vec<u8>>,
    mac: Option<Vec<u8>>,
}

/// Reads the words after `verify`; `None` when `--help` was asked and printed.
fn parse(w: &mut World<'_>, globals: &mut Globals, args: &[Vec<u8>]) -> Flow<Option<Given>> {
    let mut words: Vec<&[u8]> = Vec::new();
    let mut apple_root: Option<Vec<u8>> = None;
    let mut mac: Option<Vec<u8>> = None;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let rest = args.get(at..).unwrap_or_default();
        if matches!(arg.as_slice(), b"--help" | b"-h") {
            let _ = writeln!(w.stdout, "{XCODE_VERIFY_HELP}");
            return Ok(None);
        }
        if flag_of(arg) == b"--apple-root" {
            let (value, took) = value_of(rest)?;
            if apple_root.replace(value.to_vec()).is_some() {
                return Err(Stop::usage("--apple-root is given twice"));
            }
            at += took;
        } else if flag_of(arg) == b"--mac" {
            let (value, took) = value_of(rest)?;
            if mac.replace(value.to_vec()).is_some() {
                return Err(Stop::usage("--mac is given twice"));
            }
            at += took;
        } else if let Some(took) = globals.take(rest)? {
            at += took;
        } else if arg.starts_with(b"-") {
            return Err(Stop::usage(
                [
                    format!("{PROGRAM} xcode verify has no flag ").as_bytes(),
                    flag_of(arg),
                ]
                .concat(),
            ));
        } else {
            words.push(arg);
            at += 1;
        }
    }
    let [path] = words.as_slice() else {
        return Err(Stop::usage(format!(
            "{PROGRAM} xcode verify takes the path of one archive (~/Downloads/Xcode_27.2.xip)"
        )));
    };
    Ok(Some(Given {
        path: path.to_vec(),
        apple_root,
        mac,
    }))
}

/// `hk xcode verify`: `args` are the words after `verify`; `globals` are those given before it.
pub(crate) fn xcode_verify(w: &mut World<'_>, mut globals: Globals, args: &[Vec<u8>]) -> Flow<()> {
    let Some(given) = parse(w, &mut globals, args)? else {
        return Ok(());
    };
    let farm = crate::xcode_farm::mac_of(w, &mut globals, "xcode verify", given.mac)?;
    if farm.is_some() {
        if given.apple_root.is_some() {
            return Err(Stop::usage(format!(
                "--apple-root is for the check on this Mac, which fetches or reads Apple's root \
                 certificate itself; this system is {}, and the archive is checked on a farm Mac",
                w.mac.os
            )));
        }
        target(&globals, "xcode verify")?;
    }
    let Some(home) = Home::of(w) else {
        return Err(Stop::fail(format!(
            "{PROGRAM} xcode verify keeps its record under $HOME, which is not set"
        )));
    };
    home.ensure()?;
    let mac = w.mac;
    let given_path = PathBuf::from(OsString::from_vec(given.path));
    let mut file = open_file(&given_path).map_err(Stop::fail)?;
    // `absolute` fails only for a relative path when the working directory cannot be read, and
    // then the archive could not have been opened.
    let archive = std::path::absolute(&given_path).unwrap_or(given_path);
    say(w, &format!("{}: hashing", archive.display()));
    let (sha256, size) = digest_of(&mut file).map_err(Stop::fail)?;
    let first = first_bytes(&archive).map_err(Stop::fail)?;
    if let Some(on) = farm {
        let local = farm::Local {
            path: &archive,
            size,
            sha256: &sha256,
            kind: kind_of(&first),
        };
        return farm::verify(w, &globals, &home, &on, &local);
    }
    let beside = archive.parent().unwrap_or(Path::new("/"));
    let scratch = beside.join(SCRATCH);
    // What an earlier run stopped by a signal left: it is not there, or it is removed, or making
    // it fails below.
    let _ = std::fs::remove_dir_all(&scratch);
    make_dir(&scratch, "scratch").map_err(Stop::fail)?;
    let scratch = Expansion(scratch);
    let apple_root = given
        .apple_root
        .map(|path| PathBuf::from(OsString::from_vec(path)));
    let ctx = Ctx {
        mac,
        archive: &archive,
        size,
        scratch: &scratch.0,
        apple_root: apple_root.as_deref(),
        timeout_secs: globals.timeout_secs(),
        web_roots: w.web_roots,
    };
    let mut rec = Verify {
        record: VERIFY,
        hk: w.stamp.to_owned(),
        macos: programs::macos(mac),
        programs: None,
        archive: Archive {
            name: archive
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            size,
            sha256: sha256.clone(),
            kind: kind_of(&first).to_owned(),
        },
        signature: None,
        tamper: None,
        toc: None,
        expansion: None,
        farm: None,
        verdict: String::new(),
    };
    let result = checks(w, &ctx, &mut rec, &first);
    drop(scratch);
    rec.verdict = match &result {
        Ok(()) => PASS.to_owned(),
        Err(why) => format!("fail: {why}"),
    };
    let text = record::json(&rec)?;
    let path = record::write(&home, &sha256, &text)?;
    let _ = write!(w.stdout, "{text}");
    say(
        w,
        &format!("verdict {}; record kept in {}", rec.verdict, path.display()),
    );
    match result {
        Ok(()) => Ok(()),
        Err(_) => Err(Stop::Exit(1)),
    }
}
