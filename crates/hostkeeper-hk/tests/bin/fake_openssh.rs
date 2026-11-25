//! A stand-in for `ssh`, `ssh-keygen` and `ssh-add` in the tests: it writes the names of the
//! environment variables it was started with, one per line, to `<its own path>.env`, and exits 0.

use std::io::Write as _;

fn main() -> std::process::ExitCode {
    let Ok(exe) = std::env::current_exe() else {
        return std::process::ExitCode::FAILURE;
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the stand-in's job is to report which variables it inherited"
    )]
    let inherited = std::env::vars_os();
    let mut names: Vec<String> = inherited
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = exe.into_os_string();
    out.push(".env");
    let written = std::fs::File::create(out)
        .and_then(|mut file| file.write_all(format!("{}\n", names.join("\n")).as_bytes()));
    if written.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
