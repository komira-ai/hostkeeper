//! A stand-in for `xcodes`, `pkgutil`, `xip` and `plutil` in the tests of `hk xcode-push`, by the
//! name of the file it is run as, `<dir>/<name>`. Every run appends its arguments, joined by tabs,
//! as one line to `<dir>/<name>.args` and its environment, `NAME=value` a line, to
//! `<dir>/<name>.env`; prints `<dir>/<name>.out` when there is one; and exits with the number in
//! `<dir>/<name>.exit` (0 without it). Besides:
//!
//! - `xcodes download <version> --directory <into>` copies `<dir>/xcodes.xip`, when there is one,
//!   to `<into>/Xcode-<version>.xip`;
//! - `xip --expand <archive>` makes `Xcode.app/Contents` in its working directory, with
//!   `Info.plist` a copy of `<dir>/xip.version` and `version.plist` of `<dir>/xip.product-build`, when
//!   `<dir>/xip.version` exists, and `Xcode-beta.app/Contents` too when `<dir>/xip.second` exists; and
//!   appends `<dir>/xip.append` to the archive when that exists;
//! - `plutil -extract <key> raw -o - <file>` prints the file when the key is the one of its name
//!   (`CFBundleShortVersionString` of `Info.plist`, `ProductBuildVersion` of `version.plist`),
//!   and exits 1 when it is not, or when the key is what `<dir>/plutil.refuse` holds.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

/// Appends `bytes` to the file `path`.
fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}

/// The run: its exit code.
fn run() -> std::io::Result<u8> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap_or(Path::new("/")).to_path_buf();
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let at = |suffix: &str| dir.join(format!("{name}.{suffix}"));
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    append(&at("args"), format!("{}\n", args.join("\t")).as_bytes())?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the stand-in's job is to report the environment it inherited"
    )]
    let vars = std::env::vars_os();
    let env: String = vars
        .map(|(k, v)| format!("{}={}\n", k.to_string_lossy(), v.to_string_lossy()))
        .collect();
    append(&at("env"), env.as_bytes())?;
    if let Ok(out) = std::fs::read(at("out")) {
        std::io::stdout().write_all(&out)?;
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match (name.as_str(), words.as_slice()) {
        ("xcodes", ["download", version, "--directory", into]) => {
            let source = dir.join("xcodes.xip");
            if source.exists() {
                std::fs::copy(
                    &source,
                    Path::new(into).join(format!("Xcode-{version}.xip")),
                )?;
            }
        }
        ("xip", ["--expand", archive]) => {
            if dir.join("xip.version").exists() {
                let contents = Path::new("Xcode.app/Contents");
                std::fs::create_dir_all(contents)?;
                std::fs::copy(dir.join("xip.version"), contents.join("Info.plist"))?;
                std::fs::copy(
                    dir.join("xip.product-build"),
                    contents.join("version.plist"),
                )?;
            }
            if dir.join("xip.second").exists() {
                std::fs::create_dir_all("Xcode-beta.app/Contents")?;
            }
            if let Ok(more) = std::fs::read(dir.join("xip.append")) {
                append(Path::new(archive), &more)?;
            }
        }
        ("plutil", ["-extract", key, "raw", "-o", "-", file]) => {
            let wanted = if file.ends_with("/Info.plist") {
                "CFBundleShortVersionString"
            } else if file.ends_with("/version.plist") {
                "ProductBuildVersion"
            } else {
                ""
            };
            let refused = std::fs::read_to_string(dir.join("plutil.refuse")).unwrap_or_default();
            if *key != wanted || *key == refused.trim() {
                return Ok(1);
            }
            std::io::stdout().write_all(&std::fs::read(file)?)?;
        }
        _ => {}
    }
    let code = std::fs::read_to_string(at("exit"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0);
    Ok(code)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("fake mac tool: {error}");
            ExitCode::from(101)
        }
    }
}
