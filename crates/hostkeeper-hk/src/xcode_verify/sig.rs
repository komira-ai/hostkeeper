//! Apple's signature on the archive (MP-X2): `pkgutil --check-signature` as it printed, read and
//! judged by `xcode_push::signature`, and Apple's published root beside the constant.

use std::path::Path;

use super::run::{exit_text, tool};
use super::{Ctx, root};
use crate::xcode_push::signature::{parse, verdict};
use crate::xcode_record::{Link, Signature, Verify};

/// Checks the signature of the archive and records it; the failure is the first of: pkgutil's
/// exit, the Built verdict, Apple's root not read, its fingerprint not the constant.
pub(super) fn check(ctx: &Ctx<'_>, rec: &mut Verify) -> Result<(), String> {
    let out = tool(
        &ctx.mac.pkgutil,
        &[&"--check-signature", &ctx.archive],
        None,
    );
    let report = parse(&out.stdout);
    let apple_root = root::published(ctx.mac, ctx.apple_root, ctx.web_roots, ctx.timeout_secs);
    let exit = out.exit;
    let said = exit_text(&out);
    let signature = rec.signature.insert(Signature {
        output: out,
        status: report.status.clone(),
        chain: report
            .chain
            .iter()
            .map(|link| Link {
                name: link.name.clone(),
                sha256: link.sha256.clone(),
            })
            .collect(),
        apple_root,
    });
    if exit != Some(0) {
        return Err(format!(
            "signature: {} --check-signature {said}",
            ctx.mac.pkgutil.display()
        ));
    }
    verdict(&report).map_err(|why| {
        format!("signature: pkgutil does not report Apple's own signature: {why}")
    })?;
    let root = &signature.apple_root;
    if let Some(error) = &root.error {
        return Err(format!("signature: Apple's root certificate: {error}"));
    }
    if !root.equal {
        return Err(format!(
            "signature: Apple's published root has the SHA-256 fingerprint {}, not the {} the code holds",
            root.sha256.as_deref().unwrap_or_default(),
            root.constant
        ));
    }
    Ok(())
}

/// `pkgutil --check-signature` on `file`: its output and the status it gave.
pub(super) fn of(ctx: &Ctx<'_>, file: &Path) -> (crate::xcode_record::Output, Option<String>) {
    let out = tool(&ctx.mac.pkgutil, &[&"--check-signature", &file], None);
    let status = parse(&out.stdout).status;
    (out, status)
}
