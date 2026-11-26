//! The expansion and the version read (MP-X5): the free space, `xip --expand` and its seconds, the
//! app it makes, `plutil` for the version and the build, and `codesign` and `spctl` on the app.
//! The seconds are read from the Mac's clock and recorded; nothing judges them.

use std::path::Path;

use super::free::{enough, free_bytes};
use super::run::{exit_text, tool};
use super::{Ctx, Mac, make_dir};
use crate::xcode_push::archive::{is_build, is_version};
use crate::xcode_record::{Expansion, Output, Plutil, Verify};

/// The names of the entries of `dir` that `keep` takes, sorted.
fn names(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| keep(&entry.path()))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// The value `plutil -extract <key> raw` read from `file`.
fn plist(mac: &Mac, file: &Path, key: &str) -> Output {
    tool(
        &mac.plutil,
        &[&"-extract", &key, &"raw", &"-o", &"-", &file],
        None,
    )
}

/// The text of a value `plutil` printed, when it exited 0.
fn value(out: &Output) -> Option<String> {
    (out.exit == Some(0)).then(|| out.stdout.trim().to_owned())
}

/// Expands the archive, reads the release, and records all of it; the failure is the first of:
/// too little free space, `xip` not exiting 0, not one app, a version or a build that is not an
/// Xcode release's, `codesign` not exiting 0.
pub(super) fn expand(ctx: &Ctx<'_>, rec: &mut Verify) -> Result<(), String> {
    let mac = ctx.mac;
    let exp = rec.expansion.insert(Expansion::default());
    let dir = ctx.scratch.join("expansion");
    make_dir(&dir, "expansion")?;
    let before = free_bytes(mac, &dir);
    exp.free_before = before.as_ref().ok().copied();
    let before = before.map_err(|why| format!("expansion: {why}"))?;
    enough(before, ctx.size, &dir)?;
    let start = (mac.clock)();
    let out = tool(&mac.xip, &[&"--expand", &ctx.archive], Some(&dir));
    let end = (mac.clock)();
    exp.seconds = Some(end.saturating_sub(start).as_secs_f64());
    exp.free_after = free_bytes(mac, &dir).ok();
    exp.exit = out.exit;
    let xip = mac.xip.display();
    let said = exit_text(&out);
    exp.output = Some(out.stdout + &out.stderr);
    if exp.exit != Some(0) {
        return Err(format!("expansion: {xip} --expand {said}"));
    }
    exp.apps = names(&dir, |path| {
        path.extension().is_some_and(|ext| ext == "app")
    });
    let [app] = exp.apps.as_slice() else {
        return Err(format!(
            "expansion: {xip} --expand made {} apps, not one Xcode",
            exp.apps.len()
        ));
    };
    let app = dir.join(app);
    let contents = app.join("Contents");
    exp.contents = names(&contents, |_| true);
    exp.version_plist = contents.join("version.plist").is_file();
    let version = plist(
        mac,
        &contents.join("Info.plist"),
        "CFBundleShortVersionString",
    );
    let build = plist(mac, &contents.join("version.plist"), "ProductBuildVersion");
    exp.version = value(&version);
    exp.build = value(&build);
    exp.plutil = Some(Plutil { version, build });
    let codesign = tool(
        &mac.codesign,
        &[&"--verify", &"--deep", &"--strict", &app],
        None,
    );
    let signed = codesign.exit == Some(0);
    let said = exit_text(&codesign);
    exp.codesign = Some(codesign);
    exp.spctl = Some(tool(
        &mac.spctl,
        &[&"--assess", &"--type", &"execute", &"-vv", &app],
        None,
    ));
    let (Some(version), Some(build)) = (&exp.version, &exp.build) else {
        return Err("expansion: plutil could not read the version and the build".to_owned());
    };
    if !is_version(version.as_bytes()) || !is_build(build.as_bytes()) {
        return Err(format!(
            "expansion: the app says version {version:?} and build {build:?}, which are not an Xcode release's"
        ));
    }
    if !signed {
        return Err(format!(
            "expansion: {} --verify --deep --strict {said}",
            mac.codesign.display()
        ));
    }
    Ok(())
}
