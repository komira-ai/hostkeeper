//! The unit, `hostkeeper.service`: its command lines, its account, its restart policy, its lack
//! of a start limit of its own and its watchdog, the units it conflicts with and the keys it sets.

use hostkeeper_link::CALL_SECS;

use crate::daemon_flags::parse_flags;
use crate::unit::{self, Unit};
use crate::{UNIT, altered, read, refuses};

/// The daemon's command line: `current` of the binary directory of hostkeeper's own update (the
/// binary the update chose; `self_update.rs` holds it to the daemon's `binary_dir`), the `daemon`
/// verb with each path of design sections 4.1 and 10 spelled out as a flag, and the flag that
/// makes it report ready to systemd. `daemon_flags.rs` parses this line with the binary's own flag
/// parser.
const DAEMON_COMMAND: [&str; 11] = [
    "/var/lib/updater/state/hostkeeper/bin/current",
    "daemon",
    "--config",
    "/etc/hostkeeper/hostkeeper.toml",
    "--state-dir",
    "/var/lib/updater/state",
    "--packages-dir",
    "/var/lib/hostkeeper/packages",
    "--admin-socket",
    "/run/hostkeeper/admin.sock",
    "--notify-systemd",
];

/// The command line run before each start: the binary the package installed gives the binary
/// directory its first binary when `current` starts none (`self-update-seed`). Its `-` prefix,
/// the one prefix a line of this unit has, lets the start go on when it fails: an older binary
/// without the verb, under a unit an update placed, while `current` is already there.
/// `daemon_flags.rs` parses it with the binary's own flag parser.
const SEED_COMMAND: [&str; 6] = [
    "/usr/bin/hostkeeper",
    "self-update-seed",
    "--state-dir",
    "/var/lib/updater/state",
    "--binary",
    "/usr/bin/hostkeeper",
];

/// kbf's update agent and the root half that serves it, as kbf v0.0.64 names their units in
/// `packaging/host-agent/linux/`: `kbf-updater-agent.service`, and `kbf-host-local.service` or,
/// on a host whose frontend is a container, `kbf-host-local-frontend.service` in its place
/// (`packaging/host-agent/README.md` of that tag). Design section 12.3: the two families never
/// run together.
const KBF_UNITS: [&str; 3] = [
    "kbf-updater-agent.service",
    "kbf-host-local.service",
    "kbf-host-local-frontend.service",
];

/// Every key the unit sets, by section.
const UNIT_KEYS: [(&str, &[&str]); 3] = [
    (
        "Unit",
        &[
            "Description",
            "Documentation",
            "After",
            "Wants",
            "Conflicts",
            "StartLimitIntervalSec",
        ],
    ),
    (
        "Service",
        &[
            "Type",
            "TimeoutStartSec",
            "ExecStartPre",
            "ExecStart",
            "Restart",
            "RestartSec",
            "RestartMode",
            "WatchdogSec",
            "RuntimeDirectory",
            "RuntimeDirectoryMode",
        ],
    ),
    ("Install", &["WantedBy"]),
];

/// Keys that take root, or a right of root, from the service: an account, a capability bound or
/// a sandbox. The daemon is root by design (section 5.3).
const CONFINING_KEYS: [&str; 15] = [
    "User",
    "Group",
    "DynamicUser",
    "SupplementaryGroups",
    "NoNewPrivileges",
    "CapabilityBoundingSet",
    "AmbientCapabilities",
    "SystemCallFilter",
    "SystemCallArchitectures",
    "ReadOnlyPaths",
    "ReadWritePaths",
    "InaccessiblePaths",
    "LockPersonality",
    "MemoryDenyWriteExecute",
    "RootDirectory",
];
const CONFINING_PREFIXES: [&str; 3] = ["Protect", "Private", "Restrict"];

fn check_command(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    for entry in &unit.entries {
        let key = entry.key.as_str();
        if matches!(
            key,
            "Environment" | "EnvironmentFile" | "PassEnvironment" | "UnsetEnvironment"
        ) {
            return Err(format!(
                "line {}: the unit sets the environment ({key}=); the daemon's configuration is its flags",
                entry.line
            ));
        }
        if key.starts_with("Exec") && !matches!(key, "ExecStart" | "ExecStartPre") {
            return Err(format!(
                "line {}: {key}= runs a command besides the daemon and its seed",
                entry.line
            ));
        }
    }
    let words = unit::command_words(unit.single("Service", "ExecStart")?)?;
    if words != DAEMON_COMMAND {
        return Err(format!("ExecStart= runs {words:?}, not {DAEMON_COMMAND:?}"));
    }
    let pre = unit.single("Service", "ExecStartPre")?;
    let seed = pre.strip_prefix('-').ok_or_else(|| {
        format!("ExecStartPre= has no `-`: a seed that fails would stop the start: {pre:?}")
    })?;
    let words = unit::command_words(seed)?;
    if words != SEED_COMMAND {
        return Err(format!(
            "ExecStartPre= runs {words:?}, not {SEED_COMMAND:?}"
        ));
    }
    Ok(())
}

/// Catches a unit whose command line is not the daemon's: a program other than `current` of the
/// binary directory (the deb's `/usr/bin/hostkeeper`, `previous`), a renamed or dropped flag, a
/// flag without its value, another verb, a second command, a prefix that ignores a failure, a
/// value taken from the environment, or any environment set for the daemon; and a unit whose seed
/// before the start is missing, can stop the start, or is not the seed of the state directory
/// with the package's binary.
#[test]
fn unit_command_line_is_the_daemon_with_its_paths_and_no_environment() {
    let text = read(UNIT);
    assert_eq!(check_command(&text), Ok(()));
    let line_end = "--notify-systemd\n";
    let cases = [
        (
            altered(
                &text,
                line_end,
                "--notify-systemd\nEnvironment=RUST_LOG=debug\n",
            ),
            "(Environment=)",
        ),
        (
            altered(
                &text,
                line_end,
                "--notify-systemd\nEnvironmentFile=-/etc/default/hostkeeper\n",
            ),
            "(EnvironmentFile=)",
        ),
        (
            altered(&text, line_end, "--notify-systemd\nPassEnvironment=HOME\n"),
            "(PassEnvironment=)",
        ),
        (
            altered(
                &text,
                line_end,
                "--notify-systemd\nExecStartPost=/usr/bin/true\n",
            ),
            "ExecStartPost= runs a command",
        ),
        (
            altered(
                &text,
                line_end,
                "--notify-systemd\nExecStartPre=-/usr/bin/true\n",
            ),
            "sets ExecStartPre= 2 times",
        ),
        (
            altered(&text, "ExecStartPre=-/usr/bin/", "ExecStartPre=/usr/bin/"),
            "ExecStartPre= has no `-`",
        ),
        (
            altered(
                &text,
                "self-update-seed --state-dir",
                "self-update-seed --dir",
            ),
            "ExecStartPre= runs",
        ),
        (
            altered(&text, " --binary /usr/bin/hostkeeper\n", "\n"),
            "ExecStartPre= runs",
        ),
        (
            altered(
                &text,
                "ExecStartPre=-/usr/bin/hostkeeper self-update-seed --state-dir /var/lib/updater/state --binary /usr/bin/hostkeeper\n",
                "",
            ),
            "[Service] has no ExecStartPre=",
        ),
        (
            altered(
                &text,
                line_end,
                "--notify-systemd\nExecReload=/bin/kill -HUP $MAINPID\n",
            ),
            "ExecReload= runs a command",
        ),
        (
            altered(
                &text,
                "--config /etc/hostkeeper/hostkeeper.toml",
                "--config $CONFIG",
            ),
            "holds `$`",
        ),
        (
            altered(
                &text,
                "--config /etc/hostkeeper/hostkeeper.toml",
                "--config %E/hostkeeper/hostkeeper.toml",
            ),
            "holds `%`",
        ),
        (
            altered(&text, "current daemon --config", "previous daemon --config"),
            "ExecStart= runs",
        ),
        (
            altered(
                &text,
                "ExecStart=/var/lib/updater/state/hostkeeper/bin/current daemon",
                "ExecStart=/usr/bin/hostkeeper daemon",
            ),
            "ExecStart= runs",
        ),
        (
            altered(
                &text,
                "daemon --config /etc/hostkeeper/hostkeeper.toml --state-dir",
                "daemon --config /etc/hostkeeper/hostkeeper.toml --state-directory",
            ),
            "ExecStart= runs",
        ),
        (
            altered(
                &text,
                "--packages-dir /var/lib/hostkeeper/packages ",
                "--packages-dir ",
            ),
            "ExecStart= runs",
        ),
        (
            altered(&text, "current daemon --config", "current serve --config"),
            "ExecStart= runs",
        ),
        (
            altered(&text, " --notify-systemd\n", "\n"),
            "ExecStart= runs",
        ),
        (
            altered(
                &text,
                "--notify-systemd\n",
                "--notify-systemd ; /usr/bin/true\n",
            ),
            "a second command",
        ),
        (
            altered(&text, "ExecStart=/var/lib/", "ExecStart=-/var/lib/"),
            "has a prefix",
        ),
        (
            altered(&text, "--admin-socket /run", "--admin-socket \"/run"),
            "unclosed",
        ),
        (
            altered(
                &text,
                line_end,
                "--notify-systemd\nExecStart=/var/lib/updater/state/hostkeeper/bin/current daemon\n",
            ),
            "sets ExecStart= 2 times",
        ),
    ];
    refuses(check_command, &cases);
    // A command line continued over two lines is one command line.
    let continued = altered(&text, " --packages-dir", " \\\n    --packages-dir");
    assert_eq!(check_command(&continued), Ok(()));
}

fn is_confining(key: &str) -> bool {
    CONFINING_KEYS.contains(&key) || CONFINING_PREFIXES.iter().any(|p| key.starts_with(p))
}

fn check_root_notify(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let kind = unit.single("Service", "Type")?;
    if kind != "notify" {
        return Err(format!(
            "Type={kind}: the daemon reports ready by sd_notify after its boot pass (design section 6.5), so the unit is Type=notify"
        ));
    }
    if let Some(entry) = unit.entries.iter().find(|e| is_confining(&e.key)) {
        return Err(format!(
            "line {}: {}= takes root or a right of root from the daemon, which runs as root by design",
            entry.line, entry.key
        ));
    }
    let wanted = unit.list("Install", "WantedBy");
    if wanted != ["multi-user.target"] {
        return Err(format!("WantedBy= is {wanted:?}, not multi-user.target"));
    }
    Ok(())
}

/// Catches a unit that runs the daemon as another account or in a sandbox, that is not
/// `Type=notify` (systemd would count the daemon ready before its boot pass), or that a host
/// does not start at boot.
#[test]
fn unit_runs_as_root_with_type_notify() {
    let text = read(UNIT);
    assert_eq!(check_root_notify(&text), Ok(()));
    let after_type = "Type=notify\n";
    let cases = [
        (altered(&text, after_type, "Type=simple\n"), "Type=simple"),
        (altered(&text, after_type, "Type=exec\n"), "Type=exec"),
        (
            altered(&text, after_type, "Type=notify\nUser=hostkeeper\n"),
            "User= takes root",
        ),
        (
            altered(&text, after_type, "Type=notify\nDynamicUser=yes\n"),
            "DynamicUser= takes root",
        ),
        (
            altered(&text, after_type, "Type=notify\nNoNewPrivileges=yes\n"),
            "NoNewPrivileges= takes root",
        ),
        (
            altered(&text, after_type, "Type=notify\nProtectSystem=strict\n"),
            "ProtectSystem= takes root",
        ),
        (
            altered(&text, after_type, "Type=notify\nPrivateTmp=yes\n"),
            "PrivateTmp= takes root",
        ),
        (
            altered(
                &text,
                after_type,
                "Type=notify\nCapabilityBoundingSet=CAP_NET_ADMIN\n",
            ),
            "CapabilityBoundingSet= takes root",
        ),
        (
            altered(
                &text,
                "WantedBy=multi-user.target",
                "WantedBy=default.target",
            ),
            "WantedBy= is",
        ),
        (
            altered(&text, "WantedBy=multi-user.target", "WantedBy="),
            "WantedBy= is",
        ),
    ];
    refuses(check_root_notify, &cases);
}

fn check_limits(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    for key in ["StartLimitBurst", "StartLimitIntervalSec"] {
        if !unit.values("Service", key).is_empty() {
            return Err(format!(
                "{key}= is in [Service]; systemd reads it in [Unit]"
            ));
        }
    }
    let interval = unit.single("Unit", "StartLimitIntervalSec")?;
    if interval != "0" {
        return Err(format!(
            "StartLimitIntervalSec={interval}: the unit has no start limit of its own (0); the self-update drop-in sets it"
        ));
    }
    let restart = unit.single("Service", "Restart")?;
    if restart != "always" {
        return Err(format!(
            "Restart={restart}, not always: the daemon exits 0 to be started again after its own update"
        ));
    }
    let delay = unit.single("Service", "RestartSec")?;
    if delay != "1" {
        return Err(format!(
            "RestartSec={delay}, not 1, the delay of the self-update drop-in"
        ));
    }
    let mode = unit.single("Service", "RestartMode")?;
    if mode != "direct" {
        return Err(format!(
            "RestartMode={mode}, not direct: OnFailure= would run the fallback at every failed start, not at the start limit"
        ));
    }
    let timeout = unit.single("Service", "TimeoutStartSec")?;
    if timeout != "infinity" {
        return Err(format!(
            "TimeoutStartSec={timeout}: systemd would stop a daemon whose boot pass runs longer"
        ));
    }
    check_watchdog(&unit)
}

/// The heartbeat period of the unit's command line, in seconds: its `--heartbeat-secs`, or the
/// binary's default, as the binary's own flag parser reads the line.
fn heartbeat_secs(unit: &Unit) -> Result<u64, String> {
    let words = unit::command_words(unit.single("Service", "ExecStart")?)?;
    Ok(parse_flags(&words)?.link.heartbeat_secs.get())
}

/// The watchdog: `WatchdogSec=` in whole seconds, at least three heartbeat periods of the
/// command line, and longer than one period and the longest heartbeat call together.
fn check_watchdog(unit: &Unit) -> Result<(), String> {
    let value = unit.single("Service", "WatchdogSec")?;
    let watchdog: u64 = value
        .parse()
        .map_err(|_| format!("WatchdogSec={value} is not a whole number of seconds"))?;
    let period = heartbeat_secs(unit)?;
    let least = (3 * period).max(period + CALL_SECS + 1);
    if watchdog < least {
        return Err(format!(
            "WatchdogSec={watchdog} is under {least} seconds: a heartbeat every {period} seconds, each call up to {CALL_SECS}, would be killed while it is healthy"
        ));
    }
    Ok(())
}

/// Catches a start limit in the unit itself (which, before the daemon has first been ready and
/// written the self-update drop-in, would leave a daemon that fails three starts failed with no
/// fallback to start it again), limits put where systemd does not read them, a restart policy
/// that does not restart a daemon that exited cleanly for its own update (`on-failure`, the
/// policy that left a self-updated host down), a restart delay other than the drop-in's, a start
/// timeout that would stop a daemon during a long boot pass (design section 6.5), and a
/// watchdog that is missing, not in whole seconds, or too short for the heartbeat the command
/// line runs: under three periods, or under one period and the longest heartbeat call.
#[test]
fn unit_restarts_always_with_no_start_limit_of_its_own_and_a_watchdog_over_the_heartbeat() {
    let text = read(UNIT);
    assert_eq!(check_limits(&text), Ok(()));
    let limit = "StartLimitIntervalSec=0\n";
    let moved = altered(
        &altered(&text, limit, ""),
        "Restart=always\n",
        &format!("Restart=always\n{limit}"),
    );
    let flags = "--notify-systemd\n";
    let cases = [
        (
            altered(
                &text,
                "StartLimitIntervalSec=0",
                "StartLimitIntervalSec=300",
            ),
            "StartLimitIntervalSec=300: the unit has no start limit",
        ),
        (altered(&text, limit, ""), "has no StartLimitIntervalSec="),
        (moved, "is in [Service]"),
        (
            altered(&text, "TimeoutStartSec=infinity", "TimeoutStartSec=90"),
            "TimeoutStartSec=90",
        ),
        (
            altered(&text, "TimeoutStartSec=infinity\n", ""),
            "has no TimeoutStartSec=",
        ),
        (
            altered(&text, "Restart=always", "Restart=on-failure"),
            "Restart=on-failure, not always",
        ),
        (
            altered(&text, "Restart=always", "Restart=no"),
            "Restart=no, not always",
        ),
        (
            altered(&text, "RestartSec=1", "RestartSec=30"),
            "RestartSec=30, not 1",
        ),
        (altered(&text, "RestartSec=1\n", ""), "has no RestartSec="),
        (
            altered(&text, "RestartMode=direct\n", "RestartMode=normal\n"),
            "RestartMode=normal, not direct",
        ),
        (
            altered(&text, "RestartMode=direct\n", ""),
            "has no RestartMode=",
        ),
        (
            altered(&text, "WatchdogSec=60\n", ""),
            "has no WatchdogSec=",
        ),
        (
            altered(&text, "WatchdogSec=60", "WatchdogSec=1min"),
            "WatchdogSec=1min is not a whole number",
        ),
        (
            altered(&text, "WatchdogSec=60", "WatchdogSec=44"),
            "WatchdogSec=44 is under 45 seconds",
        ),
        (
            altered(&text, flags, "--notify-systemd --heartbeat-secs 30\n"),
            "WatchdogSec=60 is under 90 seconds",
        ),
        (
            altered(&text, flags, "--notify-systemd --heartbeat-secs 0\n"),
            "--heartbeat-secs",
        ),
    ];
    refuses(check_limits, &cases);
    // Three periods is enough, at the default heartbeat and at one the command line sets.
    let at_three = altered(&text, "WatchdogSec=60", "WatchdogSec=45");
    assert_eq!(check_limits(&at_three), Ok(()));
    let set = altered(&text, flags, "--notify-systemd --heartbeat-secs 20\n");
    assert_eq!(check_limits(&set), Ok(()));
    // A short heartbeat needs a watchdog over one period and the longest call, not three
    // periods alone.
    let short = altered(
        &altered(&text, flags, "--notify-systemd --heartbeat-secs 2\n"),
        "WatchdogSec=60",
        "WatchdogSec=12",
    );
    assert_eq!(
        check_limits(&short),
        Err(format!(
            "WatchdogSec=12 is under 13 seconds: a heartbeat every 2 seconds, each call up to {CALL_SECS}, would be killed while it is healthy"
        ))
    );
    assert_eq!(
        check_limits(&altered(&short, "WatchdogSec=12", "WatchdogSec=13")),
        Ok(())
    );
}

/// Keys that would start a kbf unit with this one, or tie this unit's life to it.
const PULLING_KEYS: [&str; 6] = [
    "Wants",
    "Requires",
    "Requisite",
    "BindsTo",
    "PartOf",
    "Upholds",
];

fn check_conflicts(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let conflicts = unit.list("Unit", "Conflicts");
    if conflicts != KBF_UNITS {
        return Err(format!(
            "Conflicts= is {conflicts:?}, not kbf's units {KBF_UNITS:?}"
        ));
    }
    for key in PULLING_KEYS {
        if let Some(name) = unit
            .list("Unit", key)
            .into_iter()
            .find(|name| KBF_UNITS.contains(&name.as_str()))
        {
            return Err(format!(
                "{key}= names {name}, which pulls in a unit this one conflicts with"
            ));
        }
    }
    Ok(())
}

/// Catches a unit that could run beside kbf's update agent or its root half (design section
/// 12.3): a kbf unit missing from `Conflicts=` or misspelled there, or named in a key that
/// would start it with this unit.
#[test]
fn the_unit_conflicts_with_kbfs_updater_units() {
    let text = read(UNIT);
    assert_eq!(check_conflicts(&text), Ok(()));
    let all = KBF_UNITS.join(" ");
    let conflicts = format!("Conflicts={all}");
    let cases = [
        (
            altered(
                &text,
                &conflicts,
                "Conflicts=kbf-updater-agent.service kbf-host-local.service",
            ),
            "Conflicts= is",
        ),
        (
            altered(
                &text,
                "Conflicts=kbf-updater-agent.service",
                "Conflicts=kbf-update-agent.service",
            ),
            "Conflicts= is",
        ),
        (altered(&text, &conflicts, "Conflicts="), "Conflicts= is"),
        (
            altered(&text, &conflicts, &format!("Before={all}")),
            "Conflicts= is",
        ),
        (
            altered(
                &text,
                "Wants=network-online.target",
                "Wants=network-online.target kbf-host-local.service",
            ),
            "Wants= names kbf-host-local.service",
        ),
        (
            altered(
                &text,
                "Wants=network-online.target\n",
                "Wants=network-online.target\nBindsTo=kbf-updater-agent.service\n",
            ),
            "BindsTo= names kbf-updater-agent.service",
        ),
    ];
    refuses(check_conflicts, &cases);
}

fn check_handoff(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let ordered: Vec<String> = unit
        .list("Unit", "After")
        .into_iter()
        .chain(unit.list("Unit", "Before"))
        .collect();
    for name in unit.list("Unit", "Conflicts") {
        if !ordered.contains(&name) {
            return Err(format!(
                "{name} is in Conflicts= but in neither After= nor Before=, so systemd may start one while the other is still stopping"
            ));
        }
    }
    Ok(())
}

/// Catches a handoff between hostkeeper and kbf's units that could overlap. systemd orders the
/// stop job of one unit before the start job of another only when an ordering (`After=` or
/// `Before=`, either one, either direction) links them; `Conflicts=` alone queues both jobs at
/// once. So every unit this one conflicts with must also be ordered against it.
#[test]
fn the_handoff_with_kbfs_units_stops_one_before_starting_the_other() {
    let text = read(UNIT);
    assert_eq!(check_handoff(&text), Ok(()));
    let after = format!("After={}\n", KBF_UNITS.join(" "));
    // Ordered by Before= instead of After=: the stop still comes first.
    let before = altered(&text, &after, &after.replace("After=", "Before="));
    assert_eq!(check_handoff(&before), Ok(()));
    let cases = [
        (
            altered(&text, &after, ""),
            "kbf-updater-agent.service is in Conflicts= but in neither",
        ),
        (
            altered(
                &text,
                &after,
                "After=kbf-updater-agent.service kbf-host-local.service\n",
            ),
            "kbf-host-local-frontend.service is in Conflicts= but in neither",
        ),
        (
            altered(&text, &after, "After=\n"),
            "kbf-updater-agent.service is in Conflicts= but in neither",
        ),
    ];
    refuses(check_handoff, &cases);
}

fn check_unit_keys(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    for entry in &unit.entries {
        let known = UNIT_KEYS
            .iter()
            .find(|(section, _)| *section == entry.section)
            .map_or(&[][..], |(_, keys)| *keys);
        if !known.contains(&entry.key.as_str()) {
            return Err(format!(
                "line {}: [{}] {}= is not a key of this unit",
                entry.line, entry.section, entry.key
            ));
        }
    }
    for (section, keys) in UNIT_KEYS {
        if let Some(key) = keys.iter().find(|key| unit.values(section, key).is_empty()) {
            return Err(format!("[{section}] has no {key}="));
        }
    }
    Ok(())
}

/// Catches a key the design does not name: a generated `OnFailure=` (the self-update fallback
/// of section 6.6 is a drop-in the daemon writes), a timeout or a section added without a test,
/// or a key the unit lost.
#[test]
fn unit_holds_only_the_keys_the_design_names() {
    let text = read(UNIT);
    assert_eq!(check_unit_keys(&text), Ok(()));
    let cases = [
        (
            altered(
                &text,
                "StartLimitIntervalSec=0\n",
                "StartLimitIntervalSec=0\nOnFailure=hostkeeper-fallback.service\n",
            ),
            "[Unit] OnFailure= is not",
        ),
        (
            altered(
                &text,
                "StartLimitIntervalSec=0\n",
                "StartLimitIntervalSec=0\nStartLimitBurst=3\n",
            ),
            "[Unit] StartLimitBurst= is not",
        ),
        (
            altered(
                &text,
                "WatchdogSec=60\n",
                "WatchdogSec=60\nWatchdogSignal=SIGKILL\n",
            ),
            "[Service] WatchdogSignal= is not",
        ),
        (
            altered(&text, "WatchdogSec=60\n", ""),
            "[Service] has no WatchdogSec=",
        ),
        (
            altered(
                &text,
                "Restart=always\n",
                "Restart=always\nRestartPreventExitStatus=0\n",
            ),
            "[Service] RestartPreventExitStatus= is not",
        ),
        (
            altered(
                &text,
                "[Install]\n",
                "[Socket]\nListenStream=/run/hostkeeper/admin.sock\n\n[Install]\n",
            ),
            "[Socket] ListenStream= is not",
        ),
        (
            altered(&text, "Wants=network-online.target\n", ""),
            "[Unit] has no Wants=",
        ),
        (
            altered(&text, "RuntimeDirectoryMode=0700\n", ""),
            "[Service] has no RuntimeDirectoryMode=",
        ),
    ];
    refuses(check_unit_keys, &cases);
}
