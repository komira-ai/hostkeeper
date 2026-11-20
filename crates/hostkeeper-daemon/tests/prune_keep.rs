//! What a prune after a confirmed switch keeps besides what the records use (design section 4.7
//! and Q14): the versions `current` and `previous` name, the version the service's pin names
//! (`pins/<service>` beside the state directory), and the newest `--keep-versions` others,
//! newest by the time each was staged; and that it deletes nothing while an update of the
//! service is on trial.

mod support;
mod support_apt;
mod support_host;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use clap::Parser;
use hostkeeper_config::names::Version;
use hostkeeper_daemon::cli::{Cli, Command};
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::pin;
use hostkeeper_daemon::prune::{self, DEFAULT_KEEP_VERSIONS, LayoutPrune, MAX_KEEP_VERSIONS};
use hostkeeper_daemon::start::{Settings, startup};
use hostkeeper_daemon::testing::{RecordingPrune, manifest};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_steps::record::State;
use hostkeeper_steps::{Layout, PruneReport, RecordStore, Stamp};
use support::force::{WEB, succeeded};
use support::{OWN, Rig, alerting, config, lines, name, snapshot};
use support_host::NativeHost;

/// A version, from a literal.
fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// The names `texts`, as a set.
fn set(texts: &[&str]) -> BTreeSet<String> {
    texts.iter().map(|text| (*text).to_owned()).collect()
}

/// `<n>.0.0` for each of `ns`.
fn numbered(ns: &[u32]) -> Vec<String> {
    ns.iter().map(|n| format!("{n}.0.0")).collect()
}

/// The version directories of `web`, by name.
fn versions(rig: &Rig) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(rig.packages.join(WEB))
        .expect("the package directory")
        .map(|entry| entry.expect("an entry"))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort_by_key(|name| {
        name.split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
    });
    names
}

/// The path of `web`'s pin on `rig`.
fn pin_of(rig: &Rig) -> PathBuf {
    pin::path(&rig.state, &name(WEB))
}

/// Writes `bytes` as `web`'s pin on `rig`.
fn pin_web(rig: &Rig, bytes: &[u8]) {
    let path = pin_of(rig);
    fs::create_dir_all(path.parent().expect("the pins directory")).expect("the pins directory");
    fs::write(path, bytes).expect("the pin");
}

/// Version `<n>.0.0` of `web` staged on `rig` and never switched to, its seal written at second
/// `n`.
fn staged(rig: &Rig, n: u32) {
    let text = format!("{n}.0.0");
    rig.add_with(WEB, &text, &manifest(WEB, &text));
    fs::File::options()
        .write(true)
        .open(rig.packages.join(WEB).join(format!("{text}.seal")))
        .expect("the seal")
        .set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(n)))
        .expect("the time of the seal");
}

/// A daemon on `rig` whose prune is the host's, keeping what the flag's default keeps.
fn daemon(rig: &Rig) -> Daemon {
    let ports = Ports {
        prune: Arc::new(LayoutPrune),
        ..rig.ports()
    };
    Daemon::new(config(), rig.dirs(), ports, OWN)
}

/// The confirmed switch of `web` to `text` on `daemon`, under its lock.
fn confirm(daemon: &Daemon, text: &str) -> Option<PruneReport> {
    let ctx = daemon.ctx();
    let service = name(WEB);
    let held = Package {
        verb: HookedVerb::Update,
        version: version(text),
        digest: PackageDigest::new(&support::d()).expect("a digest"),
    };
    let guard = ctx.locks.take(&service, held).expect("the service's lock");
    prune::confirmed_switch(ctx, &guard, &service, &version(text))
}

/// Catches a prune that drops any one reason to keep a version, or keeps one for no reason:
/// with 1.0.0 to 9.0.0 staged in turn, `current` 9.0.0, `previous` 8.0.0 and the pin naming
/// 1.0.0, the oldest, a daemon on the default of 4 keeps 9.0.0, 8.0.0, 1.0.0 and the 4 newest
/// others, 4.0.0 to 7.0.0, and deletes 2.0.0 and 3.0.0 with their seals. Once the pin is gone,
/// a switch to 10.0.0 lets 1.0.0 go with 4.0.0, the oldest of the others now, which shows the
/// pin, not its place among the newest, kept 1.0.0.
#[test]
fn a_prune_keeps_current_previous_the_pin_and_the_newest_four() {
    let rig = Rig::new("keep-reasons");
    let daemon = daemon(&rig);
    assert_eq!(daemon.ctx().keep_versions(), 4);
    assert_eq!(DEFAULT_KEEP_VERSIONS, 4);
    let layout = Layout::package(&rig.packages.join(WEB));
    for n in 1..=9 {
        staged(&rig, n);
    }
    layout.switch("8.0.0").expect("the switch");
    layout.switch("9.0.0").expect("the switch");
    pin_web(&rig, b"1.0.0\n");
    let report = confirm(&daemon, "9.0.0").expect("a report");
    assert_eq!(report.deleted, set(&["2.0.0", "3.0.0"]));
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(versions(&rig), numbered(&[1, 4, 5, 6, 7, 8, 9]));
    for gone in ["2.0.0", "3.0.0"] {
        assert!(!rig.packages.join(WEB).join(format!("{gone}.seal")).exists());
    }

    fs::remove_file(pin_of(&rig)).expect("the pin removed");
    staged(&rig, 10);
    layout.switch("10.0.0").expect("the switch");
    let report = confirm(&daemon, "10.0.0").expect("a report");
    assert_eq!(report.deleted, set(&["1.0.0", "4.0.0"]));
    assert_eq!(versions(&rig), numbered(&[5, 6, 7, 8, 9, 10]));
}

/// Catches a count that is not the daemon's, or a pin read only with a newline: a daemon keeping
/// 1 other, of 1.0.0 to 5.0.0 with `current` 5.0.0, `previous` 4.0.0 and a pin of 1.0.0 written
/// with no newline, keeps 1.0.0 and 3.0.0, the newest other; a pin of a version that is not on
/// the host keeps nothing more; and a daemon keeping none deletes every other version.
#[test]
fn the_count_is_the_daemons_and_the_pin_needs_no_newline() {
    let rig = Rig::new("keep-count");
    let layout = Layout::package(&rig.packages.join(WEB));
    for n in 1..=5 {
        staged(&rig, n);
    }
    layout.switch("4.0.0").expect("the switch");
    layout.switch("5.0.0").expect("the switch");
    pin_web(&rig, b"1.0.0");
    let one = daemon(&rig).keeping_versions(1);
    assert_eq!(one.ctx().keep_versions(), 1);
    let report = confirm(&one, "5.0.0").expect("a report");
    assert_eq!(report.deleted, set(&["2.0.0"]));
    assert_eq!(versions(&rig), numbered(&[1, 3, 4, 5]));

    pin_web(&rig, b"0.9.0\n");
    let none = daemon(&rig).keeping_versions(0);
    let report = confirm(&none, "5.0.0").expect("a report");
    assert_eq!(report.deleted, set(&["1.0.0", "3.0.0"]));
    assert_eq!(versions(&rig), numbered(&[4, 5]));
}

/// Catches a pin that cannot be read taken for no pin, which could delete the version it names:
/// a pin that is empty, a bare newline, a version with a second line, a word with a space, a
/// name that is no version, bytes that are not UTF-8 or longer than any version, a directory
/// where the pin is, and a file where the pins directory is, each make the confirmed switch the
/// alert that names the pin, with nothing deleted.
#[test]
fn a_pin_that_cannot_be_read_prunes_nothing() {
    let rig = Rig::new("keep-bad-pin");
    let layout = Layout::package(&rig.packages.join(WEB));
    for n in 1..=4 {
        staged(&rig, n);
    }
    layout.switch("3.0.0").expect("the switch");
    layout.switch("4.0.0").expect("the switch");
    let (daemon, alerts) = alerting(daemon(&rig).keeping_versions(0));
    let before = snapshot(&rig.packages);
    let path = pin_of(&rig);
    let failed = |why: String| {
        format!(
            "hostkeeper daemon: level=error alert=prune_failed service=web: the old versions of web could not be pruned: {why}"
        )
    };
    let malformed = failed(format!(
        "the pin {} does not hold a version",
        path.display()
    ));
    let mut expected = Vec::new();
    let long = vec![b'1'; 2000];
    let contents: [&[u8]; 7] = [
        b"",
        b"\n",
        b"1.0.0\n\n",
        b"1.0.0 2.0.0",
        b"-1.0.0",
        b"1.0.\xff",
        long.as_slice(),
    ];
    for bytes in contents {
        pin_web(&rig, bytes);
        assert_eq!(confirm(&daemon, "4.0.0"), None, "{bytes:?}");
        assert_eq!(snapshot(&rig.packages), before, "{bytes:?}");
        expected.push(malformed.clone());
        assert_eq!(lines(&alerts), expected, "{bytes:?}");
    }

    fs::remove_file(&path).expect("the pin removed");
    fs::create_dir(&path).expect("a directory in its place");
    assert_eq!(confirm(&daemon, "4.0.0"), None);
    expected.push(failed(format!(
        "the pin {} cannot be read: Is a directory (os error 21)",
        path.display()
    )));
    assert_eq!(lines(&alerts), expected);

    let pins = path.parent().expect("the pins directory");
    fs::remove_dir_all(pins).expect("the pins directory removed");
    fs::write(pins, "").expect("a file in its place");
    assert_eq!(confirm(&daemon, "4.0.0"), None);
    expected.push(failed(format!(
        "the pin {} cannot be read: Not a directory (os error 20)",
        path.display()
    )));
    assert_eq!(lines(&alerts), expected);
    assert_eq!(snapshot(&rig.packages), before);

    fs::remove_file(pins).expect("the file removed");
    let report = confirm(&daemon, "4.0.0").expect("a report");
    assert_eq!(report.deleted, set(&["1.0.0", "2.0.0"]));
    assert_eq!(lines(&alerts), expected);
}

/// Catches a prune during a trial: with `web`'s step record holding an update that is not over
/// (`pending`, `running`, or `done` between two steps) and not in the post-success watch, a
/// confirmed switch deletes nothing, calls no prune and alerts nothing, though 1.0.0 is beyond
/// what is kept. The same record in the post-success watch, whose commit has happened, is
/// pruned, and so is the update that ends `Succeeded` once the record is over.
#[test]
fn nothing_is_pruned_while_an_update_is_on_trial() {
    let farm = NativeHost::new("keep-trial");
    let rig = farm.rig();
    for text in ["1.0.0", "2.0.0", "3.0.0"] {
        farm.publish(WEB, text);
    }
    let installing = farm.daemon();
    assert_eq!(
        installing.order(&farm.install(WEB, "1.0.0", 1)),
        succeeded()
    );
    assert_eq!(installing.order(&farm.update(WEB, "2.0.0", 2)), succeeded());
    assert_eq!(installing.order(&farm.update(WEB, "3.0.0", 3)), succeeded());
    let finished = rig.record(WEB).expect("the record");
    let store = RecordStore::new(&rig.state.join(WEB));
    let pruner = RecordingPrune::new();
    let (daemon, alerts) = alerting(
        farm.daemon_on(rig.placed_fs.clone(), pruner.clone())
            .keeping_versions(0),
    );
    let before = snapshot(&rig.packages);
    let trials = [
        State::Pending,
        State::Running {
            started: Stamp::now(&*rig.clock),
            deadline_secs: 100,
            unbounded: false,
        },
        State::Done {
            code: Some(0),
            last_line: None,
        },
    ];
    for state in trials {
        let mut rec = finished.clone();
        rec.state = state.clone();
        rec.progress.watching = None;
        store.save(&rec).expect("the record");
        assert_eq!(confirm(&daemon, "3.0.0"), None, "{state:?}");
        assert_eq!(snapshot(&rig.packages), before, "{state:?}");
        assert!(pruner.calls().is_empty(), "{state:?}");
    }
    assert!(lines(&alerts).is_empty(), "{:?}", lines(&alerts));

    let mut watched = finished.clone();
    watched.state = State::Pending;
    watched.progress.watching = Some(Stamp::now(&*rig.clock));
    store.save(&watched).expect("the record");
    let report = confirm(&daemon, "3.0.0").expect("a report");
    assert_eq!(report.deleted, set(&["1.0.0"]));
    assert_eq!(pruner.calls(), [WEB]);

    store.save(&finished).expect("the record");
    farm.publish(WEB, "4.0.0");
    assert_eq!(daemon.order(&farm.update(WEB, "4.0.0", 4)), succeeded());
    assert_eq!(pruner.calls(), [WEB, WEB]);
    assert_eq!(versions(rig), numbered(&[3, 4]));
}

/// Catches a keep that holds for a direct call only, not the orders: eight orders of `web`
/// through a daemon on the default leave `current` 8.0.0, `previous` 7.0.0 and the 4 staged
/// before them, 3.0.0 to 6.0.0; the pin then names 3.0.0, the oldest, and two more updates
/// leave it with `current`, `previous` and the 4 newest others, 5.0.0 to 8.0.0, deleting 4.0.0.
/// Of the others each prune here chooses among (none past 8.0.0), the one staged later has the
/// greater name, so two seals written in the same tick of the file system's clock are still
/// ordered as they were staged.
#[test]
fn updates_keep_current_previous_the_pin_and_the_newest_four() {
    let farm = NativeHost::new("keep-orders");
    let rig = farm.rig();
    let daemon = farm.daemon_on(rig.placed_fs.clone(), Arc::new(LayoutPrune));
    for n in 1..=10 {
        farm.publish(WEB, &format!("{n}.0.0"));
    }
    assert_eq!(daemon.order(&farm.install(WEB, "1.0.0", 1)), succeeded());
    for n in 2..=8 {
        let order = farm.update(WEB, &format!("{n}.0.0"), u64::from(n));
        assert_eq!(daemon.order(&order), succeeded(), "{n}.0.0");
    }
    assert_eq!(versions(rig), numbered(&[3, 4, 5, 6, 7, 8]));

    pin_web(rig, b"3.0.0\n");
    for n in 9..=10 {
        let order = farm.update(WEB, &format!("{n}.0.0"), u64::from(n));
        assert_eq!(daemon.order(&order), succeeded(), "{n}.0.0");
    }
    assert_eq!(versions(rig), numbered(&[3, 5, 6, 7, 8, 9, 10]));
}

/// The settings `hostkeeper daemon` parses from `args`.
fn parsed(args: &[&str]) -> Result<Settings, clap::Error> {
    let mut all = vec!["hostkeeper", "daemon"];
    all.extend_from_slice(args);
    let Cli {
        command: Command::Daemon(flags),
    } = Cli::try_parse_from(all)?
    else {
        panic!("`hostkeeper daemon` is the daemon verb");
    };
    Ok(flags.settings())
}

/// Catches a flag that does not reach the daemon, or takes a count outside its range: with no
/// flag the settings keep 4, `--keep-versions` takes 0 and 32 as given, and 33, -1 and a word
/// are usage errors (exit 2, as clap reports them); a daemon started with 2 keeps 2.
#[test]
fn the_keep_versions_flag_reaches_the_daemon_within_its_range() {
    assert_eq!(parsed(&[]).expect("no flag").keep_versions, 4);
    for count in [0, MAX_KEEP_VERSIONS] {
        let given = count.to_string();
        let settings = parsed(&["--keep-versions", &given]).expect("a count in range");
        assert_eq!(settings.keep_versions, count);
    }
    assert_eq!(MAX_KEEP_VERSIONS, 32);
    for refused in ["33", "-1", "four"] {
        let error = parsed(&["--keep-versions", refused]).expect_err(refused);
        assert_eq!(error.exit_code(), 2, "{refused}");
    }

    let rig = Rig::new("keep-flag");
    let settings = Settings {
        keep_versions: 2,
        ..rig.settings()
    };
    let started = startup(&settings, rig.ports(), OWN).expect("the daemon starts");
    assert_eq!(started.daemon().ctx().keep_versions(), 2);
}
