//! What the archive says without being expanded (MP-X4), for the record only: `xip --list`,
//! `xar -tf`, the table of contents (`xar --dump-toc`) and the `Metadata` member, extracted. The
//! expanded app stays the authority for the version and the build.

use std::io::Read as _;
use std::path::Path;

use super::run::tool;
use super::{Ctx, make_dir};
use crate::xcode_record::{Metadata, Toc, Verify};

/// The most bytes of the `Metadata` member that are read.
const METADATA_BYTES: u64 = 1 << 20;

/// What the extracted `Metadata` is, and its text when it is a file.
pub(super) fn metadata(dir: &Path) -> (String, Option<String>) {
    let path = dir.join("Metadata");
    let kind = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => "file",
        Ok(meta) if meta.is_dir() => "directory",
        Ok(_) => "link",
        Err(_) => "missing",
    };
    let mut bytes = Vec::new();
    let text = (kind == "file")
        .then(|| std::fs::File::open(&path).ok())
        .flatten()
        .and_then(|file| file.take(METADATA_BYTES).read_to_end(&mut bytes).ok())
        .map(|_| String::from_utf8_lossy(&bytes).into_owned());
    (kind.to_owned(), text)
}

/// Whether `text` names a version or a build.
pub(super) fn names_version(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("version") || lower.contains("build")
}

/// Runs the four reads and records them. A directory that cannot be made is the only failure.
pub(super) fn read(ctx: &Ctx<'_>, rec: &mut Verify) -> Result<(), String> {
    let mac = ctx.mac;
    let dir = ctx.scratch.join("meta");
    make_dir(&dir, "toc")?;
    let toc_file = ctx.scratch.join("toc.xml");
    let xip_list = tool(&mac.xip, &[&"--list", &ctx.archive], None);
    let xar_list = tool(&mac.xar, &[&"-tf", &ctx.archive], None);
    let mut dump = std::ffi::OsString::from("--dump-toc=");
    dump.push(&toc_file);
    let dump_toc = tool(&mac.xar, &[&dump, &"-f", &ctx.archive], None);
    let extract = tool(&mac.xar, &[&"-xf", &ctx.archive, &"Metadata"], Some(&dir));
    let (kind, text) = metadata(&dir);
    let names = names_version(&xar_list.stdout) || text.as_deref().is_some_and(names_version);
    rec.toc = Some(Toc {
        xip_list,
        xar_list,
        dump_toc,
        toc: std::fs::read(&toc_file)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        metadata: Metadata {
            extract,
            kind,
            text,
        },
        names_version: names,
    });
    Ok(())
}
