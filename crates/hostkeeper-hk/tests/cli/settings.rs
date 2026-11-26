//! The settings file `~/.hostkeeper/hk.toml` and `hk settings init`: the defaults it gives the
//! three global flags, what wins, what is refused, and the file `settings init` writes with the
//! modes it makes, run in process against a fake coordinator on loopback port 0, and under
//! umasks as the built program.

use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use hostkeeper_hk::xcode_push::Mac;

use crate::rig::{Opts, Ran, Recorder, Reply, Rig, Server, TOKEN, args, closed_port};

/// The answer of a coordinator with no hosts.
const NO_HOSTS: &str = "{\"hosts\":[]}";

/// `~/.hostkeeper` of the test's home.
fn dir_of(rig: &Rig) -> PathBuf {
    rig.path("home").join(".hostkeeper")
}

/// The settings file's path.
fn file_of(rig: &Rig) -> PathBuf {
    dir_of(rig).join("hk.toml")
}

/// The text of a settings file for a coordinator on `port`, the CA and the token of the rig.
fn settings_for(rig: &Rig, port: u16) -> String {
    format!(
        "endpoint = \"https://127.0.0.1:{port}\"\nca_file = \"{}\"\ntoken_file = \"{}\"\n",
        rig.path("ca.pem").display(),
        rig.path("token").display()
    )
}

/// Makes `~/.hostkeeper` (0700) and the settings file in it (0600) holding `text`.
fn write_settings(rig: &Rig, text: &str) {
    std::fs::create_dir(dir_of(rig)).expect("the directory");
    std::fs::set_permissions(dir_of(rig), std::fs::Permissions::from_mode(0o700)).expect("mode");
    std::fs::write(file_of(rig), text).expect("the file");
    std::fs::set_permissions(file_of(rig), std::fs::Permissions::from_mode(0o600)).expect("mode");
}

/// The permission bits of `path`.
fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777
}

/// `hosts list` with no flag at all.
fn list(rig: &Rig) -> Ran {
    rig.hk(&args(&["hosts", "list"]))
}

/// Proves: a call with no flag takes the endpoint, the CA file and the token file of the settings
/// file: it posts to the endpoint the file names, with the file's token as the bearer, and prints
/// the answer.
#[test]
fn the_settings_file_gives_a_call_its_three_flags() {
    let rig = Rig::new("settings-defaults");
    let server = Server::canned(&rig.pki, 200, NO_HOSTS);
    write_settings(&rig, &settings_for(&rig, server.port));
    let ran = list(&rig);
    assert_eq!(
        ran,
        Ran {
            code: 0,
            stdout: format!("{NO_HOSTS}\n"),
            stderr: String::new()
        }
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].target, "/hostkeeper.operator.v1.Hosts/List");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

/// Proves: a flag on the command line wins over the file, for each of the three: the file names
/// a closed port, a CA file and a token file that are not there, and one flag at a time replaces
/// its key (before the service, and after the method), so that the call reaches the server.
#[test]
fn a_flag_beats_the_file_key_it_names() {
    let rig = Rig::new("settings-flag-wins");
    let server = Server::canned(&rig.pki, 200, NO_HOSTS);
    let wrong = format!(
        "endpoint = \"https://127.0.0.1:{}\"\nca_file = \"{}\"\ntoken_file = \"{}\"\n",
        closed_port(),
        rig.path("no-ca.pem").display(),
        rig.path("no-token").display()
    );
    write_settings(&rig, &wrong);
    let endpoint = format!("https://127.0.0.1:{}", server.port);
    let ca = rig.path("ca.pem").display().to_string();
    let token = rig.path("token").display().to_string();
    // The file is right for two keys and wrong for the one the flag names.
    let right = settings_for(&rig, server.port);
    let cases: [(&[&str], &str); 3] = [
        (&["--endpoint", &endpoint], "endpoint"),
        (&["--ca-file", &ca], "ca_file"),
        (&["--token-file", &token], "token_file"),
    ];
    for (flags, key) in cases {
        let mut text = String::new();
        for line in right.lines() {
            let broken = wrong.lines().find(|l| l.starts_with(key)).expect("the key");
            text.push_str(if line.starts_with(key) { broken } else { line });
            text.push('\n');
        }
        std::fs::write(file_of(&rig), &text).expect("the file");
        let mut all = args(flags);
        all.extend(args(&["hosts", "list"]));
        let ran = rig.hk(&all);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{flags:?}");
        // Given after the method it wins as well.
        let mut after = args(&["hosts", "list"]);
        after.extend(args(flags));
        let ran = rig.hk(&after);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{flags:?} after");
    }
    // All three flags win over a file that is wrong in all three.
    std::fs::write(file_of(&rig), &wrong).expect("the file");
    let ran = rig.hk(&crate::rig::call(&rig, server.port, &["hosts", "list"]));
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(server.seen().len(), 7);
}

/// Proves: a key that neither the command line nor the file gives leaves the flag required, with
/// the usage error a run with no file says: no file; a file with none of the keys; one with only
/// the endpoint; one with the endpoint and the CA file.
#[test]
fn a_key_absent_from_both_is_the_usage_error_of_a_run_without_a_file() {
    let rig = Rig::new("settings-absent");
    let none = list(&rig);
    assert_eq!(
        (none.code, none.stderr.as_str()),
        (2, "hk: a call needs --endpoint\n")
    );
    let server = Server::canned(&rig.pki, 200, NO_HOSTS);
    let full = settings_for(&rig, server.port);
    let lines: Vec<&str> = full.lines().collect();
    write_settings(&rig, "");
    let empty = list(&rig);
    assert_eq!(
        (empty.code, empty.stderr.as_str()),
        (2, "hk: a call needs --endpoint\n")
    );
    std::fs::write(file_of(&rig), format!("{}\n", lines[0])).expect("the file");
    assert_eq!(
        (list(&rig).code, list(&rig).stderr.as_str()),
        (2, "hk: a call needs --ca-file\n")
    );
    std::fs::write(file_of(&rig), format!("{}\n{}\n", lines[0], lines[1])).expect("the file");
    assert_eq!(
        (list(&rig).code, list(&rig).stderr.as_str()),
        (
            2,
            "hk: a call needs exactly one of --token-file, --token-stdin and \
             --github-oidc-audience\n"
        )
    );
    assert!(server.seen().is_empty());
}

/// Proves: the file's `token_file` is the default only when no other credential is given:
/// `--token-stdin` sends the token of stdin though the file names a token file that is not
/// there.
#[test]
fn the_files_token_file_gives_way_to_another_credential() {
    let rig = Rig::new("settings-token-stdin");
    let server = Server::canned(&rig.pki, 200, NO_HOSTS);
    let text = settings_for(&rig, server.port).replace(
        &rig.path("token").display().to_string(),
        &rig.path("no-token").display().to_string(),
    );
    write_settings(&rig, &text);
    let ran = rig.run(
        Opts::real().stdin(format!("{TOKEN}\n").as_bytes()),
        &args(&["--token-stdin", "hosts", "list"]),
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(
        server.seen()[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

/// Proves: the ssh commands take the three values from the file: the coordinator they hand the
/// ssh module is the file's; and a command line that gives all three never reads the file (one a
/// symlink would be refused).
#[test]
fn the_ssh_commands_take_the_file_and_a_full_command_line_does_not_read_it() {
    let rig = Rig::new("settings-ssh");
    let server = Server::canned(&rig.pki, 200, "{}");
    write_settings(&rig, &settings_for(&rig, server.port));
    let recorder = Recorder::default();
    let ran = rig.run(
        Opts::real().ssh(&recorder),
        &args(&["ssh-status", "host-7"]),
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    {
        let calls = recorder.calls.lock().expect("the calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].coordinator.endpoint,
            format!("https://127.0.0.1:{}", server.port)
        );
        assert_eq!(calls[0].coordinator.ca_file, rig.path("ca.pem"));
        assert_eq!(calls[0].coordinator.token_file, rig.path("token"));
    }
    // Nothing is missing, so the broken settings are not looked at.
    std::fs::remove_file(file_of(&rig)).expect("removed");
    symlink(rig.path("token"), file_of(&rig)).expect("a link");
    let mut all = rig.globals(server.port);
    all.extend(args(&["ssh-status", "host-7"]));
    let recorder = Recorder::default();
    let ran = rig.run(Opts::real().ssh(&recorder), &all);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let ran = rig.run(
        Opts::real().ssh(&recorder),
        &args(&["ssh-status", "host-7"]),
    );
    assert_eq!(ran.code, 2);
}

/// Proves: `xcode-push` takes the three values from the file: with the file it reaches the check
/// of the token file, which names the file's token file when it may be read by others; with no
/// file it says the three flags are needed.
#[test]
fn xcode_push_takes_the_file() {
    let rig = Rig::new("settings-xcode-push");
    let tool = |name: &str| rig.path(name);
    let mac = Mac {
        os: "macos",
        xcodes: tool("xcodes"),
        pkgutil: tool("pkgutil"),
        xip: tool("xip"),
        plutil: tool("plutil"),
        ..Mac::system()
    };
    let push = || {
        rig.run(
            Opts::real().mac(&mac),
            &args(&["xcode-push", "/nowhere.xip"]),
        )
    };
    let without = push();
    assert_eq!(
        (without.code, without.stderr.as_str()),
        (
            2,
            "hk xcode-push is now hk xcode add\nhk: hk xcode-push needs --endpoint, --ca-file and --token-file\n"
        )
    );
    let server = Server::canned(&rig.pki, 200, "{}");
    let open = rig.put("open-token", b"x\n", 0o644);
    let text =
        settings_for(&rig, server.port).replace(&rig.path("token").display().to_string(), &open);
    write_settings(&rig, &text);
    let with = push();
    assert_eq!(
        (with.code, with.stderr),
        (
            2,
            format!(
                "hk xcode-push is now hk xcode add\nhk: --token-file {open} may be read by its \
                 group or others; make it 0600\n"
            )
        )
    );
}

/// Proves: an unknown key is ignored with exactly `hk: <path>: unknown key <name> ignored` on
/// stderr, one line for each in the file's order of names, and the call goes on with the keys it
/// knows.
#[test]
fn an_unknown_key_is_ignored_with_a_warning() {
    let rig = Rig::new("settings-unknown");
    let server = Server::canned(&rig.pki, 200, NO_HOSTS);
    let text = format!(
        "{}colour = \"red\"\nendpont = 5\n",
        settings_for(&rig, server.port)
    );
    write_settings(&rig, &text);
    let ran = list(&rig);
    let path = file_of(&rig).display().to_string();
    assert_eq!(
        ran,
        Ran {
            code: 0,
            stdout: format!("{NO_HOSTS}\n"),
            stderr: format!(
                "hk: {path}: unknown key colour ignored\nhk: {path}: unknown key endpont ignored\n"
            ),
        }
    );
}

/// Proves: a file that is not TOML, a known key that is not a string and a file that is not text
/// are usage errors naming the file.
#[test]
fn a_file_that_cannot_be_read_as_settings_is_refused_naming_it() {
    let rig = Rig::new("settings-bad-file");
    write_settings(&rig, "endpoint = \n");
    let path = file_of(&rig).display().to_string();
    let ran = list(&rig);
    assert_eq!(ran.code, 2);
    assert!(
        ran.stderr.starts_with(&format!("hk: {path}: ")) && ran.stderr.ends_with('\n'),
        "{}",
        ran.stderr
    );
    std::fs::write(file_of(&rig), "endpoint = 5\n").expect("the file");
    let ran = list(&rig);
    assert_eq!(
        (ran.code, ran.stderr),
        (2, format!("hk: {path}: endpoint must be a string\n"))
    );
    std::fs::write(file_of(&rig), [0xff, 0xfe]).expect("the file");
    let ran = list(&rig);
    assert_eq!(
        (ran.code, ran.stderr),
        (2, format!("hk: {path} is not text\n"))
    );
}

/// Proves: a settings file or `~/.hostkeeper` that is a link, is writable by group or by others,
/// is not the user's own (the run is as another user) or is not a file or a directory is refused
/// with a usage error naming the path, and the call posts nothing.
#[test]
fn a_settings_file_or_directory_that_is_not_the_users_own_is_refused() {
    let server_rig = Rig::new("settings-refuse-server");
    let server = Server::canned(&server_rig.pki, 200, NO_HOSTS);
    let new = |name: &str| {
        let rig = Rig::new(name);
        write_settings(&rig, &settings_for(&rig, server.port));
        rig
    };
    let said = |rig: &Rig, path: &Path, tail: &str| {
        let ran = list(rig);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (2, "", format!("hk: {}{tail}\n", path.display())),
            "{tail}"
        );
    };
    let writable = " may be changed by its group or others; run chmod go-w on it";

    let rig = new("settings-refuse-file-group");
    std::fs::set_permissions(file_of(&rig), std::fs::Permissions::from_mode(0o620)).expect("mode");
    said(&rig, &file_of(&rig), writable);

    let rig = new("settings-refuse-file-others");
    std::fs::set_permissions(file_of(&rig), std::fs::Permissions::from_mode(0o602)).expect("mode");
    said(&rig, &file_of(&rig), writable);

    let rig = new("settings-refuse-dir-group");
    std::fs::set_permissions(dir_of(&rig), std::fs::Permissions::from_mode(0o770)).expect("mode");
    said(&rig, &dir_of(&rig), writable);

    let rig = new("settings-refuse-dir-others");
    std::fs::set_permissions(dir_of(&rig), std::fs::Permissions::from_mode(0o707)).expect("mode");
    said(&rig, &dir_of(&rig), writable);

    // Another owner: the same run as a user that does not own what is there. The directory is
    // looked at first, so it is the one named.
    let rig = new("settings-refuse-owner");
    let owner = std::fs::metadata(dir_of(&rig)).expect("metadata").uid();
    let ran = rig.run(Opts::real().uid(owner + 1), &args(&["hosts", "list"]));
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: {} is not owned by the user running hk (user id {}; its owner is {owner})\n",
                dir_of(&rig).display(),
                owner + 1
            )
        )
    );

    let rig = new("settings-refuse-file-link");
    let real = rig.path("real.toml");
    std::fs::rename(file_of(&rig), &real).expect("moved");
    symlink(&real, file_of(&rig)).expect("a link");
    said(&rig, &file_of(&rig), " is a link; hk does not follow it");

    let rig = new("settings-refuse-dir-link");
    let real = rig.path("real-dir");
    std::fs::rename(dir_of(&rig), &real).expect("moved");
    symlink(&real, dir_of(&rig)).expect("a link");
    said(&rig, &dir_of(&rig), " is a link; hk does not follow it");

    let rig = new("settings-refuse-file-kind");
    std::fs::remove_file(file_of(&rig)).expect("removed");
    std::fs::create_dir(file_of(&rig)).expect("a directory");
    said(&rig, &file_of(&rig), " is not a regular file");

    let rig = new("settings-refuse-dir-kind");
    std::fs::remove_file(file_of(&rig)).expect("removed");
    std::fs::remove_dir(dir_of(&rig)).expect("removed");
    std::fs::write(dir_of(&rig), "").expect("a file");
    said(&rig, &dir_of(&rig), " is not a directory");

    // A home that is a file: nothing under it can be looked at.
    let rig = Rig::new("settings-refuse-home-file");
    let home = rig.path("a-file");
    std::fs::write(&home, "").expect("a file");
    let ran = rig.run(Opts::real().home(&home), &args(&["hosts", "list"]));
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: cannot look at {}: Not a directory (os error 20)\n",
                home.join(".hostkeeper").display()
            )
        )
    );
    assert!(server.seen().is_empty());
}

/// Proves: with no home, or an empty one, or a home with no `~/.hostkeeper`, there are no
/// settings and the usage error is the one of a run without them.
#[test]
fn no_home_means_no_settings() {
    let rig = Rig::new("settings-no-home");
    let expected = (2, "hk: a call needs --endpoint\n".to_owned());
    let ran = rig.run(Opts::real().without_home(), &args(&["hosts", "list"]));
    assert_eq!((ran.code, ran.stderr), expected);
    let ran = rig.run(Opts::real().home(Path::new("")), &args(&["hosts", "list"]));
    assert_eq!((ran.code, ran.stderr), expected);
    let ran = list(&rig);
    assert_eq!((ran.code, ran.stderr), expected);
    // A `~/.hostkeeper` with no settings file in it, as `hk ssh` leaves it.
    std::fs::create_dir(dir_of(&rig)).expect("the directory");
    std::fs::set_permissions(dir_of(&rig), std::fs::Permissions::from_mode(0o700)).expect("mode");
    let ran = list(&rig);
    assert_eq!((ran.code, ran.stderr), expected);
}

/// Proves: a settings file its owner cannot read is a usage error naming it and the system's
/// reason. Root reads any file, so as root the test has nothing to check and says so.
#[test]
fn a_settings_file_that_cannot_be_read_is_refused_naming_it() {
    let rig = Rig::new("settings-unreadable");
    write_settings(&rig, "");
    std::fs::set_permissions(file_of(&rig), std::fs::Permissions::from_mode(0o200)).expect("mode");
    if std::fs::metadata(rig.path("")).expect("metadata").uid() == 0 {
        eprintln!("skipped: the user is root, which reads a file whatever its mode");
        return;
    }
    let ran = list(&rig);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: cannot read {}: Permission denied (os error 13)\n",
                file_of(&rig).display()
            )
        )
    );
}

/// A `settings init` for a coordinator on `port`, with the rig's CA and token.
fn init_words(rig: &Rig, port: u16) -> Vec<String> {
    let mut all = args(&["settings", "init"]);
    all.extend(rig.globals(port));
    all
}

/// Proves: `settings init` calls `Xcode.List` with the token, and then writes the file with mode
/// 0600 holding the three values (the endpoint as given, the paths absolute), after making
/// `~/.hostkeeper` and `~/.hostkeeper/xcode` with mode 0700; the file it wrote supplies the
/// flags of the next call; it says so on one line of stdout.
#[test]
fn settings_init_writes_the_file_after_a_call_that_works() {
    let rig = Rig::new("settings-init");
    let server = Server::canned(&rig.pki, 200, "{}");
    let ran = rig.hk(&init_words(&rig, server.port));
    let path = file_of(&rig);
    assert_eq!(
        ran,
        Ran {
            code: 0,
            stdout: format!(
                "wrote {}; the coordinator answered Xcode.List\n",
                path.display()
            ),
            stderr: String::new()
        }
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].target, "/hostkeeper.operator.v1.Xcode/List");
    assert_eq!(seen[0].text(), "{}");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file"),
        format!(
            "ca_file = \"{}\"\nendpoint = \"https://127.0.0.1:{}\"\ntoken_file = \"{}\"\n",
            rig.path("ca.pem").display(),
            server.port,
            rig.path("token").display()
        )
    );
    assert_eq!(
        (
            mode_of(&dir_of(&rig)),
            mode_of(&dir_of(&rig).join("xcode")),
            mode_of(&path)
        ),
        (0o700, 0o700, 0o600)
    );
    let next = list(&rig);
    assert_eq!((next.code, next.stderr.as_str()), (0, ""));
    assert_eq!(
        server.seen()[1].target,
        "/hostkeeper.operator.v1.Hosts/List"
    );
}

/// Proves: `settings init` does not replace a file that is there (a file, or a link even when it
/// points nowhere): exit 2 naming it, the file as it was, and no call made.
#[test]
fn settings_init_does_not_replace_a_file() {
    let rig = Rig::new("settings-init-twice");
    let server = Server::canned(&rig.pki, 200, "{}");
    assert_eq!(rig.hk(&init_words(&rig, server.port)).code, 0);
    let first = std::fs::read(file_of(&rig)).expect("the file");
    let again = rig.hk(&init_words(&rig, server.port));
    assert_eq!(
        (again.code, again.stdout.as_str(), again.stderr),
        (
            2,
            "",
            format!(
                "hk: {} exists; settings init does not replace it\n",
                file_of(&rig).display()
            )
        )
    );
    assert_eq!(std::fs::read(file_of(&rig)).expect("the file"), first);
    assert_eq!(server.seen().len(), 1);

    std::fs::remove_file(file_of(&rig)).expect("removed");
    symlink(rig.path("nowhere"), file_of(&rig)).expect("a link");
    let again = rig.hk(&init_words(&rig, server.port));
    assert_eq!(again.code, 2);
    assert!(
        again
            .stderr
            .ends_with("; settings init does not replace it\n")
    );
    assert_eq!(server.seen().len(), 1);
}

/// Proves: when the call fails, nothing is written and the failure says so: a refusal (its own
/// lines, exit 3, then the path), an endpoint nothing answers (exit 1) and a CA file that cannot
/// be read (exit 2) each end naming the path that was not written.
#[test]
fn settings_init_writes_nothing_when_the_call_fails() {
    let rig = Rig::new("settings-init-fails");
    let path = file_of(&rig).display().to_string();
    let refused = Server::canned(
        &rig.pki,
        403,
        "{\"code\":\"PERMISSION_DENIED\",\"reason\":\"no\"}",
    );
    let ran = rig.hk(&init_words(&rig, refused.port));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            3,
            "",
            format!(
                "hk: the coordinator answered PERMISSION_DENIED\nrefused: no\nhk: {path} was not written\n"
            )
        )
    );
    let gone = closed_port();
    let ran = rig.hk(&init_words(&rig, gone));
    assert_eq!(ran.code, 1);
    assert!(
        ran.stderr
            .starts_with(&format!("hk: no answer from https://127.0.0.1:{gone} ("))
            && ran
                .stderr
                .ends_with(&format!("); {path} was not written\n")),
        "{}",
        ran.stderr
    );
    let mut no_ca = init_words(&rig, refused.port);
    let at = no_ca.iter().position(|a| a == "--ca-file").expect("a flag");
    no_ca[at + 1] = rig.path("no-ca.pem").display().to_string();
    let ran = rig.hk(&no_ca);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: cannot read --ca-file {}; {path} was not written\n",
                rig.path("no-ca.pem").display()
            )
        )
    );
    assert!(!file_of(&rig).exists());
}

/// Proves: the flags of `settings init` are checked before anything is made or called: all three
/// are needed, the endpoint must be https, the two paths must be absolute (the file keeps them)
/// and text, no other credential is taken, and an unknown flag, no word and another word are
/// usage errors.
#[test]
fn settings_init_checks_its_flags_first() {
    let rig = Rig::new("settings-init-flags");
    let server = Server::canned(&rig.pki, 200, "{}");
    let with = |edit: &dyn Fn(&mut Vec<String>)| {
        let mut all = init_words(&rig, server.port);
        edit(&mut all);
        rig.hk(&all)
    };
    let replace = |all: &mut Vec<String>, flag: &str, value: &str| {
        let at = all.iter().position(|a| a == flag).expect("a flag");
        all[at + 1] = value.to_owned();
    };
    let usage = |ran: Ran, message: &str| {
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (2, "", format!("hk: {message}\n"))
        );
    };
    usage(
        rig.hk(&args(&["settings", "init"])),
        "hk settings init needs --endpoint, --ca-file and --token-file",
    );
    usage(
        with(&|all| replace(all, "--endpoint", "http://127.0.0.1:1")),
        "--endpoint takes https://<host>[:<port>]",
    );
    usage(
        with(&|all| replace(all, "--ca-file", "ca.pem")),
        "--ca-file is kept in the settings file, so it takes an absolute path",
    );
    usage(
        with(&|all| replace(all, "--token-file", "token")),
        "--token-file is kept in the settings file, so it takes an absolute path",
    );
    usage(
        with(&|all| all.push("--token-stdin".to_owned())),
        "hk settings init takes --token-file only: --token-stdin, --github-oidc-audience and \
         --response-file are not for it",
    );
    for flags in [
        &["--github-oidc-audience", "aud"][..],
        &["--response-file", "/answer"][..],
    ] {
        usage(
            with(&|all| all.extend(args(flags))),
            "hk settings init takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it",
        );
    }
    usage(
        with(&|all| all.push("--bogus".to_owned())),
        "hk settings has no flag --bogus",
    );
    usage(
        rig.hk(&args(&["settings", "other"])),
        "settings takes the one word init; 'hk settings --help' says what it does",
    );
    assert!(!dir_of(&rig).exists());
    assert!(server.seen().is_empty());

    // A path that is not text cannot be kept in a file of text.
    let mut os: Vec<OsString> = init_words(&rig, server.port)
        .into_iter()
        .map(OsString::from)
        .collect();
    let at = os.iter().position(|a| a == "--ca-file").expect("a flag");
    os[at + 1] = <OsString as std::os::unix::ffi::OsStringExt>::from_vec(b"/ca\xff.pem".to_vec());
    let ran = rig.run_os(Opts::real(), &os);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            "hk: --ca-file is kept in the settings file as text\n".to_owned()
        )
    );

    let bare = rig.hk(&args(&["settings"]));
    assert_eq!((bare.code, bare.stdout.as_str()), (2, ""));
    assert!(bare.stderr.starts_with("hk settings init "));
    let help = rig.hk(&args(&["settings", "--help"]));
    assert_eq!((help.code, help.stderr.as_str()), (0, ""));
    assert_eq!(help.stdout.trim_end(), bare.stderr.trim_end());
}

/// Proves: `settings init` fails, with nothing made, when there is no home, when the home cannot
/// hold `~/.hostkeeper`, and when the table has no `Xcode.List`; and refuses a `~/.hostkeeper`
/// that others may write, naming it, as a read does.
#[test]
fn settings_init_needs_a_home_and_a_method_to_call() {
    let rig = Rig::new("settings-init-env");
    let server = Server::canned(&rig.pki, 200, "{}");
    let words = init_words(&rig, server.port);
    let ran = rig.run(Opts::real().without_home(), &words);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            1,
            "hk: hk settings init keeps its file under $HOME, which is not set\n".to_owned()
        )
    );
    let missing = rig.path("no-such-home");
    let ran = rig.run(Opts::real().home(&missing), &words);
    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stderr,
        format!(
            "hk: cannot make {}: No such file or directory (os error 2)\n",
            missing.join(".hostkeeper").display()
        )
    );
    let ran = rig.run(Opts::fixture(), &words);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            1,
            "hk: this hk has no Xcode.List method to try the settings with\n".to_owned()
        )
    );
    std::fs::create_dir(dir_of(&rig)).expect("the directory");
    std::fs::set_permissions(dir_of(&rig), std::fs::Permissions::from_mode(0o775)).expect("mode");
    let ran = rig.hk(&words);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: {} may be changed by its group or others; run chmod go-w on it\n",
                dir_of(&rig).display()
            )
        )
    );
    assert!(server.seen().is_empty());
}

/// Proves: the file is made with no replacing and no guessing at the last step: when another
/// process makes the file while the call is made, `settings init` leaves it as it is and refuses,
/// and when the directory is gone it fails naming the path it could not make.
#[test]
fn settings_init_does_not_replace_a_file_made_while_it_called() {
    let rig = Rig::new("settings-init-race");
    let file = file_of(&rig);
    let made = file.clone();
    let server = Server::serve(
        &rig.pki,
        || (),
        move |(), _| {
            std::fs::write(&made, "theirs").expect("their file");
            Reply::new(200, "{}")
        },
    );
    let ran = rig.hk(&init_words(&rig, server.port));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!("hk: {} exists; it is not replaced\n", file.display())
        )
    );
    assert_eq!(std::fs::read_to_string(&file).expect("the file"), "theirs");

    let rig = Rig::new("settings-init-race-gone");
    let gone = dir_of(&rig);
    let server = Server::serve(
        &rig.pki,
        || (),
        move |(), _| {
            std::fs::remove_dir_all(&gone).expect("removed");
            Reply::new(200, "{}")
        },
    );
    let ran = rig.hk(&init_words(&rig, server.port));
    assert_eq!(
        (ran.code, ran.stderr),
        (
            1,
            format!(
                "hk: cannot make {}: No such file or directory (os error 2)\n",
                dir_of(&rig).join("hk.toml").display()
            )
        )
    );
    assert!(!dir_of(&rig).exists());
}

/// Proves: a `~/.hostkeeper` that is there and private keeps the mode it has (0750), and
/// `settings init` makes only what is missing in it (`xcode`, 0700); an `xcode` that is a link
/// is refused naming it.
#[test]
fn settings_init_keeps_the_mode_of_a_directory_that_is_there() {
    let rig = Rig::new("settings-init-existing");
    let server = Server::canned(&rig.pki, 200, "{}");
    std::fs::create_dir(dir_of(&rig)).expect("the directory");
    std::fs::set_permissions(dir_of(&rig), std::fs::Permissions::from_mode(0o750)).expect("mode");
    symlink(rig.path("elsewhere"), dir_of(&rig).join("xcode")).expect("a link");
    let ran = rig.hk(&init_words(&rig, server.port));
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: {} is a link; hk does not follow it\n",
                dir_of(&rig).join("xcode").display()
            )
        )
    );
    std::fs::remove_file(dir_of(&rig).join("xcode")).expect("removed");
    let ran = rig.hk(&init_words(&rig, server.port));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        (
            mode_of(&dir_of(&rig)),
            mode_of(&dir_of(&rig).join("xcode")),
            mode_of(&file_of(&rig))
        ),
        (0o750, 0o700, 0o600)
    );
}

/// The built `hk settings init`, run under the umask `umask` (the shell sets it, then runs the
/// program in its place, so no other test is affected): the files and directories it made.
fn init_under_umask(name: &str, umask: &str) -> (Rig, Output2) {
    let rig = Rig::new(name);
    let server = Server::canned(&rig.pki, 200, "{}");
    let empty = rig.path("empty-path");
    std::fs::create_dir(&empty).expect("a directory");
    let _alone = crate::rig::alone();
    let out = Command::new("/bin/sh")
        .args(["-c", &format!("umask {umask}; exec \"$0\" \"$@\"")])
        .arg(env!("CARGO_BIN_EXE_hk"))
        .args(init_words(&rig, server.port))
        .env_clear()
        .envs(crate::process::coverage_env())
        .env("HOME", rig.path("home"))
        .env("PATH", &empty)
        .output()
        .expect("hk runs");
    let ran = Output2 {
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    (rig, ran)
}

/// What a child process ended with.
struct Output2 {
    code: Option<i32>,
    stderr: String,
}

/// Proves: the program (its own `main`, with the user id it reads from the system and `$HOME`)
/// makes `~/.hostkeeper` and `~/.hostkeeper/xcode` 0700 and the file 0600 under a umask of 000,
/// and under a umask of 277 that would take the owner's write bit away from what is made.
#[test]
fn the_modes_are_the_same_under_any_umask() {
    for umask in ["000", "277"] {
        let (rig, ran) = init_under_umask(&format!("settings-umask-{umask}"), umask);
        assert_eq!((ran.code, ran.stderr.as_str()), (Some(0), ""), "{umask}");
        assert_eq!(
            (
                mode_of(&dir_of(&rig)),
                mode_of(&dir_of(&rig).join("xcode")),
                mode_of(&file_of(&rig))
            ),
            (0o700, 0o700, 0o600),
            "{umask}"
        );
    }
}
