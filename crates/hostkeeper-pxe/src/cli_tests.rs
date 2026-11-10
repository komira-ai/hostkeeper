//! The command line: what parses, the exit status and the one line of each outcome, the
//! settings file expanded into the flags of `serve`, and each step verb reaching its step.

use std::cell::RefCell;
use std::fs;

use hostkeeper_hook::Step;

use super::*;
use crate::hook::tests::kit::{FakeSystemctl, Setup, unreached};
use crate::hook::{FetchJob, Host};
use crate::seal::Seal;

/// Runs the command line `args` (after the program name) through `serve` and the hook `host`;
/// the exit status, standard output and standard error.
fn run_with(args: &[&str], serve: Serve<'_>, host: &Host<'_>) -> (u8, String, String) {
    let mut argv: Vec<OsString> = vec!["pxe".into()];
    argv.extend(args.iter().map(OsString::from));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = run(argv, serve, host, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("UTF-8"),
        String::from_utf8(err).expect("UTF-8"),
    )
}

/// A serve the test does not expect to be reached.
fn unreached_serve(_: &ServeFlags) -> Result<(), String> {
    panic!("serve is not reached")
}

/// Runs `args` where `serve` is not reached and a step uses the fake `systemctl` of `codes`.
fn run_step(args: &[&str], owner: u32, codes: &[i32]) -> (u8, String, String) {
    let systemctl = FakeSystemctl::answering(codes);
    let fetch: &dyn Fn(&FetchJob, u32) -> Result<Seal, String> = &unreached;
    let host = Host {
        owner,
        systemctl: &systemctl,
        fetch,
    };
    run_with(args, &unreached_serve, &host)
}

/// A command line that does not parse exits 2 and says why on standard error: no verb, the
/// verb `fetch` that is gone, the bare step verbs that are gone (a step is `update-hook <step>`),
/// a mode that is not one. Nothing is reached.
///
/// Catches the old `fetch` verb or a bare step verb still answering.
#[test]
fn a_command_line_that_does_not_parse_exits_2() {
    let cases: [(Vec<&str>, &str); 5] = [
        (vec![], "Usage: pxe <COMMAND>"),
        (
            vec!["fetch", "--release", "https://r.example"],
            "unrecognized subcommand 'fetch'",
        ),
        (
            vec!["prepare", "/staged", "--params", "p"],
            "unrecognized subcommand 'prepare'",
        ),
        (
            vec!["uninstall", "--params", "p"],
            "unrecognized subcommand 'uninstall'",
        ),
        (
            vec!["serve", "--mode", "both"],
            "invalid value 'both' for '--mode <MODE>'",
        ),
    ];
    for (args, text) in cases {
        let (status, out, err) = run_step(&args, 0, &[]);
        assert_eq!(status, 2, "{args:?}");
        assert!(out.is_empty(), "{args:?}");
        assert!(err.contains(text), "{args:?}: {err}");
    }
}

/// An `update-hook` command line the hook contract cannot read exits 2 with the one line
/// `pxe update-hook: <why>` on standard error and nothing on standard output; no step runs.
///
/// Catches a step run without its params file, a prepare without its staged release, and a
/// parse refusal that exits 1 or prints the contract's report.
#[test]
fn an_update_hook_command_line_that_cannot_be_read_exits_2() {
    let cases: [(Vec<&str>, &str); 7] = [
        (vec!["update-hook"], "pxe update-hook: no step given\n"),
        (
            vec!["update-hook", "fetch", "--params", "p"],
            "pxe update-hook: unknown step \"fetch\"\n",
        ),
        (
            vec!["update-hook", "prepare", "--params", "p"],
            "pxe update-hook: prepare needs the staged release directory\n",
        ),
        (
            vec!["update-hook", "cutover"],
            "pxe update-hook: the step needs --params <file>\n",
        ),
        (
            vec!["update-hook", "verify", "--params"],
            "pxe update-hook: --params needs a file\n",
        ),
        (
            vec!["update-hook", "verify", "--params", "p", "--params", "q"],
            "pxe update-hook: --params given twice\n",
        ),
        (
            vec!["update-hook", "verify", "--mode", "proxy", "--params", "p"],
            "pxe update-hook: unexpected argument \"--mode\"\n",
        ),
    ];
    for (args, line) in cases {
        let (status, out, err) = run_step(&args, 0, &[]);
        assert_eq!(
            (status, out.as_str(), err.as_str()),
            (2, "", line),
            "{args:?}"
        );
    }
}

/// `--help` and `--version` go to standard output and exit 0; the help names every verb and not
/// `fetch`, and the version is `pxe <version>`.
///
/// Catches help printed as an error and a program that still calls itself hostkeeper-pxe.
#[test]
fn help_and_version_go_to_standard_output() {
    let (status, out, err) = run_step(&["--help"], 0, &[]);
    assert_eq!((status, err.as_str()), (0, ""));
    for verb in ["serve", "update-hook"] {
        assert!(out.contains(&format!("  {verb} ")), "{verb}: {out}");
    }
    assert!(!out.contains("fetch"), "{out}");
    let (status, out, err) = run_step(&["--version"], 0, &[]);
    assert_eq!(
        (status, out, err.as_str()),
        (0, format!("pxe {}\n", env!("CARGO_PKG_VERSION")), "")
    );
}

/// The program itself runs a step with root as the owner it trusts and `/usr/bin/systemctl`:
/// a params file that is not there is refused before anything is touched. (Its exit status and
/// line are asserted on the binary, in `tests/binary.rs`.)
#[test]
fn the_program_runs_the_verb() {
    let setup = Setup::new("main");
    let missing = setup.path("missing.toml");
    let _ = main(vec![
        "pxe".into(),
        "update-hook".into(),
        "uninstall".into(),
        "--params".into(),
        missing.into(),
    ]);
    assert!(!setup.boot.exists());
}

/// `serve --help` lists every flag of `serve`; `serve` without `--mode` exits 2 naming it, and
/// the serve is not reached; a refused serve prints one line, `pxe serve: <why>`, and exits 1;
/// a serve that stops prints nothing and exits 0.
///
/// Catches a serve that starts in a mode the operator did not name, and a refusal that exits 0.
#[test]
fn serve_takes_its_flags_and_reports_its_refusal() {
    let (status, out, _) = run_step(&["serve", "--help"], 0, &[]);
    assert_eq!(status, 0);
    for flag in [
        "--mode",
        "--address",
        "--interface",
        "--dhcp-listen",
        "--proxy-listen",
        "--tftp-listen",
        "--http-listen",
        "--dhcp-reply-to",
        "--coordinator",
        "--ca-file",
        "--token-file",
        "--dir",
        "--daemon-config",
        "--ssh-authorized-keys",
        "--install-disk",
        "--serial-console",
        "--repositories",
        "--reinstall",
        "--user-id",
        "--group-id",
        "--tftp-timeout-ms",
        "--settings",
        "--notify-systemd",
    ] {
        assert!(out.contains(flag), "{flag}: {out}");
    }
    let required = [
        "serve",
        "--address",
        "192.0.2.10",
        "--coordinator",
        "https://192.0.2.1:8444",
        "--ca-file",
        "ca.pem",
        "--token-file",
        "token",
        "--dir",
        "boot",
        "--daemon-config",
        "c.toml",
        "--ssh-authorized-keys",
        "keys",
        "--user-id",
        "1000",
        "--group-id",
        "1000",
    ];
    let (status, out, err) = run_step(&required, 0, &[]);
    assert_eq!((status, out.as_str()), (2, ""));
    assert!(err.contains("--mode <MODE>"), "{err}");
    let with_mode = [&required[..], &["--mode", "router"]].concat();
    let systemctl = FakeSystemctl::default();
    let host = Host {
        owner: 0,
        systemctl: &systemctl,
        fetch: &unreached,
    };
    let refused = |flags: &ServeFlags| {
        assert_eq!(flags.mode, ModeFlag::Router);
        assert!(!flags.notify_systemd);
        assert_eq!(flags.settings, None);
        assert_eq!(flags.repositories, None);
        Err("the coordinator could not be read".to_owned())
    };
    let (status, out, err) = run_with(&with_mode, &refused, &host);
    assert_eq!((status, out.as_str()), (1, ""));
    assert_eq!(err, "pxe serve: the coordinator could not be read\n");
    let (status, out, err) = run_with(&with_mode, &|_| Ok(()), &host);
    assert_eq!((status, out.as_str(), err.as_str()), (0, "", ""));
}

/// `serve --settings <file>` reads the `[serve]` table as flags after the command line's: the
/// file may hold every flag, the command line adds `--dir`, `--settings=<file>` is the same,
/// a `[prepare]` table in the file is accepted, and a flag given in both the line and the file is
/// refused by the parser (exit 2).
///
/// Catches a settings file read but not obeyed, and a flag in two places silently taking one.
#[test]
fn serve_reads_its_flags_from_the_settings_file() {
    let setup = Setup::new("serve-settings");
    let settings = setup.settings.display().to_string();
    let systemctl = FakeSystemctl::default();
    let host = Host {
        owner: setup.owner,
        systemctl: &systemctl,
        fetch: &unreached,
    };
    let seen = RefCell::new(Vec::new());
    let record = |flags: &ServeFlags| {
        seen.borrow_mut().push((
            flags.mode,
            flags.address,
            flags.user_id,
            flags.dir.clone(),
            flags.settings.clone(),
            flags.notify_systemd,
        ));
        Ok(())
    };
    let (status, out, err) = run_with(
        &[
            "serve",
            "--notify-systemd",
            "--settings",
            &settings,
            "--dir",
            "/var/lib/pxe/current",
        ],
        &record,
        &host,
    );
    assert_eq!((status, out.as_str(), err.as_str()), (0, "", ""));
    let equals = format!("--settings={settings}");
    let (status, _, _) = run_with(&["serve", &equals, "--dir", "d"], &record, &host);
    assert_eq!(status, 0);
    let wanted = |settings: &str, notify: bool, dir: &str| {
        (
            ModeFlag::Router,
            Ipv4Addr::new(192, 0, 2, 10),
            65534,
            PathBuf::from(dir),
            Some(PathBuf::from(settings)),
            notify,
        )
    };
    assert_eq!(
        *seen.borrow(),
        [
            wanted(&settings, true, "/var/lib/pxe/current"),
            wanted(&settings, false, "d")
        ]
    );
    for twice in [
        vec![
            "serve",
            "--settings",
            &settings,
            "--mode",
            "proxy",
            "--dir",
            "d",
        ],
        vec![
            "serve",
            "--settings",
            &settings,
            "--dir",
            "d",
            "--user-id",
            "1",
        ],
    ] {
        let (status, out, err) = run_with(&twice, &unreached_serve, &host);
        assert_eq!((status, out.as_str()), (2, ""), "{twice:?}");
        assert!(err.contains("cannot be used multiple times"), "{err}");
    }
    let (status, _, err) = run_with(
        &["serve", "--dir", "d", "--settings"],
        &unreached_serve,
        &host,
    );
    assert_eq!(status, 2, "{err}");
    let (status, _, err) = run_with(
        &[
            "update-hook",
            "verify",
            "--params",
            "p",
            "--settings",
            &settings,
        ],
        &unreached_serve,
        &host,
    );
    assert_eq!(status, 2, "{err}");
    assert_eq!(err, "pxe update-hook: unexpected argument \"--settings\"\n");
}

/// A settings file that cannot be read, is not the owner's, or is not a settings file refuses
/// the `serve` with one line `pxe serve: <path>: <why>` and exit 1; `serve` is not reached.
///
/// Catches a serve that starts on settings anyone could have written.
#[test]
fn serve_refuses_a_settings_file_it_cannot_trust() {
    let setup = Setup::new("serve-bad-settings");
    let settings = setup.settings.display().to_string();
    let systemctl = FakeSystemctl::default();
    let host = Host {
        owner: setup.owner + 1,
        systemctl: &systemctl,
        fetch: &unreached,
    };
    let (status, out, err) = run_with(
        &["serve", "--settings", &settings, "--dir", "d"],
        &unreached_serve,
        &host,
    );
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(
        err.starts_with(&format!("pxe serve: {settings}: ")),
        "{err}"
    );
    let host = Host {
        owner: setup.owner,
        systemctl: &systemctl,
        fetch: &unreached,
    };
    setup.write_settings("[prepare]\n");
    let (status, _, err) = run_with(
        &["serve", "--settings", &settings, "--dir", "d"],
        &unreached_serve,
        &host,
    );
    assert_eq!(status, 1);
    assert_eq!(
        err,
        format!("pxe serve: {settings}: there is no [serve] table\n")
    );
}

/// Each step reaches its code through `update-hook`: the step's name is on its refusal,
/// `pxe <step>: <text>`; `prepare` takes the staged release first; a deferred step exits 75 with
/// its report on standard output; a step that succeeds prints nothing.
///
/// Catches a step wired to another step's code.
#[test]
fn each_update_hook_step_reaches_its_step() {
    let setup = Setup::new("verbs");
    let missing = setup.path("missing.toml").display().to_string();
    for (args, step) in [
        (
            vec!["update-hook", "prepare", "/staged", "--params", &missing],
            Step::Prepare,
        ),
        (
            vec!["update-hook", "cutover", "--params", &missing],
            Step::Cutover,
        ),
        (
            vec!["update-hook", "verify", "--params", &missing],
            Step::Verify,
        ),
        (
            vec!["update-hook", "rollback", "--params", &missing],
            Step::Rollback,
        ),
        (
            vec!["update-hook", "cleanup", "--params", &missing],
            Step::Cleanup,
        ),
        (
            vec!["update-hook", "uninstall", "--params", &missing],
            Step::Uninstall,
        ),
    ] {
        let (status, out, err) = run_step(&args, setup.owner, &[]);
        assert_eq!(status, 1, "{args:?}");
        assert!(
            err.starts_with(&format!("pxe {step}: {missing}: ")),
            "{err}"
        );
        assert!(out.contains("\"reason\":\"bad_params\""), "{out}");
    }
    let params = setup.params(None, None, "").display().to_string();
    let verify = ["update-hook", "verify", "--params", &params];
    let (status, out, err) = run_step(&verify, setup.owner, &[]);
    assert_eq!((status, out.as_str(), err.as_str()), (0, "", ""));
    let (status, out, err) = run_step(&verify, setup.owner, &[3]);
    assert_eq!(status, 75);
    assert_eq!(err, "pxe verify: pxe.service is not active\n");
    assert_eq!(
        out,
        "{\"class\":\"environment\",\"detail\":\"pxe.service is not active\",\"promotion_pending\":false,\"reason\":\"not_active\",\"retry_after_secs\":2}\n"
    );
    fs::remove_file(setup.path("params.toml")).expect("remove");
}

/// `prepare`'s staged release must be the params' `updater.staged_release` byte for byte: one
/// that differs, or a params file without the key, is refused as `bad_params` before anything is
/// done, and the one that equals it goes on to the step (here to the settings, which are the
/// owner's alone).
///
/// Catches a prepare that runs from a directory the updater did not stage.
#[test]
fn prepare_must_be_given_the_params_staged_release() {
    let setup = Setup::new("staged");
    let staged = "/var/lib/hostkeeper/packages/pxe/1.2.3";
    let named = || {
        setup
            .params(
                Some("1.2.3"),
                None,
                &format!("\"updater.staged_release\" = \"{staged}\"\n"),
            )
            .display()
            .to_string()
    };
    let run = |given: &str, params: &str, owner: u32| {
        run_step(
            &["update-hook", "prepare", given, "--params", params],
            owner,
            &[],
        )
    };
    let (status, out, err) = run(
        "/var/lib/hostkeeper/packages/pxe/9.9.9",
        &named(),
        setup.owner,
    );
    assert_eq!(status, 1);
    assert_eq!(
        err,
        "pxe prepare: prepare was given /var/lib/hostkeeper/packages/pxe/9.9.9, which is not the params' updater.staged_release\n"
    );
    assert!(out.contains("\"reason\":\"bad_params\""), "{out}");
    let bare = setup.params(Some("1.2.3"), None, "").display().to_string();
    let (status, _, err) = run(staged, &bare, setup.owner);
    assert_eq!(status, 1);
    assert!(
        err.contains("which is not the params' updater.staged_release"),
        "{err}"
    );
    let (status, _, err) = run(staged, &named(), setup.owner + 1);
    assert_eq!(status, 1);
    assert!(
        err.starts_with(&format!("pxe prepare: {}: ", setup.settings.display())),
        "{err}"
    );
}

/// A `[serve]` key `repositories` is main's `--repositories` flag: the directory of the
/// installed hosts' repository entries, given once in the settings file, reaches `serve`.
///
/// Catches a settings file whose `repositories` is dropped or refused as an unknown flag.
#[test]
fn serve_reads_repositories_from_the_settings_file() {
    let setup = Setup::new("serve-repositories");
    let settings = setup.settings.display().to_string();
    let text = fs::read_to_string(&setup.settings).expect("the settings");
    setup.write_settings(&text.replace(
        "[prepare]",
        "repositories = \"/etc/pxe/repositories\"\n[prepare]",
    ));
    let systemctl = FakeSystemctl::default();
    let host = Host {
        owner: setup.owner,
        systemctl: &systemctl,
        fetch: &unreached,
    };
    let seen = RefCell::new(None);
    let record = |flags: &ServeFlags| {
        *seen.borrow_mut() = Some(flags.repositories.clone());
        Ok(())
    };
    let (status, _, err) = run_with(
        &["serve", "--settings", &settings, "--dir", "d"],
        &record,
        &host,
    );
    assert_eq!((status, err.as_str()), (0, ""));
    assert_eq!(
        *seen.borrow(),
        Some(Some(PathBuf::from("/etc/pxe/repositories")))
    );
}

/// `parse_serve_line` reads a `serve` line with the program's parser: the settings file, the
/// directory and the readiness flag come back, a line the parser refuses is its refusal on one
/// line, and a line of another verb is not a `serve` line.
///
/// Catches a unit check that accepts a line the program would refuse.
#[test]
fn a_serve_line_is_read_with_the_programs_parser() {
    let words = |line: &[&str]| line.iter().map(OsString::from).collect::<Vec<_>>();
    let all = [
        "--mode",
        "router",
        "--address",
        "192.0.2.10",
        "--coordinator",
        "https://192.0.2.1:8444",
        "--ca-file",
        "ca",
        "--token-file",
        "token",
        "--daemon-config",
        "config",
        "--ssh-authorized-keys",
        "keys",
        "--user-id",
        "1",
        "--group-id",
        "1",
    ];
    assert_eq!(
        parse_serve_line(
            &words(&[
                "serve",
                "--notify-systemd",
                "--settings",
                "s.toml",
                "--dir",
                "d"
            ]),
            &all
        ),
        Ok(ServeLine {
            settings: Some(PathBuf::from("s.toml")),
            dir: PathBuf::from("d"),
            notify_systemd: true,
        })
    );
    assert_eq!(
        parse_serve_line(&words(&["serve", "--dir", "d", "--archive", "x"]), &all),
        Err("unexpected argument '--archive' found".to_owned())
    );
    assert_eq!(
        parse_serve_line(&words(&["serve", "--dir", "d"]), &[]).map(|l| l.dir),
        Err("the following required arguments were not provided: --mode <MODE> --address <ADDRESS> --coordinator <COORDINATOR> --ca-file <CA_FILE> --token-file <TOKEN_FILE> --daemon-config <DAEMON_CONFIG> --ssh-authorized-keys <SSH_AUTHORIZED_KEYS> --user-id <USER_ID> --group-id <GROUP_ID>".to_owned())
    );
    assert_eq!(
        parse_serve_line(&words(&["update-hook", "verify"]), &[]),
        Err("not a serve line".to_owned())
    );
}

/// The release URL of the params is `https://` with no query or fragment.
///
/// Catches a release URL that could carry a query into the downloads.
#[test]
fn the_release_url_is_https_without_query_or_fragment() {
    assert_eq!(
        release_url("https://r.example/v1"),
        Ok("https://r.example/v1".to_owned())
    );
    for bad in [
        "",
        "https://",
        "http://r.example",
        "https://r.example/v1?x=1",
        "https://r.example/#f",
        "https://r.example/a b",
    ] {
        assert_eq!(
            release_url(bad),
            Err("not an https:// URL without a query or fragment".to_owned()),
            "{bad}"
        );
    }
}
