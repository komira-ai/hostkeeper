//! The file that keeps the address the coordinator was last reached at, and the reach the daemon
//! makes over it.

use std::net::{IpAddr, Ipv4Addr};

use hostkeeper_link::testing::{FakeLookup, RecordingLog};
use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A daemon config whose coordinator is `url`.
fn config(url: &str) -> DaemonConfig {
    DaemonConfig::parse(&format!(
        "coordinator = \"{url}\"\nca_sha256 = \"{}\"\n\n[trusted_signers]\n\
         organisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/tags/*\"]\nhosted_runner = true\n",
        "ab".repeat(32)
    ))
    .expect("a config")
}

/// The reach of the coordinator at `url`, whose name looks up to nothing.
fn reach(url: &str) -> Reach {
    Reach::new(
        &config(url),
        Arc::new(FakeLookup::answering(&[])),
        Arc::new(RecordingLog::default()),
    )
    .expect("a reach")
}

const KEPT: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 8443);

/// Catches an address kept in a form the next start cannot read, or read back for a coordinator
/// it was not reached as: the file holds the name and the address, and is read back as that
/// address for the same name and port alone; another name, another port, a file that does not
/// parse, and no file are read as none.
#[test]
fn the_address_kept_is_read_back_for_the_same_name_and_port_alone() {
    let dir = scratch("coordinator-address-kept");
    let named = reach("https://coordinator.example:8443");
    assert_eq!(read(dir.path(), &named), None, "no file");
    write(dir.path(), &named, KEPT).expect("written");
    assert_eq!(
        fs::read_to_string(dir.path().join(ADDRESS_FILE)).expect("the file"),
        r#"{"name":"coordinator.example","address":"192.0.2.10:8443"}"#
    );
    assert_eq!(read(dir.path(), &named), Some(KEPT));
    assert_eq!(
        read(dir.path(), &reach("https://other.example:8443")),
        None,
        "another name"
    );
    assert_eq!(
        read(dir.path(), &reach("https://coordinator.example:9443")),
        None,
        "another port"
    );
    fs::write(dir.path().join(ADDRESS_FILE), b"{\"name\": 1}").expect("a broken file");
    assert_eq!(read(dir.path(), &named), None, "a file that does not parse");
}

/// Catches a restarted daemon that forgets where it last reached the coordinator: the reach the
/// daemon makes over a state directory remembers the address its file keeps, and without a
/// state directory, or a file, it remembers none. A state directory the file cannot be written
/// to is the write's error.
#[test]
fn the_daemons_reach_remembers_the_address_its_state_directory_keeps() {
    let dir = scratch("coordinator-address-reach");
    let url = "https://coordinator.example:8443";
    let made = |state_dir: Option<&Path>| {
        host_reach(
            &config(url),
            state_dir,
            Arc::new(FakeLookup::answering(&[])),
            Arc::new(RecordingLog::default()),
        )
        .expect("a reach")
        .remembered()
    };
    assert_eq!(made(Some(dir.path())), None);
    write(dir.path(), &reach(url), KEPT).expect("written");
    assert_eq!(made(Some(dir.path())), Some(KEPT));
    assert_eq!(made(None), None);

    let refused = host_reach(
        &config("https://coordinator.example/x"),
        Some(dir.path()),
        Arc::new(FakeLookup::answering(&[])),
        Arc::new(RecordingLog::default()),
    );
    assert!(matches!(refused, Err(EndpointError::Path(_))));

    let missing = dir.path().join("missing");
    let error = write(&missing, &reach(url), KEPT).expect_err("no directory");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

/// Catches a production lookup that is not the system's: an address is looked up as itself,
/// with no resolver.
#[test]
fn the_host_lookup_reads_an_address_as_itself() {
    assert_eq!(
        host_lookup().lookup("127.0.0.1", 9).expect("an address"),
        [SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9)]
    );
}
