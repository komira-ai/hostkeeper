//! A Mac that builds never sleeps: the power settings the start of a Mac's daemon makes
//! ([`hostkeeper_daemon::keep_awake`]), computed from the output of `pmset -g custom` and made
//! through a scripted `pmset`.

use std::cell::RefCell;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_daemon::keep_awake::{
    self, Change, WANTED, args, at_host_start, at_start, plan, read_args,
};
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput, PlatformKind};

/// What `pmset -g custom` prints on a Mac Studio as it comes from Apple: one power source, the
/// factory values.
const STUDIO_FACTORY: &str = "AC Power:
 lowpowermode         0
 powermode            0
 womp                 1
 networkoversleep     0
 sleep                1
 Sleep On Power Button 1
 ttyskeepawake        1
 hibernatemode        0
 disksleep            10
 displaysleep         10
 standbydelayhigh     86400
 standby              1
 powernap             1
 autopoweroffdelay    259200
 autopoweroff         1
 highstandbythreshold 50
 standbydelaylow      10800
 autorestart          0
";

/// The same Mac after the settings of a build Mac were made; the display still sleeps.
const STUDIO_SET: &str = "AC Power:
 lowpowermode         0
 powermode            0
 womp                 1
 networkoversleep     0
 sleep                0
 Sleep On Power Button 1
 ttyskeepawake        1
 hibernatemode        0
 disksleep            0
 displaysleep         10
 standbydelayhigh     86400
 standby              0
 powernap             0
 autopoweroffdelay    259200
 autopoweroff         0
 highstandbythreshold 50
 standbydelaylow      10800
 autorestart          1
";

/// A notebook's output: two power sources, the battery one with the sleep timer still set.
const NOTEBOOK: &str = "Battery Power:
 lowpowermode         0
 powermode            0
 womp                 0
 sleep                1
 disksleep            10
 displaysleep         2
 standby              0
 powernap             0
 autopoweroff         0
 autorestart          1
AC Power:
 lowpowermode         0
 powermode            0
 womp                 1
 sleep                0
 disksleep            0
 displaysleep         10
 standby              0
 powernap             0
 autopoweroff         0
 autorestart          1
";

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn config(keep_awake: bool) -> DaemonConfig {
    DaemonConfig::parse(&format!(
        "coordinator = \"https://coordinator.example:8443\"\n\
         ca_sha256 = \"9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\"\n\
         keep_awake = {keep_awake}\n\n[trusted_signers]\n\
         organisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/tags/*\"]\nhosted_runner = true\n"
    ))
    .expect("a daemon config")
}

fn change(key: &'static str, from: &str, to: u32) -> Change {
    Change {
        key,
        from: from.to_owned(),
        to,
    }
}

/// Runs [`at_start`] and returns what it returned and the lines it said.
fn start(
    keep_awake: bool,
    kind: PlatformKind,
    pmset: &FakeLaunchctl,
) -> (Vec<Change>, Vec<String>) {
    let said = RefCell::new(Vec::new());
    let changes = at_start(&config(keep_awake), kind, pmset, &|line| {
        said.borrow_mut().push(line.to_owned());
    });
    (changes, said.into_inner())
}

/// The keys are the ones a build Mac has, each value as the module says, displaysleep not among
/// them.
#[test]
fn the_wanted_settings_are_those_of_a_build_mac() {
    assert_eq!(
        WANTED,
        [
            ("sleep", 0),
            ("disksleep", 0),
            ("standby", 0),
            ("autopoweroff", 0),
            ("powernap", 0),
            ("autorestart", 1),
            ("womp", 1),
        ]
    );
    assert_eq!(read_args(), strings(&["-g", "custom"]));
}

/// A Mac Studio as Apple ships it gets one command that sets what differs: the sleep timers, the
/// standby and the deep idle off; `womp` is 1 already and is left out, so is `displaysleep`.
#[test]
fn a_factory_mac_studio_gets_one_pmset_command_for_what_differs() {
    let changes = plan(STUDIO_FACTORY);
    assert_eq!(
        changes,
        vec![
            change("sleep", "1", 0),
            change("disksleep", "10", 0),
            change("standby", "1", 0),
            change("autopoweroff", "1", 0),
            change("powernap", "1", 0),
            change("autorestart", "0", 1),
        ]
    );
    assert_eq!(
        args(&changes),
        Some(strings(&[
            "-a",
            "sleep",
            "0",
            "disksleep",
            "0",
            "standby",
            "0",
            "autopoweroff",
            "0",
            "powernap",
            "0",
            "autorestart",
            "1",
        ]))
    );
}

/// A Mac that is set already has nothing to change, so no command is made.
#[test]
fn a_mac_that_is_set_gets_no_command() {
    assert_eq!(plan(STUDIO_SET), Vec::new());
    assert_eq!(args(&[]), None);
}

/// `Sleep On Power Button`, `autopoweroffdelay` and `displaysleep` are other settings than
/// `sleep`, `autopoweroff` and nothing wanted: a Mac with those at any value and the wanted ones
/// set gets no command.
#[test]
fn a_key_is_matched_by_its_whole_first_word() {
    let text = "AC Power:\n Sleep On Power Button 7\n autopoweroffdelay 9\n displaysleep 3\n \
                sleepx 5\n sleep 0\n";
    assert_eq!(plan(text), Vec::new());
}

/// A setting the machine does not list is not set: a Mac without `powernap` or `autopoweroff`
/// gets no command for them.
#[test]
fn a_setting_the_host_does_not_list_is_not_set() {
    let text = "AC Power:\n sleep 1\n womp 1\n";
    assert_eq!(plan(text), vec![change("sleep", "1", 0)]);
    assert_eq!(plan(""), Vec::new());
    assert_eq!(plan("not pmset output at all\n"), Vec::new());
}

/// With two power sources, one that differs is enough: the change names its value, and `-a`
/// sets both.
#[test]
fn a_power_source_that_differs_is_changed_for_all() {
    let changes = plan(NOTEBOOK);
    assert_eq!(
        changes,
        vec![
            change("sleep", "1", 0),
            change("disksleep", "10", 0),
            change("womp", "0", 1),
        ]
    );
    assert_eq!(
        args(&changes),
        Some(strings(&["-a", "sleep", "0", "disksleep", "0", "womp", "1"]))
    );
}

/// A note after a value is not read as part of it, and a value that is not a number, or a key
/// with none, is changed.
#[test]
fn a_note_is_ignored_and_a_value_that_is_not_a_number_is_changed() {
    let noted = "AC Power:\n sleep 0 (prevented by powerd)\n";
    assert_eq!(plan(noted), Vec::new());
    let odd = "AC Power:\n sleep n/a\n disksleep\n";
    assert_eq!(
        plan(odd),
        vec![change("sleep", "n/a", 0), change("disksleep", "", 0)]
    );
}

/// A line that is not indented is a power source's name, never a setting.
#[test]
fn an_unindented_line_is_not_a_setting() {
    assert_eq!(plan("sleep 1\nAC Power:\n"), Vec::new());
}

/// The log line of a change names the key, what it was and what it is set to.
#[test]
fn a_change_logs_its_key_and_both_values() {
    assert_eq!(
        change("sleep", "1", 0).line(),
        "level=info keep_awake: pmset sleep was 1, set to 0"
    );
}

/// The start on a factory Mac reads the settings, makes them with one command and logs each
/// change once.
#[test]
fn the_start_reads_then_sets_what_differs_and_logs_each_change() {
    let pmset = FakeLaunchctl::new();
    pmset.always(&read_args(), Ok(LaunchctlOutput::success(STUDIO_FACTORY)));
    let command = args(&plan(STUDIO_FACTORY)).expect("a command");
    pmset.always(&command, Ok(LaunchctlOutput::success("")));
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, plan(STUDIO_FACTORY));
    assert_eq!(pmset.calls(), vec![read_args(), command]);
    assert_eq!(
        said,
        vec![
            "level=info keep_awake: pmset sleep was 1, set to 0",
            "level=info keep_awake: pmset disksleep was 10, set to 0",
            "level=info keep_awake: pmset standby was 1, set to 0",
            "level=info keep_awake: pmset autopoweroff was 1, set to 0",
            "level=info keep_awake: pmset powernap was 1, set to 0",
            "level=info keep_awake: pmset autorestart was 0, set to 1",
        ]
    );
}

/// A Mac that is set already is only read: no command, nothing logged.
#[test]
fn the_start_of_a_set_mac_runs_no_command_and_says_nothing() {
    let pmset = FakeLaunchctl::new();
    pmset.always(&read_args(), Ok(LaunchctlOutput::success(STUDIO_SET)));
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(pmset.calls(), vec![read_args()]);
    assert_eq!(said, Vec::<String>::new());
}

/// With `keep_awake` off the Mac's power settings are not read or set; the start says so once.
#[test]
fn with_keep_awake_off_no_pmset_runs() {
    let pmset = FakeLaunchctl::new();
    let (changes, said) = start(false, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(pmset.calls(), Vec::<Vec<String>>::new());
    assert_eq!(
        said,
        vec!["level=info keep_awake: is off in the config; the power settings are not touched"]
    );
}

/// A host that is not a Mac has no `pmset`: nothing runs and nothing is said, on or off.
#[test]
fn a_host_that_is_not_a_mac_runs_no_pmset() {
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        for on in [true, false] {
            let pmset = FakeLaunchctl::new();
            let (changes, said) = start(on, kind, &pmset);
            assert_eq!(changes, Vec::new());
            assert_eq!(pmset.calls(), Vec::<Vec<String>>::new());
            assert_eq!(said, Vec::<String>::new());
        }
    }
}

/// A `pmset -g custom` that cannot run, or that ends with a failure, is logged as an error and
/// nothing is set.
#[test]
fn a_read_that_fails_is_logged_and_nothing_is_set() {
    let pmset = FakeLaunchctl::new();
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(pmset.calls(), vec![read_args()]);
    assert_eq!(
        said,
        vec!["level=error keep_awake: pmset -g custom did not run: the fake has no answer"]
    );

    let pmset = FakeLaunchctl::new();
    pmset.always(&read_args(), Ok(LaunchctlOutput::exit(1, "pmset: no\n")));
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(pmset.calls(), vec![read_args()]);
    assert_eq!(
        said,
        vec!["level=error keep_awake: pmset -g custom ended with Some(1): pmset: no"]
    );
}

/// A command that cannot run, or that ends with a failure, logs the error and no change: the
/// settings are not said to be made.
#[test]
fn a_set_that_fails_logs_an_error_and_no_change() {
    let command = args(&plan(STUDIO_FACTORY)).expect("a command");

    let pmset = FakeLaunchctl::new();
    pmset.always(&read_args(), Ok(LaunchctlOutput::success(STUDIO_FACTORY)));
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(pmset.calls(), vec![read_args(), command.clone()]);
    assert_eq!(
        said,
        vec![format!(
            "level=error keep_awake: pmset {} did not run: the fake has no answer",
            command.join(" ")
        )]
    );

    let pmset = FakeLaunchctl::new();
    pmset.always(&read_args(), Ok(LaunchctlOutput::success(STUDIO_FACTORY)));
    pmset.always(&command, Ok(LaunchctlOutput::exit(77, "Must be root\n")));
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(
        said,
        vec![format!(
            "level=error keep_awake: pmset {} ended with Some(77): Must be root",
            command.join(" ")
        )]
    );
}

/// The start's glue on the host: a host that is not a Mac, and a Mac with the key off, run no
/// `pmset` of the machine the test runs on.
#[test]
fn the_host_start_runs_no_pmset_for_a_linux_host_or_with_the_key_off() {
    assert_eq!(
        at_host_start(&config(true), PlatformKind::Linux),
        Vec::new()
    );
    assert_eq!(
        at_host_start(&config(false), PlatformKind::Macos),
        Vec::new()
    );
    assert!(keep_awake::PMSET.starts_with("/usr/bin/"));
}

/// A `pmset` that is killed at its deadline is logged with the deadline, for the read and for the
/// command that sets.
#[test]
fn a_pmset_killed_at_its_deadline_says_so() {
    const KILLED: &str = "killed at its deadline of 30 seconds";
    let timed_out = |args: &[String]| LaunchctlError::TimedOut {
        args: args.join(" "),
    };
    let pmset = FakeLaunchctl::new();
    pmset.always(&read_args(), Err(timed_out(&read_args())));
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(
        said,
        vec![format!(
            "level=error keep_awake: pmset -g custom did not run: {KILLED}"
        )]
    );

    let command = args(&plan(STUDIO_FACTORY)).expect("a command");
    let pmset = FakeLaunchctl::new();
    pmset.always(&read_args(), Ok(LaunchctlOutput::success(STUDIO_FACTORY)));
    pmset.always(&command, Err(timed_out(&command)));
    let (changes, said) = start(true, PlatformKind::Macos, &pmset);
    assert_eq!(changes, Vec::new());
    assert_eq!(
        said,
        vec![format!(
            "level=error keep_awake: pmset {} did not run: {KILLED}",
            command.join(" ")
        )]
    );
}
