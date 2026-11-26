//! A damaged archive fails (MP-X3): a copy-on-write clone of the archive with the byte at its
//! middle changed, and a 16-byte file that is no archive. For each, `pkgutil --check-signature`
//! and `xip --expand` are run and recorded; `pkgutil`'s answer is not judged, since it may accept
//! a damaged archive and leave the check to `xip`, but `xip --expand` must refuse both. When
//! `/Library/Developer/CommandLineTools/Packages` holds a package, `pkgutil`'s answer on it is
//! recorded too: a package that is not `signed Apple Software`.

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

use super::free::{enough, free_bytes};
use super::run::{exit_text, tool};
use super::{Ctx, make_dir, sig, write_file};
use crate::xcode_record::{CommandLineTools, Output, Refusals, Tamper, Verify};

/// The file that is no archive: 16 bytes.
const NON_ARCHIVE: &[u8; 16] = b"this is no xip!\n";

/// What the record says of a clone that was not made.
const NOT_RUN: &str = "not run, the volume cannot clone";

/// `xip --expand <file>` in the new directory `name` of the scratch directory, after the free
/// space check for an archive of `ctx.size` bytes (the clone is that size).
fn expand(ctx: &Ctx<'_>, file: &Path, name: &str) -> Result<Output, String> {
    let dir = ctx.scratch.join(name);
    make_dir(&dir, "tamper")?;
    let free = free_bytes(ctx.mac, &dir).map_err(|why| format!("tamper: {why}"))?;
    enough(free, ctx.size, &dir)?;
    Ok(tool(&ctx.mac.xip, &[&"--expand", &file], Some(&dir)))
}

/// `pkgutil` and `xip --expand` on `file`.
fn refusals(ctx: &Ctx<'_>, file: &Path, name: &str) -> Result<Refusals, String> {
    let (pkgutil, _) = sig::of(ctx, file);
    Ok(Refusals {
        pkgutil,
        xip: expand(ctx, file, name)?,
    })
}

/// Changes the byte at the middle of the file `path` to another.
fn changed(path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let middle = file.metadata()?.len() / 2;
    file.seek(SeekFrom::Start(middle))?;
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)?;
    file.seek(SeekFrom::Start(middle))?;
    file.write_all(&[!byte.first().copied().unwrap_or_default()])?;
    file.sync_all()
}

/// [`changed`], or the failure that says it could not be.
pub(super) fn damage(path: &Path) -> Result<(), String> {
    changed(path).map_err(|error| {
        format!(
            "tamper: cannot change a byte of {} ({error})",
            path.display()
        )
    })
}

/// The first package of the Command Line Tools' directory, by name.
pub(super) fn tools_package(dir: &Path) -> Option<std::path::PathBuf> {
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pkg"))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Whether `xip` refused: the failure that it accepted `what`, or did not answer.
pub(super) fn refused(out: &Output, what: &str, mac: &super::Mac) -> Result<(), String> {
    match out.exit {
        Some(0) => Err(format!(
            "tamper: {} --expand accepted {what}",
            mac.xip.display()
        )),
        Some(_) => Ok(()),
        None => Err(format!(
            "tamper: {} --expand on {what} {}: {}",
            mac.xip.display(),
            exit_text(out),
            out.stderr.trim()
        )),
    }
}

/// Makes and records the damaged files' runs; the failure is that `xip --expand` accepted one.
pub(super) fn check(ctx: &Ctx<'_>, rec: &mut Verify) -> Result<(), String> {
    let damaged = ctx.scratch.join("damaged.xip");
    let clone = tool(&ctx.mac.cp, &[&"-c", &ctx.archive, &damaged], None);
    let cloned = clone.exit == Some(0);
    let tamper = rec.tamper.insert(Tamper {
        damaged_clone: if cloned { "run" } else { NOT_RUN }.to_owned(),
        clone,
        damaged: None,
        non_archive: None,
        command_line_tools: None,
    });
    if cloned {
        damage(&damaged)?;
        tamper.damaged = Some(refusals(ctx, &damaged, "damaged-expansion")?);
    }
    let fake = ctx.scratch.join("non-archive.xip");
    write_file(&fake, NON_ARCHIVE, "tamper")?;
    let fake = refusals(ctx, &fake, "non-archive-expansion")?;
    let fake_xip = fake.xip.clone();
    tamper.non_archive = Some(fake);
    if let Some(package) = tools_package(&ctx.mac.command_line_tools) {
        let (pkgutil, status) = sig::of(ctx, &package);
        tamper.command_line_tools = Some(CommandLineTools {
            package: package.display().to_string(),
            pkgutil,
            status,
        });
    }
    if let Some(found) = &tamper.damaged {
        refused(&found.xip, "the damaged clone", ctx.mac)?;
    }
    refused(&fake_xip, "the 16-byte file that is no archive", ctx.mac)
}
