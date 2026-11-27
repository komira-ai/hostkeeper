//! The kernel's sources as `hardware_sources` runs them: each source read by its worker every
//! `POLL_SECS`, the host's two sources built by `at_root`, and the boot they are given
//! (`KernelBoot::of`). The helpers are the parent module's.

use super::*;

/// Proves each source is read at once and after each wait of `POLL_SECS` by its worker, until the
/// sleeper stops it.
#[test]
fn the_log_is_read_every_poll_secs_by_its_worker() {
    let host = Host::quiet("run");
    let keeper = host.keeper(BOOT_A);
    let records = |seq: u64| -> io::Result<Vec<KmsgRecord>> { Ok(vec![r(seq)]) };
    let kmsg = ScriptedKmsg(vec![records(UE), records(IO), records(AER)].into());
    let journal = Journalctl::new(host.root.path(), Box::new(host.launchctl.clone()));
    let mut log = KernelLog::new(Box::new(kmsg), Box::new(journal), KernelBoot::of(&keeper));
    assert_eq!((log.name(), log.every_secs()), ("journal", POLL_SECS));
    let sleeper = ScriptedSleeper::new(2);
    crate::hardware_sources::run(&mut log, &keeper, &FakeClock::new(NOW, 10), &sleeper);
    assert_eq!(sleeper.waits(), [60, 60, 60]);
    let expected = [id(BOOT_A, UE), id(BOOT_A, IO), id(BOOT_A, AER)];
    assert_eq!(host.held_ids("journal"), expected);
}

/// Proves a host's kernel sources are its log and its files, read every `POLL_SECS`, built
/// without reading anything, in the boot they are given.
#[test]
fn at_root_builds_the_log_and_the_files_without_reading_anything() {
    let host = Host::new("atroot");
    let boot = KernelBoot {
        id: BOOT_A.to_owned(),
        unix: BOOT_UNIX,
    };
    let sources = at_root(host.root.path(), &boot);
    let named: Vec<(&str, u64)> = sources.iter().map(|s| (s.name(), s.every_secs())).collect();
    assert_eq!(named, [("journal", POLL_SECS), ("sysfs", POLL_SECS)]);
    assert!(host.launchctl.calls().is_empty());
    assert_eq!(
        fs::read_dir(host.root.path()).expect("the root").count(),
        0,
        "nothing is made under the root"
    );
}

/// Proves a keeper's boot names the kernel sources' boot, and a boot with no id is named by when
/// it began, as 16 hex digits.
#[test]
fn a_kernel_boot_is_the_keepers_and_a_boot_with_no_id_is_named_by_its_start() {
    let host = Host::new("kboot");
    let keeper = host.keeper(BOOT_A);
    assert_eq!(
        KernelBoot::of(&keeper),
        KernelBoot {
            id: BOOT_A.to_owned(),
            unix: BOOT_UNIX
        }
    );
    drop(keeper);
    let facts = BootFacts {
        boot_id: None,
        boot_unix: 0x1234,
        machine_id: "m-1".to_owned(),
    };
    let log = Arc::new(RecordingLog::default());
    let unread = HealthKeeper::open(host.state.path(), facts, Box::new(Quiet), log);
    assert_eq!(KernelBoot::of(&unread).id, "0000000000001234");
}
