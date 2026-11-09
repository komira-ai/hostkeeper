//! The daemon's two units, the bootstrap `hostkeeper.service` and the package `units/hostkeeper.service`,
//! each held to the checks of a unit: its command lines, its account, its restart policy, its lack
//! of a start limit of its own and its watchdog, the units it conflicts with and the keys it sets.

use hostkeeper_link::CALL_SECS;

use crate::daemon_flags::parse_flags;
use crate::unit::{self, Unit};
use crate::{Kind, altered, refuses};

/// The daemon's command line after its program: the `daemon` verb with each path of design
/// sections 4.1 and 10 spelled out as a flag, and the flag that makes it report ready to systemd.
/// No `--admin-socket`: the binary has no such flag. The program is [`Kind::program`]: `current`
/// of the binary directory of hostkeeper's own update in the package unit (`self_update.rs` holds
/// it to the daemon's `binary_dir`), the installed binary in the bootstrap unit.
/// `daemon_flags.rs` parses this line with the binary's own flag parser.
const DAEMON_ARGS: [&str; 8] = [
    "daemon",
    "--config",
    "/etc/hostkeeper/hostkeeper.toml",
    "--state-dir",
    "/var/lib/updater/state",
    "--packages-dir",
    "/var/lib/hostkeeper/packages",
    "--notify-systemd",
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

/// Every key a unit sets, by section. No unit sets `ExecStartPre` (the binary has no seed verb: the
/// start of `/usr/bin/hostkeeper daemon` seeds) or `RuntimeDirectory*` (the tmpfiles rules make the
/// run directory).
fn unit_keys() -> Vec<(&'static str, Vec<&'static str>)> {
    let service = vec![
        "Type",
        "TimeoutStartSec",
        "ExecStart",
        "Restart",
        "RestartSec",
        "RestartMode",
        "WatchdogSec",
    ];
    vec![
        (
            "Unit",
            vec![
                "Description",
                "Documentation",
                "After",
                "Wants",
                "Conflicts",
                "StartLimitIntervalSec",
            ],
        ),
        ("Service", service),
        ("Install", vec!["WantedBy"]),
    ]
}

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

fn check_command(kind: Kind, text: &str) -> Result<(), String> {
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
        if key.starts_with("Exec") && key != "ExecStart" {
            return Err(format!(
                "line {}: {key}= runs a command besides the daemon",
                entry.line
            ));
        }
    }
    let words = unit::command_words(unit.single("Service", "ExecStart")?)?;
    let wanted: Vec<&str> = std::iter::once(kind.program()).chain(DAEMON_ARGS).collect();
    if words != wanted {
        return Err(format!("ExecStart= runs {words:?}, not {wanted:?}"));
    }
    Ok(())
}

/// Catches a unit whose command line is not the daemon's: a program other than the unit's (the
/// installed binary in the package unit, `current` in the bootstrap unit, `previous`), a renamed
/// or dropped flag, an `--admin-socket`, a flag without its value, another verb, a second
/// command, a prefix that ignores a failure, a value taken from the environment, or any
/// environment set for the daemon; and a unit that runs any command before the start.
#[test]
fn each_units_command_line_is_the_daemon_with_its_paths_and_no_environment() {
    let line_end = "--notify-systemd\n";
    for kind in Kind::ALL {
        let text = kind.read();
        assert_eq!(check_command(kind, &text), Ok(()));
        let program = kind.program();
        let other = match kind {
            Kind::Bootstrap => Kind::Package.program(),
            Kind::Package => Kind::Bootstrap.program(),
        };
        let start = format!("ExecStart={program} daemon");
        let mut cases = vec![
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
                altered(
                    &text,
                    &start,
                    "ExecStart=/var/lib/updater/state/hostkeeper/bin/previous daemon",
                ),
                "ExecStart= runs",
            ),
            (
                altered(&text, &start, &format!("ExecStart={other} daemon")),
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
                altered(&text, " daemon --config", " serve --config"),
                "ExecStart= runs",
            ),
            (
                altered(&text, " --notify-systemd\n", "\n"),
                "ExecStart= runs",
            ),
            (
                altered(
                    &text,
                    " --notify-systemd\n",
                    " --notify-systemd --admin-socket /run/hostkeeper/admin.sock\n",
                ),
                "ExecStart= runs",
            ),
            (
                altered(&text, line_end, "--notify-systemd ; /usr/bin/true\n"),
                "a second command",
            ),
            (
                altered(&text, &start, &format!("ExecStart=-{program} daemon")),
                "has a prefix",
            ),
            (
                altered(&text, "--config /etc", "--config \"/etc"),
                "unclosed",
            ),
            (
                altered(
                    &text,
                    line_end,
                    &format!("--notify-systemd\nExecStart={program} daemon\n"),
                ),
                "sets ExecStart= 2 times",
            ),
        ];
        cases.extend([
            (
                altered(&text, "ExecStart=", "ExecStartPre=-/usr/bin/true\nExecStart="),
                "ExecStartPre= runs a command besides the daemon",
            ),
            (
                altered(
                    &text,
                    "ExecStart=",
                    "ExecStartPre=-/usr/bin/hostkeeper self-update-seed --state-dir /var/lib/updater/state --binary /usr/bin/hostkeeper\nExecStart=",
                ),
                "ExecStartPre= runs a command besides the daemon",
            ),
        ]);
        refuses(|t: &str| check_command(kind, t), &cases);
        // A command line continued over two lines is one command line.
        let continued = altered(&text, " --packages-dir", " \\\n    --packages-dir");
        assert_eq!(check_command(kind, &continued), Ok(()));
    }
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
fn each_unit_runs_as_root_with_type_notify() {
    for kind in Kind::ALL {
        let text = kind.read();
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
            "StartLimitIntervalSec={interval}: the unit has no start limit of its own (0)"
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
            "RestartSec={delay}, not 1, the second between the tries of a hand-over"
        ));
    }
    let mode = unit.single("Service", "RestartMode")?;
    if mode != "direct" {
        return Err(format!(
            "RestartMode={mode}, not direct: an automatic restart goes straight to the next start without the failed state"
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

/// Catches a start limit in the unit itself (which would leave a daemon that fails its starts
/// failed, with nothing to start it again), limits put where systemd does not read them, a restart policy
/// that does not restart a daemon that exited cleanly for its own update (`on-failure`, the
/// policy that left a self-updated host down), a restart delay other than one second, a start
/// timeout that would stop a daemon during a long boot pass (design section 6.5), and a
/// watchdog that is missing, not in whole seconds, or too short for the heartbeat the command
/// line runs: under three periods, or under one period and the longest heartbeat call.
#[test]
fn each_unit_restarts_always_with_no_start_limit_of_its_own_and_a_watchdog_over_the_heartbeat() {
    for kind in Kind::ALL {
        let text = kind.read();
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
fn each_unit_conflicts_with_kbfs_updater_units() {
    for kind in Kind::ALL {
        let text = kind.read();
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
fn each_units_handoff_with_kbfs_units_stops_one_before_starting_the_other() {
    for kind in Kind::ALL {
        let text = kind.read();
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
}

fn check_unit_keys(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let allowed = unit_keys();
    for entry in &unit.entries {
        let known = allowed
            .iter()
            .find(|(section, _)| *section == entry.section)
            .map_or(&[][..], |(_, keys)| keys.as_slice());
        if !known.contains(&entry.key.as_str()) {
            return Err(format!(
                "line {}: [{}] {}= is not a key of this unit",
                entry.line, entry.section, entry.key
            ));
        }
    }
    for (section, keys) in &allowed {
        if let Some(key) = keys.iter().find(|key| unit.values(section, key).is_empty()) {
            return Err(format!("[{section}] has no {key}="));
        }
    }
    Ok(())
}

/// Catches, in either unit, a key the design does not name: a generated `OnFailure=` (the update has no
/// fallback unit), a timeout or a section added without a test,
/// a key the unit lost, `RuntimeDirectory=` (the run directory is the tmpfiles rules'), and an
/// `ExecStartPre=` seed line.
#[test]
fn each_unit_holds_only_the_keys_the_design_names() {
    for kind in Kind::ALL {
        let text = kind.read();
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
                altered(
                    &text,
                    "WatchdogSec=60\n",
                    "WatchdogSec=60\nRuntimeDirectory=hostkeeper\n",
                ),
                "[Service] RuntimeDirectory= is not",
            ),
            (
                altered(
                    &text,
                    "WatchdogSec=60\n",
                    "WatchdogSec=60\nRuntimeDirectoryMode=0700\n",
                ),
                "[Service] RuntimeDirectoryMode= is not",
            ),
        ];
        let mut cases = cases.to_vec();
        cases.push((
            altered(
                &text,
                "ExecStart=",
                "ExecStartPre=-/usr/bin/true\nExecStart=",
            ),
            "[Service] ExecStartPre= is not",
        ));
        refuses(check_unit_keys, &cases);
    }
}
