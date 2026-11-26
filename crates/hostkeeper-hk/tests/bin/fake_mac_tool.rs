//! A stand-in for `xcodes`, `pkgutil`, `xip`, `plutil`, `codesign`, `spctl`, `xar`, `sw_vers`, `uname`,
//! `cp` and `df` in the tests of `hk xcode-push` and `hk xcode verify`, and for
//! `tailscale` in those of `hk tailnet-wait`, by the name of the file it is run as,
//! `<dir>/<name>`. Every run appends its arguments, joined by tabs, as one line to
//! `<dir>/<name>.args` and its environment, `NAME=value` a line, to `<dir>/<name>.env`; prints
//! `<dir>/<name>.<n>.out` when there is one for this run's number `n` (the lines of the arguments
//! file, this run's included), else `<dir>/<name>.<target>.out` when there is one for the file
//! name of the last argument, else `<dir>/<name>.out` when there is one; and `<dir>/<name>.<first argument>.err` to stderr when
//! there is one; never ends when `<dir>/<name>.<first argument>.hang` exists; and exits with the
//! number in `<dir>/<name>.<target>.exit`, else in `<dir>/<name>.<first argument>.exit`, else in
//! `<dir>/<name>.exit` (0 without any); the target is the file name of the last argument. A last
//! argument that is a file of 64 KiB or less is also copied to `<dir>/<name>.<target>.bytes`, as
//! it is when the program starts. Besides:
//!
//! - `xcodes download <version> --directory <into>` copies `<dir>/xcodes.xip`, when there is one,
//!   to `<into>/Xcode-<version>.xip`;
//! - `xip --expand <archive>` makes `Xcode.app/Contents` in its working directory, with
//!   `Info.plist` a copy of `<dir>/xip.version` and `version.plist` of `<dir>/xip.product-build`, when
//!   `<dir>/xip.version` exists, and `Xcode-beta.app/Contents` too when `<dir>/xip.second` exists; and
//!   appends `<dir>/xip.append` to the archive when that exists;
//! - `plutil -extract <key> raw -o - <file>` prints the file when the key is the one of its name
//!   (`CFBundleShortVersionString` of `Info.plist`, `ProductBuildVersion` of `version.plist`),
//!   and exits 1 when it is not, or when the key is what `<dir>/plutil.refuse` holds;
//! - `xip --list <archive>` prints `<dir>/xip.list`, `xip --help` prints `<dir>/xip.usage`;
//! - `xar -tf <archive>` prints `<dir>/xar.list`; `xar --dump-toc=<file> -f <archive>` writes
//!   `<dir>/xar.toc` to `<file>`; `xar -xf <archive> Metadata` writes `<dir>/xar.metadata` to
//!   `Metadata` in its working directory; each when that file exists;
//! - `cp -c <from> <to>` copies the file when the exit is 0.

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
    let run_number = std::fs::read_to_string(at("args"))?.lines().count();
    let target = args
        .last()
        .and_then(|last| Path::new(last).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(last) = args.last()
        && let Ok(bytes) = std::fs::read(last)
        && bytes.len() <= 65536
    {
        std::fs::write(at(&format!("{target}.bytes")), bytes)?;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the stand-in's job is to report the environment it inherited"
    )]
    let vars = std::env::vars_os();
    let env: String = vars
        .map(|(k, v)| format!("{}={}\n", k.to_string_lossy(), v.to_string_lossy()))
        .collect();
    append(&at("env"), env.as_bytes())?;
    let printed = [
        format!("{run_number}.out"),
        format!("{target}.out"),
        "out".to_owned(),
    ]
    .iter()
    .find_map(|suffix| std::fs::read(at(suffix)).ok());
    if let Some(out) = printed {
        std::io::stdout().write_all(&out)?;
    }
    let first = args.first().map(String::as_str).unwrap_or_default();
    let of_verb = |suffix: &str| dir.join(format!("{name}.{first}.{suffix}"));
    if let Ok(err) =
        std::fs::read(at(&format!("{target}.err"))).or_else(|_| std::fs::read(of_verb("err")))
    {
        std::io::stderr().write_all(&err)?;
    }
    if of_verb("hang").exists() {
        // Until it is killed.
        loop {
            std::thread::park();
        }
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = [at(&format!("{target}.exit")), of_verb("exit"), at("exit")]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0);
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
        ("xip", ["--list", _]) => {
            if let Ok(list) = std::fs::read(dir.join("xip.list")) {
                std::io::stdout().write_all(&list)?;
            }
        }
        ("xip", ["--help"]) => {
            if let Ok(help) = std::fs::read(dir.join("xip.usage")) {
                std::io::stdout().write_all(&help)?;
            }
        }
        ("xar", ["-tf", _]) => {
            if let Ok(list) = std::fs::read(dir.join("xar.list")) {
                std::io::stdout().write_all(&list)?;
            }
        }
        ("xar", [dump, "-f", _]) if dump.starts_with("--dump-toc=") => {
            if let Ok(toc) = std::fs::read(dir.join("xar.toc")) {
                std::fs::write(dump.trim_start_matches("--dump-toc="), toc)?;
            }
        }
        ("xar", ["-xf", _, "Metadata"]) => {
            if let Ok(metadata) = std::fs::read(dir.join("xar.metadata")) {
                std::fs::write("Metadata", metadata)?;
            }
        }
        ("cp", ["-c", from, to]) if code == 0 => {
            std::fs::copy(from, to)?;
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
