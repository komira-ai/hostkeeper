//! The tests of the flags of `coordinator-update`.

use super::*;

#[test]
fn the_real_pause_sleeps_the_seconds_it_is_given() {
    // Proves: the pause the run uses hands its sleeper exactly the seconds asked, each time, and
    // nothing else (a sleeper that records instead of waiting: no time is measured).
    let asked = std::cell::RefCell::new(Vec::new());
    let pause = Sleep(|d: std::time::Duration| asked.borrow_mut().push(d));
    pause.pause(5);
    pause.pause(0);
    pause.pause(17);
    assert_eq!(
        *asked.borrow(),
        [5, 0, 17].map(std::time::Duration::from_secs)
    );
}

#[derive(clap::Parser)]
struct Wrap {
    #[command(flatten)]
    flags: Flags,
}

fn parse(args: &[&str]) -> Result<Flags, String> {
    let mut all = vec![
        "wrap",
        "--download-dir",
        "/var/lib/d",
        "--coordinator-url",
        "https://c.example.com:8444",
        "--ca-file",
        "/ca.pem",
    ];
    all.extend_from_slice(args);
    Wrap::try_parse_from(all)
        .map(|w| w.flags)
        .map_err(|e| e.to_string())
}

#[test]
fn the_flags_default_to_the_deployment_and_refuse_what_does_not_fit() {
    let f = parse(&[]).expect("the defaults");
    assert_eq!(f.container, "hostkeeper-coordinator");
    assert_eq!(f.docker_socket, PathBuf::from("/var/run/docker.sock"));
    assert_eq!(f.target_version, None);
    assert_eq!(
        (f.ready_checks, f.check_every_secs, f.stop_timeout_secs),
        (60, 5, 30)
    );
    assert_eq!(f.curl, "curl");
    // No token flag: GitHub is read at its public URLs, the attestation checked offline with the
    // pinned gh and trusted root the runbook installs, against the system's CA bundle file.
    assert_eq!(
        (
            f.gh.as_path(),
            f.trusted_root.as_path(),
            f.ca_bundle.as_path(),
            f.github_api_url.as_str(),
            f.github_url.as_str(),
        ),
        (
            Path::new("/usr/local/libexec/hostkeeper/gh"),
            Path::new("/usr/local/lib/hostkeeper/trusted_root.json"),
            Path::new("/etc/ssl/certs/ca-certificates.crt"),
            "https://api.github.com",
            "https://github.com",
        )
    );
    for token in ["--gh-token-file", "--token", "--github-token"] {
        let err = parse(&[token, "/t"]).expect_err("no token flag");
        assert!(err.contains("unexpected argument"), "{token}: {err}");
    }
    let host = if cfg!(target_arch = "aarch64") {
        Platform::LinuxArm64
    } else {
        Platform::LinuxX86_64
    };
    assert_eq!(f.platform, host);

    let f = parse(&[
        "--platform",
        "linux-arm64",
        "--target-version",
        "1.2.3",
        "--container",
        "coord_2",
        "--ready-checks",
        "2",
        "--gh",
        "/opt/gh",
        "--trusted-root",
        "/opt/root.json",
        "--ca-bundle",
        "/opt/ca.pem",
        "--github-api-url",
        "https://127.0.0.1:8443",
        "--github-url",
        "https://[::1]:8443",
    ])
    .expect("flags");
    assert_eq!(
        (
            f.platform,
            f.target_version,
            f.container.as_str(),
            f.ready_checks
        ),
        (Platform::LinuxArm64, Some(v("1.2.3")), "coord_2", 2)
    );
    assert_eq!(
        (
            f.gh.as_path(),
            f.trusted_root.as_path(),
            f.ca_bundle.as_path(),
            f.github_api_url.as_str(),
            f.github_url.as_str(),
        ),
        (
            Path::new("/opt/gh"),
            Path::new("/opt/root.json"),
            Path::new("/opt/ca.pem"),
            "https://127.0.0.1:8443",
            "https://[::1]:8443",
        )
    );

    for (args, why) in [
        (
            vec!["--platform", "macos-arm64"],
            "not a platform of the coordinator's image: \"macos-arm64\"",
        ),
        (
            vec!["--platform", "x"],
            "not a platform of the coordinator's image: \"x\"",
        ),
        (vec!["--target-version", "1.2"], "not a version: \"1.2\""),
        (vec!["--ready-checks", "0"], "0 is not in 1..="),
        (vec!["--container=-x"], "not a container name: \"-x\""),
        (vec!["--container", ""], "not a container name: \"\""),
        (vec!["--container", "a/b"], "not a container name: \"a/b\""),
        (
            vec!["--github-api-url", "http://api.github.com"],
            "not an https://<host>[:<port>] URL",
        ),
        (
            vec!["--github-url", "https://github.com/komira-ai"],
            "not an https://<host>[:<port>] URL",
        ),
    ] {
        let err = parse(&args).expect_err("refused");
        assert!(err.contains(why), "{args:?}: {err}");
    }
    let all = |url: &str| {
        Wrap::try_parse_from([
            "wrap",
            "--download-dir",
            "/d",
            "--coordinator-url",
            url,
            "--ca-file",
            "/c",
        ])
        .map(|w| w.flags.coordinator_url)
        .map_err(|e| e.to_string())
    };
    for good in [
        "https://c.example.com",
        "https://c.example.com:8444",
        "https://192.0.2.5:443",
        "https://[::1]:8444",
        "https://[::1]",
        "https://[2001:db8::5:1]",
        "https://[2001:db8::5:1]:8444",
    ] {
        assert_eq!(all(good).as_deref(), Ok(good));
    }
    for bad in [
        "http://c.example.com:8444",
        "c.example.com:8444",
        "https://",
        "https://c.example.com/",
        "https://c.example.com/v1",
        "https://user@c.example.com",
        "https://c.example.com:0",
        "https://c.example.com:99999",
        "https://c.example.com:",
        "https://:8444",
        "https://c.example.com?x=1",
    ] {
        let err = all(bad).expect_err("refused");
        assert!(
            err.contains("not an https://<host>[:<port>] URL"),
            "{bad}: {err}"
        );
    }
    for missing in ["--download-dir", "--coordinator-url", "--ca-file"] {
        let mut args = vec![
            "wrap",
            "--download-dir",
            "/d",
            "--coordinator-url",
            "https://c.example.com",
            "--ca-file",
            "/c",
        ];
        let at = args.iter().position(|a| *a == missing).expect("flag");
        args.drain(at..at + 2);
        assert!(Wrap::try_parse_from(args).is_err(), "{missing}");
    }
}

#[test]
fn the_error_of_a_file_is_the_error_of_the_run() {
    let e = Error::from(release::Error::Failed("x".to_owned()));
    assert_eq!(e.to_string(), "x");
}
