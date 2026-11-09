//! `--repositories` (packages-repositories.md sections 3.1 and 3.2): the directory is judged at the
//! start by the daemon's own loader, as it will be at `/etc/hostkeeper/repositories/`, and its
//! files reach every seed byte for byte.

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_pxe_seed::inputs::{
    BootServer, DaemonConfigText, InstallDisk, Record, RepositoryFiles,
};
use hostkeeper_pxe_seed::keys::AuthorizedKeys;
use hostkeeper_pxe_seed::menu::{MenuChoice, MenuRequest, Mode};
use hostkeeper_pxe_seed::seed::{SeedInputs, seed};

use super::kit::{DAEMON_CONFIG, Kit, RECORDED, client, http, refused, tftp_read};

/// A keyring's bytes: not text, with a zero and a high byte.
const KEYRING: [u8; 6] = [0x99, 0x01, 0x0d, 0x00, 0xff, 0x0a];

/// The stanza of an entry whose keyring is `keyring`, as the host will see it.
fn stanza(keyring: &str) -> String {
    format!(
        "Types: deb\nURIs: https://packages.example.com/apt\nSuites: testing\nComponents: main\n\
         Signed-By: /etc/hostkeeper/repositories/{keyring}\n"
    )
}

fn put(dir: &Path, name: &str, bytes: &[u8], mode: u32) {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// A directory `repositories` in the kit's scratch directory (mode 0755) holding the entry
/// `a.sources` and its keyring `k.gpg`.
fn directory(kit: &Kit) -> PathBuf {
    let dir = kit.scratch.path().join("repositories");
    std::fs::create_dir(&dir).expect("the directory");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    put(&dir, "a.sources", stanza("k.gpg").as_bytes(), 0o644);
    put(&dir, "k.gpg", &KEYRING, 0o644);
    dir
}

/// The command line of a proxy run of `kit` with `--repositories dir`.
fn args(kit: &Kit, dir: &Path) -> Vec<OsString> {
    let sink = client();
    kit.args(
        "proxy",
        sink.local_addr().expect("its address"),
        &["--repositories", &dir.display().to_string()],
    )
}

/// The start refused with `dir`: its error, once no line was logged.
fn refusal(kit: &Kit, dir: &Path) -> String {
    let (result, lines) = refused(args(kit, dir));
    assert!(lines.is_empty(), "{lines:?}");
    result.expect_err("refused")
}

/// A run with an accepted directory serves, to the recorded MAC, the user-data the generator makes
/// from the same files: the `.sources` text and the keyring (not text) are in its `write_files`,
/// byte for byte, in name order, before the daemon config's.
///
/// Catches a run that checks the directory and then installs hosts without their repository
/// entries, or with a keyring altered on the way.
#[test]
fn an_accepted_directory_reaches_the_seed_byte_for_byte() {
    let kit = Kit::new("repositories-served");
    let dir = directory(&kit);
    let mut run = kit.start(args(&kit, &dir));
    tftp_read(
        run.tftp,
        &format!("/grub/hostkeeper/x86_64/{RECORDED}"),
        &[],
    )
    .expect("the menu");
    run.wait("install linux-x86_64 as host new-1");
    let user = http(run.http, "GET", &format!("/seed/{RECORDED}/user-data"));
    assert_eq!(user.status, 200);
    let record = Record {
        host_id: "new-1".parse().expect("a host id"),
        role: None,
        platform: None,
    };
    let path = format!("grub/hostkeeper/x86_64/{RECORDED}");
    let MenuChoice::Install(install) = MenuRequest::parse(&path)
        .expect("a menu path")
        .choose(Mode::Proxy, &|_| Some(&record))
    else {
        panic!("the install")
    };
    let server = BootServer::new(std::net::SocketAddrV4::new(
        Ipv4Addr::LOCALHOST,
        run.http.port(),
    ))
    .expect("a boot server");
    let files = RepositoryFiles::new(vec![
        ("k.gpg".to_owned(), KEYRING.to_vec()),
        ("a.sources".to_owned(), stanza("k.gpg").into_bytes()),
    ])
    .expect("files");
    let want = seed(&SeedInputs {
        install: &install,
        server,
        daemon_config: &DaemonConfigText::parse(DAEMON_CONFIG).expect("a config"),
        keys: &AuthorizedKeys::parse(&std::fs::read_to_string(&kit.keys).expect("the keys file"))
            .expect("keys"),
        install_disk: &InstallDisk::default(),
        serial_console: None,
        repositories: Some(&files),
    });
    let served = String::from_utf8(user.body).expect("text");
    assert_eq!(served, want.user_data);
    let sources_at = served.find("repositories/a.sources").expect("the entry");
    let keyring_at = served.find("repositories/k.gpg").expect("the keyring");
    let config_at = served.find("hostkeeper.toml").expect("the daemon config");
    assert!(
        sources_at < keyring_at && keyring_at < config_at,
        "{served}"
    );
    assert_eq!(run.stop().0, Ok(()));
}

/// Each defect the daemon's loader refuses is refused at the start, naming the file in the
/// operator's directory and the loader's reason, before a line is logged or a socket served: a
/// key that turns off a check, a `Signed-By` naming the operator's own directory, a stray file, a
/// name outside the grammar, a file others may write, a keyring that is a link.
///
/// Catches a seed that installs hosts the daemon then refuses to start on, and an entry that
/// weakens apt's checks on every machine.
#[test]
fn each_loader_refusal_refuses_the_start_naming_the_file() {
    let kit = Kit::new("repositories-refused");
    let dir = directory(&kit);
    let shown = |name: &str| dir.join(name).display().to_string();

    let weak = format!("{}Trusted: yes\n", stanza("k.gpg"));
    put(&dir, "a.sources", weak.as_bytes(), 0o644);
    assert_eq!(
        refusal(&kit, &dir),
        format!(
            "{}: has the key \"trusted\", which turns off a check of apt's",
            shown("a.sources")
        )
    );

    let own = stanza("k.gpg").replace("/etc/hostkeeper/repositories", &dir.display().to_string());
    put(&dir, "a.sources", own.as_bytes(), 0o644);
    assert_eq!(
        refusal(&kit, &dir),
        format!(
            "{}: has the Signed-By \"{}\", not an absolute path of a keyring in its directory",
            shown("a.sources"),
            shown("k.gpg")
        )
    );

    put(&dir, "a.sources", stanza("k.gpg").as_bytes(), 0o644);
    put(&dir, "notes.txt", b"x", 0o644);
    assert_eq!(
        refusal(&kit, &dir),
        format!(
            "{}: neither a .sources file nor a keyring one names",
            shown("notes.txt")
        )
    );
    std::fs::remove_file(dir.join("notes.txt")).expect("remove");

    put(&dir, "Bad_Name.sources", stanza("k.gpg").as_bytes(), 0o644);
    assert_eq!(
        refusal(&kit, &dir),
        format!(
            "{}: the name \"Bad_Name\" is not 1 to 32 bytes of [a-z0-9-]",
            shown("Bad_Name.sources")
        )
    );
    std::fs::remove_file(dir.join("Bad_Name.sources")).expect("remove");

    put(&dir, "a.sources", stanza("k.gpg").as_bytes(), 0o666);
    assert_eq!(
        refusal(&kit, &dir),
        format!(
            "{}: has mode 0666: group or others can write it",
            shown("a.sources")
        )
    );
    put(&dir, "a.sources", stanza("k.gpg").as_bytes(), 0o644);

    std::fs::remove_file(dir.join("k.gpg")).expect("remove");
    put(kit.scratch.path(), "real.gpg", &KEYRING, 0o644);
    symlink(kit.scratch.path().join("real.gpg"), dir.join("k.gpg")).expect("link");
    assert_eq!(
        refusal(&kit, &dir),
        format!(
            "{}: the keyring it names is a symbolic link",
            shown("a.sources")
        )
    );
}

/// A directory that is missing, one holding no file, and one that is a file are refused naming the
/// directory: the flag names nothing an install could use.
///
/// Catches a run that serves seeds with no repository after the operator asked for some.
#[test]
fn a_missing_or_empty_directory_refuses_the_start() {
    let kit = Kit::new("repositories-none");
    let dir = kit.scratch.path().join("repositories");
    let none = format!(
        "{}: the directory is missing or holds no repository entry (a .sources file)",
        dir.display()
    );
    assert_eq!(refusal(&kit, &dir), none);
    std::fs::create_dir(&dir).expect("the directory");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(refusal(&kit, &dir), none);
    std::fs::remove_dir(&dir).expect("remove");
    put(kit.scratch.path(), "repositories", b"x", 0o644);
    assert_eq!(
        refusal(&kit, &dir),
        format!("{}: is not a directory", dir.display())
    );
}

/// A keyring over 1 MiB is refused naming it, and a keyring whose name a seed cannot carry is
/// refused naming the directory.
///
/// Catches a multi-megabyte file copied into the seed of every machine, and a `write_files` path
/// built from a name with a space in it.
#[test]
fn a_keyring_too_large_or_with_an_unusable_name_is_refused() {
    let kit = Kit::new("repositories-keyring");
    let dir = directory(&kit);
    put(&dir, "k.gpg", &vec![0; 1024 * 1024 + 1], 0o644);
    assert_eq!(
        refusal(&kit, &dir),
        format!("{}: is over 1048576 bytes", dir.join("k.gpg").display())
    );
    put(&dir, "k.gpg", &vec![0; 1024 * 1024], 0o644);
    let run = kit.start(args(&kit, &dir));
    assert_eq!(
        run.stop().0,
        Ok(()),
        "a keyring of exactly 1 MiB is accepted"
    );
    std::fs::remove_file(dir.join("k.gpg")).expect("remove");
    put(&dir, "my key.gpg", &KEYRING, 0o644);
    put(&dir, "a.sources", stanza("my key.gpg").as_bytes(), 0o644);
    assert_eq!(
        refusal(&kit, &dir),
        format!(
            "{}: the repository file name \"my key.gpg\" is not 1 to 255 of A-Za-z0-9._+-, not starting with a dot",
            dir.display()
        )
    );
}
