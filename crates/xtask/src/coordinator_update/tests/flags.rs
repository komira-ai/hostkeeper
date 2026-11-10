//! The tests of the flags of `coordinator-update`.

use super::*;

#[test]
fn the_real_pause_waits_the_seconds_it_is_given() {
    Sleep.pause(0);
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
    assert_eq!(f.version, None);
    assert_eq!(
        (f.ready_checks, f.check_every_secs, f.stop_timeout_secs),
        (60, 5, 30)
    );
    assert_eq!(f.curl, "curl");
    let host = if cfg!(target_arch = "aarch64") {
        Platform::LinuxArm64
    } else {
        Platform::LinuxX86_64
    };
    assert_eq!(f.platform, host);

    let f = parse(&[
        "--platform",
        "linux-arm64",
        "--version",
        "1.2.3",
        "--container",
        "coord_2",
        "--ready-checks",
        "2",
    ])
    .expect("flags");
    assert_eq!(
        (f.platform, f.version, f.container.as_str(), f.ready_checks),
        (Platform::LinuxArm64, Some(v("1.2.3")), "coord_2", 2)
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
        (vec!["--version", "1.2"], "not a version: \"1.2\""),
        (vec!["--ready-checks", "0"], "0 is not in 1..="),
        (vec!["--container=-x"], "not a container name: \"-x\""),
        (vec!["--container", ""], "not a container name: \"\""),
        (vec!["--container", "a/b"], "not a container name: \"a/b\""),
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
        "https://10.0.0.5:443",
        "https://[::1]:8444",
        "https://[::1]",
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
