//! On macOS, embeds `macos/Info.plist` in the `hostkeeper` binary's `__TEXT,__info_plist` section,
//! so the program has a bundle identifier, `dev.hostkeeper.hostkeeper`, the identifier the
//! release signs it under (xtask's `mac_programs.rs`). The privacy profile that grants hostkeeper
//! Full Disk Access names the program by that identifier. On every other target this does
//! nothing.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=macos/Info.plist");
    #[expect(
        clippy::disallowed_methods,
        reason = "Cargo tells a build script the target OS and its package directory only through CARGO_CFG_TARGET_OS and CARGO_MANIFEST_DIR"
    )]
    let (target_os, manifest_dir) = (
        std::env::var_os("CARGO_CFG_TARGET_OS"),
        std::env::var_os("CARGO_MANIFEST_DIR"),
    );
    if target_os.as_deref() != Some("macos".as_ref()) {
        return Ok(());
    }
    let plist = PathBuf::from(manifest_dir.ok_or("cargo set no CARGO_MANIFEST_DIR")?)
        .join("macos/Info.plist");
    let plist = plist
        .to_str()
        .ok_or("the path of Info.plist is not UTF-8")?;
    // The linker reads the option as one comma-separated argument.
    if plist.contains(',') {
        return Err("the path of Info.plist holds a comma".into());
    }
    println!("cargo:rustc-link-arg-bin=hostkeeper=-Wl,-sectcreate,__TEXT,__info_plist,{plist}");
    Ok(())
}
