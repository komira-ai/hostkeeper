use super::*;

// ---------------------------------------------------------------------------------------------
// descriptor
// ---------------------------------------------------------------------------------------------

const KEYRING_KEY: &str = "apt/komira-archive-keyring.gpg";
const DESCRIPTOR_KEY: &str = "hostkeeper-repository.json";

/// A keyring of one public key: a version 4 key packet in the new packet format.
fn keyring() -> Vec<u8> {
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.extend([7_u8; 8]);
    let mut packet = vec![0xc0 | 6, u8::try_from(body.len()).expect("short")];
    packet.extend(body);
    packet
}

fn write(d: &Path, fake: &Fake, extra: &[&str]) -> Output {
    let (work, aws) = (d.join("work-descriptor"), fake.program());
    let mut args = vec![
        "channels",
        "descriptor",
        "write",
        "--bucket",
        BUCKET,
        "--keyring-key",
        KEYRING_KEY,
        "--work",
        text(&work),
        "--aws",
        text(&aws),
    ];
    args.extend_from_slice(extra);
    xtask(d, &args)
}

#[test]
fn descriptor_write_puts_the_descriptor_once_with_if_none_match_and_leaves_it_as_it_is_after() {
    let d = dir("descriptor-write");
    let fake = Fake::new(&d);
    let o = write(&d, &fake, &[]);
    assert_eq!(o.status.code(), Some(1), "the keyring is not in the bucket");
    assert!(
        stderr(&o).contains("the keyring apt/komira-archive-keyring.gpg is not in the bucket"),
        "{}",
        stderr(&o)
    );
    fake.put(KEYRING_KEY, &keyring());
    let o = write(&d, &fake, &["--distribution-id", "E1ABCDEFGHIJK"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let said = stdout(&o);
    assert!(
        said.contains("hostkeeper-repository.json: written\n"),
        "{said}"
    );
    assert!(
        said.contains("/hostkeeper-repository.json: invalidated\n"),
        "{said}"
    );
    assert_eq!(
        fake.puts(),
        [(
            DESCRIPTOR_KEY.to_owned(),
            vec!["--if-none-match".to_owned(), "*".to_owned()]
        )]
    );
    let held: serde_json::Value =
        serde_json::from_slice(&fake.object(DESCRIPTOR_KEY)).expect("JSON");
    assert_eq!(held["channels"], serde_json::json!(["testing", "stable"]));
    assert_eq!(
        held["formats"]["macos-pkg"]["packages"]["hello"]["dir"],
        "hostkeeper"
    );
    let o = write(&d, &fake, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("already holds this release's facts"),
        "{}",
        stdout(&o)
    );
    assert_eq!(fake.puts().len(), 1);
    let usage = exits(
        &d,
        2,
        &[
            "channels",
            "descriptor",
            "write",
            "--bucket",
            "Bad Bucket",
            "--keyring-key",
            KEYRING_KEY,
            "--work",
            text(&d.join("w")),
            "--aws",
            text(&fake.program()),
        ],
    );
    assert!(usage.contains("--bucket"), "{usage}");
}

#[test]
fn descriptor_check_reads_the_served_descriptor_and_keyring_and_fails_on_a_missing_file() {
    let d = dir("descriptor-check");
    let fake = Fake::new(&d);
    fake.put(KEYRING_KEY, &keyring());
    let o = write(&d, &fake, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let work = d.join("work-check");
    let url = format!("https://{HOST}");
    let check = || {
        xtask(
            &d,
            &[
                "channels",
                "descriptor",
                "check",
                "--url",
                &url,
                "--work",
                text(&work),
                "--curl",
                text(&fake.program()),
            ],
        )
    };
    let o = check();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(&format!("{url}/{DESCRIPTOR_KEY} could not be downloaded")),
        "{}",
        stderr(&o)
    );
    fake.serve(DESCRIPTOR_KEY, &fake.object(DESCRIPTOR_KEY));
    fake.serve(KEYRING_KEY, &keyring());
    let o = check();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("hostkeeper-repository.json: valid, 2 channels, platforms linux-x86_64 linux-arm64 macos-arm64, keys "),
        "{}",
        stdout(&o)
    );
}
