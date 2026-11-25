//! `hk xcode add [<version> | <path to .xip>]`: brings an Xcode release into the coordinator's
//! Xcode store from the operator's own Mac (docs/design/xcode-one-action.md section 2), and
//! `hk xcode-push <version> | <path>`, its second name for one release, which says
//! `hk xcode-push is now hk xcode add` on stderr first and then does the same.
//!
//! In order, each step ending the run before anything is uploaded when it fails:
//!
//! 0. the settings file fills the flags, the CA and the token are read, `~/.hostkeeper` and
//!    `~/.hostkeeper/xcode` are made private ([`crate::home`]) and the lock ([`lock`]) is taken
//!    and held to the end;
//! 1. the archive ([`find`]): the path given; a version's file in `~/Downloads`, or downloaded with
//!    `xcodes`; with nothing given, the newest archive in `~/Downloads`, else the release waiting
//!    in the store ([`store`]);
//! 2. its kind, from its first four bytes: `xar!` goes on, `AA01` and `AEA1` are
//!    `archive_kind_unsupported`, anything else is `not_an_xip` (exit 2);
//! 3. its SHA-256 and size, read once in 8 MiB blocks, with progress on a terminal ([`progress`]);
//! 4. `Xcode.List`: an archive of these bytes already held ends the run with exit 0 after the Macs
//!    summary (a given version the held archive is not is `version_mismatch`);
//! 5. Apple's signature (`pkgutil`, [`crate::xcode_push::signature`]);
//! 6. the version and the build: from the record of an earlier run ([`reuse`]), else from the
//!    expansion, after the free space check (`free_space_short`), with `codesign` on the app,
//!    which is then recorded as `hk-xcode-add-v1`;
//! 7. `Xcode.List` again: the same release from other bytes, or another build of the version, stops
//!    the run naming the `hk xcode remove` line;
//! 8. the upload with progress ([`crate::xcode_push::upload`]), each refusal said in words, then
//!    `stored: ...` on stdout and the Macs summary ([`summary`]), whose failure leaves the exit 0.
//!
//! Progress and what the run is doing go to stderr; stdout holds the answer lines only.

mod find;
mod lock;
pub(crate) mod progress;
mod reuse;
mod store;
mod summary;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use crate::args::{Globals, flag_of};
use crate::help::{XCODE_ADD_HELP, XCODE_PUSH_HELP};
use crate::home::Home;
use crate::xcode_push::archive::{self, Release, digest_blocks, is_version};
use crate::xcode_push::free_check;
use crate::xcode_push::upload::{self, Archive};
use crate::xcode_push::{Mac, Target, failed, say, signature, target};
use crate::xcode_record::{self as record, Add, AddExpansion, AddSignature};
use crate::{Flow, PROGRAM, Stop, World};

/// The block the archive is hashed in.
const HASH_BLOCK: usize = 8 << 20;

/// What the second name says on stderr first.
const NOTICE: &str = "hk xcode-push is now hk xcode add";

/// How a run was asked for the archive.
enum Pick {
    Path(PathBuf),
    Version(String),
    Newest,
}

/// `hk xcode add`.
pub(crate) fn xcode_add(w: &mut World<'_>, globals: Globals, args: &[Vec<u8>]) -> Flow<()> {
    run(w, globals, args, "xcode add")
}

/// `hk xcode-push`: the notice, then `hk xcode add` with exactly one argument.
pub(crate) fn xcode_push(w: &mut World<'_>, globals: Globals, args: &[Vec<u8>]) -> Flow<()> {
    if !args
        .iter()
        .any(|a| matches!(a.as_slice(), b"--help" | b"-h"))
    {
        let _ = writeln!(w.stderr, "{NOTICE}");
    }
    run(w, globals, args, "xcode-push")
}

/// The words after the verb `word`, the global flags among them taken; `None` when `--help` was
/// asked and printed.
fn parse(
    w: &mut World<'_>,
    globals: &mut Globals,
    args: &[Vec<u8>],
    word: &str,
) -> Flow<Option<Vec<Vec<u8>>>> {
    let mut words = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if matches!(arg.as_slice(), b"--help" | b"-h") {
            let help = if word == "xcode add" {
                XCODE_ADD_HELP
            } else {
                XCODE_PUSH_HELP
            };
            let _ = writeln!(w.stdout, "{help}");
            return Ok(None);
        }
        if let Some(took) = globals.take(args.get(at..).unwrap_or_default())? {
            at += took;
        } else if arg.starts_with(b"-") {
            return Err(Stop::usage(
                [
                    format!("{PROGRAM} {word} has no flag ").as_bytes(),
                    flag_of(arg),
                ]
                .concat(),
            ));
        } else {
            words.push(arg.clone());
            at += 1;
        }
    }
    Ok(Some(words))
}

/// The one thing asked for: nothing (`hk xcode add` alone), a version or a path.
fn pick_of(words: &[Vec<u8>], word: &str) -> Flow<Pick> {
    match words {
        [] if word == "xcode add" => Ok(Pick::Newest),
        [one] if is_version(one) => Ok(Pick::Version(String::from_utf8_lossy(one).into_owned())),
        [one] => Ok(Pick::Path(PathBuf::from(OsString::from_vec(one.clone())))),
        _ if word == "xcode add" => Err(Stop::usage(format!(
            "{PROGRAM} xcode add takes at most one Xcode version (27.2) or the path of an \
             archive (~/Downloads/Xcode_27.2.xip)"
        ))),
        _ => Err(Stop::usage(format!(
            "{PROGRAM} {word} takes one Xcode version (27.2) or the path of an archive \
             (~/Downloads/Xcode_27.2.xip)"
        ))),
    }
}

/// The failure that the archive's first bytes are not an xip's.
fn kind(first: &[u8], path: &Path) -> Flow<()> {
    match first {
        b"xar!" => Ok(()),
        b"AA01" | b"AEA1" => Err(failed(&format!(
            "archive_kind_unsupported: {} ({}) is {}, which the farm Macs would defer; an Xcode \
             archive here is an xip",
            String::from_utf8_lossy(first),
            path.display(),
            if first == b"AA01" {
                "an Apple Archive"
            } else {
                "an Apple Encrypted Archive"
            }
        ))),
        _ => Err(Stop::usage(format!(
            "not_an_xip: {} starts with the bytes {}, not xar!: it is no Xcode archive; nothing \
             was uploaded",
            path.display(),
            first.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ))),
    }
}

/// `Xcode 27.2 (27B52)`.
fn stored(version: &str, build: &str) -> String {
    format!("Xcode {version} ({build})")
}

fn run(w: &mut World<'_>, mut globals: Globals, args: &[Vec<u8>], word: &str) -> Flow<()> {
    let Some(words) = parse(w, &mut globals, args, word)? else {
        return Ok(());
    };
    if w.mac.os != "macos" {
        return Err(Stop::fail(format!(
            "{PROGRAM} {word} runs on macOS only, on the Mac signed in to the Apple developer \
             account: it runs xcodes, pkgutil and xip, which are macOS programs, and this system \
             is {}",
            w.mac.os
        )));
    }
    let pick = pick_of(&words, word)?;
    crate::settings::fill(w, &mut globals)?;
    let target = target(&globals, word)?;
    let Some(home) = Home::of(w) else {
        return Err(Stop::fail(format!(
            "{PROGRAM} {word} keeps its records and its lock under $HOME, which is not set"
        )));
    };
    let xcode_dir = home.ensure()?;
    let _lock = lock::take(&home, &xcode_dir, word)?;
    add(w, &globals, &target, &home, &xcode_dir, pick)
}

fn add(
    w: &mut World<'_>,
    globals: &Globals,
    target: &Target,
    home: &Home,
    xcode_dir: &Path,
    pick: Pick,
) -> Flow<()> {
    let mac = w.mac.clone();
    let downloads = w.home.clone().unwrap_or_default().join(find::DOWNLOADS);
    let newest = matches!(pick, Pick::Newest);
    let mut wanted: Option<String> = None;
    let (path, why) = match pick {
        Pick::Path(path) => (path, "the file given".to_owned()),
        Pick::Version(version) => {
            wanted = Some(version.clone());
            by_version(w, &mac, &downloads, &version)?
        }
        Pick::Newest => match find::newest(&downloads) {
            Some(path) => (
                path,
                format!("the newest Xcode archive in {}", downloads.display()),
            ),
            None => {
                let listing = store::list(w, globals)?;
                let Some(waiting) = listing.waiting.clone() else {
                    let said = match listing.archives.first() {
                        Some(held) => format!(
                            "the store holds the newest release ({}, {}); nothing to add",
                            held.version, held.build
                        ),
                        None => "the store holds no release and none is waiting; nothing to add"
                            .to_owned(),
                    };
                    let _ = writeln!(w.stdout, "{said}");
                    return Ok(());
                };
                wanted = Some(waiting.version.clone());
                by_version(w, &mac, &downloads, &waiting.version)?
            }
        },
    };
    say(w, &format!("{}: {why}", path.display()));
    let mut file = archive::open(&path)?;
    let first = crate::xcode_verify::first_bytes(&path).map_err(|why| failed(&why))?;
    kind(&first, &path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (sha256, size) = hash(w, &mut file)?;
    let listing = store::list(w, globals)?;
    if let Some(held) = listing.of_bytes(&sha256) {
        if let Some(asked) = &wanted
            && find::parts(asked) != find::parts(&held.version)
        {
            return Err(failed(&format!(
                "version_mismatch: asked {asked}, these bytes are {} in the store",
                stored(&held.version, &held.build)
            )));
        }
        let _ = writeln!(
            w.stdout,
            "already in the store: {}",
            stored(&held.version, &held.build)
        );
        summary::macs(w, globals, &listing, held);
        if newest && let Some(waiting) = &listing.waiting {
            let _ = writeln!(
                w.stdout,
                "Xcode {0} is waiting: {PROGRAM} xcode add {0}",
                waiting.version
            );
        }
        return Ok(());
    }
    say(
        w,
        &format!(
            "sha256 {sha256}, {}; the store holds no archive of these bytes",
            progress::size_text(size)
        ),
    );
    signature::check(w, &mac, &path)?;
    let record_path = xcode_dir.join(format!("{sha256}.json"));
    let (version, build) = match reuse::release_of(&record_path, w.uid, &sha256, size) {
        Some(found) => {
            say(
                w,
                &format!(
                    "{}, read from the record kept in {}; the expansion is skipped",
                    stored(&found.0, &found.1),
                    record_path.display()
                ),
            );
            found
        }
        None => {
            let release = expand(w, &mac, &path, size)?;
            keep(w, home, &record_path, &name, &sha256, size, &release)?;
            (release.version, release.build)
        }
    };
    if let Some(asked) = &wanted
        && find::parts(asked) != find::parts(&version)
    {
        return Err(failed(&format!(
            "version_mismatch: asked {asked}, {} is {}",
            path.display(),
            stored(&version, &build)
        )));
    }
    let latest = store::list(w, globals)?;
    other_bytes(&latest, &sha256, &version, &build)?;
    upload::put(
        w,
        target,
        Archive {
            file,
            size,
            sha256: &sha256,
            version: &version,
            build: &build,
        },
    )?;
    let _ = writeln!(
        w.stdout,
        "stored: {}, sha256 {sha256}, {size} bytes",
        stored(&version, &build)
    );
    let release = store::Held {
        sha256,
        version,
        build,
    };
    summary::macs(w, globals, &latest, &release);
    Ok(())
}

/// The archive named for `version`: in `~/Downloads`, else downloaded; with how it was found.
fn by_version(
    w: &mut World<'_>,
    mac: &Mac,
    downloads: &Path,
    version: &str,
) -> Flow<(PathBuf, String)> {
    match find::in_downloads(downloads, version) {
        Some(path) => Ok((path, format!("the newest archive named for {version}"))),
        None => Ok((
            find::download(w, mac, downloads, version)?,
            format!("downloaded by {}", mac.xcodes.display()),
        )),
    }
}

/// The SHA-256 and size of `file`, hashed once, with progress on a terminal.
fn hash(w: &mut World<'_>, file: &mut std::fs::File) -> Flow<(String, u64)> {
    let total = file.metadata().map(|meta| meta.len()).unwrap_or_default();
    let mut shown = progress::Progress::new("hashing", total, w.stderr_terminal, w.mac.clock);
    let terminal = w.stderr_terminal;
    let out = &mut *w.stderr;
    let hashed = digest_blocks(file, HASH_BLOCK, |done| {
        if terminal {
            shown.at(done, out);
        }
    });
    shown.finish(out);
    hashed.map_err(|why| failed(&why))
}

/// The free space check, then the expansion of the archive at `path`.
fn expand(w: &mut World<'_>, mac: &Mac, path: &Path, size: u64) -> Flow<Release> {
    let beside = std::path::absolute(path)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    free_check(mac, &beside, size)?;
    archive::release(w, mac, path)
}

/// Writes the record of this run, unless a passing record of `hk xcode verify` is there.
fn keep(
    w: &mut World<'_>,
    home: &Home,
    record_path: &Path,
    name: &str,
    sha256: &str,
    size: u64,
    release: &Release,
) -> Flow<()> {
    if reuse::keeps(record_path) {
        say(
            w,
            &format!(
                "{} holds a passing record of {PROGRAM} xcode verify, which is kept",
                record_path.display()
            ),
        );
        return Ok(());
    }
    let rec = Add {
        record: record::ADD,
        hk: w.stamp.to_owned(),
        archive: record::Archive {
            name: name.to_owned(),
            size,
            sha256: sha256.to_owned(),
            kind: "xar!".to_owned(),
        },
        signature: AddSignature {
            status: signature::SIGNED_BY_APPLE.to_owned(),
        },
        expansion: AddExpansion {
            version: release.version.clone(),
            build: release.build.clone(),
            codesign: release.codesign.clone(),
        },
        verdict: record::PASS,
    };
    let written = record::write(home, sha256, &record::json(&rec)?)?;
    say(
        w,
        &format!(
            "{}, read from the expanded app; record kept in {}",
            stored(&release.version, &release.build),
            written.display()
        ),
    );
    Ok(())
}

/// Stops when the store holds this release from other bytes, or another build of the version.
fn other_bytes(listing: &store::Listing, sha256: &str, version: &str, build: &str) -> Flow<()> {
    let Some(held) = listing
        .archives
        .iter()
        .filter(|held| held.sha256 != sha256)
        .find(|held| find::parts(&held.version) == find::parts(version))
    else {
        return Ok(());
    };
    let line = format!(
        "{PROGRAM} xcode remove --sha256 {} --reason <why> first",
        held.sha256
    );
    if held.build == build {
        Err(failed(&format!(
            "the store holds {} from other bytes (sha256 {}); if these are the ones to keep, \
             {line}",
            stored(version, build),
            held.sha256
        )))
    } else {
        Err(failed(&format!(
            "the store holds another build of Xcode {version}: {} (sha256 {}); if build {build} \
             is the one to keep, {line}",
            held.build, held.sha256
        )))
    }
}
