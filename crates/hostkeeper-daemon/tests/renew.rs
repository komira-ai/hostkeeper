//! `hostkeeper renew` (`hostkeeper_daemon::local::renew`): root's ask, on the host, that the
//! daemon renew the host's certificate at its next enrollment step; what it writes in the pki
//! directory, what it says, its exit status, and its flags.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::cli::{self, Cli, PKI_DIR};
use hostkeeper_daemon::local::{self, EXIT_REFUSED, LocalVerb, NOT_ENROLLED, RenewArgs};
use hostkeeper_testkit::Scratch;

/// A scratch directory for one test.
fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// A pki directory under `scratch`, 0700, whose `current` points at generation `generation`
/// when there is one: all of the directory that `renew` reads.
fn pki(scratch: &Scratch, generation: Option<u64>) -> PathBuf {
    let pki = scratch.path().join("pki");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&pki)
        .expect("the pki directory");
    fs::write(pki.join("SCHEMA"), "1\n").expect("SCHEMA");
    if let Some(generation) = generation {
        let name = format!("gen-{generation}");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(pki.join(&name))
            .expect("a generation");
        symlink(&name, pki.join("current")).expect("current");
    }
    pki
}

/// `hostkeeper renew --pki-dir <pki>`: its exit status, standard output and standard error.
fn renew(pki: &Path) -> (u8, String, String) {
    let args = RenewArgs {
        pki_dir: pki.to_path_buf(),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = local::renew(&args, &mut out, &mut err);
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("UTF-8");
    (code, text(out), text(err))
}

/// Catches an ask the daemon would not see, one others can read, or a success that does not
/// say what it asked: on a host whose `current` is generation 3, `renew` exits 0, says it
/// asked for generation 3 on standard output alone, and leaves `RENEW`, an empty regular file of
/// mode 0600, beside `current`; asked again, it says the same and leaves the one file.
#[test]
fn renew_asks_for_the_current_generation() {
    let scratch = scratch("renew-asks");
    let pki = pki(&scratch, Some(3));
    let said = format!(
        "renewal asked: the daemon renews generation 3 of {} at its next enrollment step\n",
        pki.display()
    );
    for _ in 0..2 {
        assert_eq!(renew(&pki), (0, said.clone(), String::new()));
        let meta = fs::symlink_metadata(pki.join("RENEW")).expect("the ask");
        assert!(meta.is_file());
        assert_eq!(meta.len(), 0);
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
    let mut names: Vec<String> = fs::read_dir(&pki)
        .expect("the pki directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .into_string()
                .expect("UTF-8")
        })
        .collect();
    names.sort();
    assert_eq!(names, ["RENEW", "SCHEMA", "current", "gen-3"]);
}

/// Catches an ask written for a host with no certificate, or a refusal that does not say why:
/// with no `current`, `renew` exits 1, says `not_enrolled` and the directory on standard error
/// alone, and writes nothing.
#[test]
fn renew_refuses_a_host_with_no_certificate() {
    let scratch = scratch("renew-unenrolled");
    let pki = pki(&scratch, None);
    assert_eq!(
        renew(&pki),
        (
            EXIT_REFUSED,
            String::new(),
            format!(
                "hostkeeper renew: refused: {NOT_ENROLLED}: {} holds no certificate; the host's enrollment gives it its first\n",
                pki.display()
            )
        )
    );
    assert!(!pki.join("RENEW").exists());
}

/// Catches a pki directory that is not there, or not the daemon's own, taken as asked: a
/// directory that is missing, and one that is a link, each exit 1 with `store_failed` and the
/// store's reason on standard error, and nothing is written.
#[test]
fn renew_refuses_a_directory_the_daemon_would_refuse() {
    let scratch = scratch("renew-refused");
    let missing = scratch.path().join("missing");
    assert_eq!(
        renew(&missing),
        (
            EXIT_REFUSED,
            String::new(),
            "hostkeeper renew: refused: store_failed: the renewal ask: cannot use the pki directory: No such file or directory (os error 2)\n"
                .to_owned()
        )
    );
    assert!(!missing.exists());

    let real = pki(&scratch, Some(1));
    let link = scratch.path().join("link");
    symlink(&real, &link).expect("a link");
    assert_eq!(
        renew(&link),
        (
            EXIT_REFUSED,
            String::new(),
            format!(
                "hostkeeper renew: refused: store_failed: the renewal ask: {link:?} is not the daemon's own: a directory or key must be owned by it, not a link, and closed to group and others\n"
            )
        )
    );
    assert!(!real.join("RENEW").exists());
}

/// Catches a verb the binary does not wire to its code, or a default that is not the daemon's
/// pki directory: `hostkeeper renew --pki-dir <dir>` from the command line exits 0 and writes the
/// ask; with no flag the directory is the daemon's own, `/etc/updater/pki`; a flag `renew` does
/// not take is a usage error.
#[test]
fn renew_runs_from_the_command_line_with_the_daemons_pki_directory() {
    let scratch = scratch("renew-cli");
    let pki = pki(&scratch, Some(1));
    let line = [
        "hostkeeper",
        "renew",
        "--pki-dir",
        pki.to_str().expect("a path"),
    ];
    assert_eq!(
        format!("{:?}", cli::run(Cli::parse_from(line))),
        format!("{:?}", ExitCode::from(0))
    );
    assert!(pki.join("RENEW").is_file());

    let parsed = Cli::parse_from(["hostkeeper", "renew"]);
    let cli::Command::Local(LocalVerb::Renew(args)) = &parsed.command else {
        panic!("not renew");
    };
    assert_eq!(args.pki_dir, Path::new(PKI_DIR));
    assert_eq!(PKI_DIR, "/etc/updater/pki");

    let refused = Cli::try_parse_from(["hostkeeper", "renew", "--service", "web"])
        .expect_err("a usage error");
    assert_eq!(refused.exit_code(), 2);
}
