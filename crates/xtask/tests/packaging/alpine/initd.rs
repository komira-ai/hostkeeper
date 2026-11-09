//! `packaging/alpine/hostkeeper.initd`, the OpenRC init script, read in the small part of
//! OpenRC's shell it is written in and held to the systemd unit and the tmpfiles rules it
//! stands in for.

use std::collections::BTreeMap;
use std::path::Path;

use super::{INITD, read_alpine};
use crate::tmpfiles;
use crate::unit::{self, Unit};
use crate::{TMPFILES, UNIT, altered, read, refuses};

/// The first line of an init script.
const SHEBANG: &str = "#!/sbin/openrc-run";

/// The variables the init script sets. `command_user` and the others that drop root or confine
/// the daemon are not among them: the daemon runs as root with no sandbox, as the unit does.
const VARIABLES: [&str; 8] = [
    "description",
    "supervisor",
    "command",
    "command_args",
    "respawn_delay",
    "respawn_max",
    "output_log",
    "error_log",
];

/// The variables that have one value: how supervise-daemon runs the daemon. The program is the
/// binary the package installed, which execs `current` itself; the daemon exits 0 to be started
/// again after its own update, so it is respawned for good (`respawn_max=0`), a second after.
const FIXED: [(&str, &str); 4] = [
    ("supervisor", "supervise-daemon"),
    ("command", "/usr/bin/hostkeeper"),
    ("respawn_delay", "1"),
    ("respawn_max", "0"),
];

/// The functions of the init script.
const FUNCTIONS: [&str; 3] = ["depend", "start_pre", "start_post"];

/// The ready file the daemon writes (`--ready-file`), which `start_pre` removes and `start_post`
/// waits for. The systemd unit has `--notify-systemd` where the init script has
/// `--ready-file` and this path.
const READY_FILE: &str = "/run/hostkeeper/ready";

/// The directories `start_pre` makes beyond those of the tmpfiles rules: the parents that
/// `checkpath` does not make, at the mode and owner of a directory made by default.
const PARENTS: [&str; 2] = ["/var/lib/hostkeeper", "/var/lib/updater"];

/// An init script: its variables and the lines of its functions.
struct Initd {
    vars: BTreeMap<String, String>,
    functions: BTreeMap<String, Vec<String>>,
}

fn is_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// The value of an assignment: bare, or between double quotes. Only letters, digits, spaces and
/// `,._/:=+-` are in it, so there is no variable, substitution, quote or redirection.
fn value_of(raw: &str, n: usize) -> Result<String, String> {
    let (inner, quoted) = match raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(inner) => (inner, true),
        None => (raw, false),
    };
    if let Some(bad) = inner
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || " ,._/:=+-".contains(*c)))
    {
        return Err(format!(
            "line {n}: the character {bad:?} is outside the init script's grammar (letters, digits, spaces and `,._/:=+-`): no variable, substitution, quote or redirection"
        ));
    }
    if !quoted && inner.contains(' ') {
        return Err(format!("line {n}: a value with a space is not quoted"));
    }
    Ok(inner.to_owned())
}

impl Initd {
    /// Reads `text`: the shebang, then comments, blank lines, `name=value` assignments and
    /// functions (`name() {`, its lines, and `}` alone on a line). Anything else is refused with
    /// its line.
    fn parse(text: &str) -> Result<Initd, String> {
        let mut lines = text.lines().enumerate();
        match lines.next() {
            Some((_, first)) if first == SHEBANG => {}
            _ => {
                return Err(format!(
                    "line 1: the script does not start with `{SHEBANG}`"
                ));
            }
        }
        let mut vars = BTreeMap::new();
        let mut functions = BTreeMap::new();
        let mut open: Option<(String, Vec<String>)> = None;
        for (at, raw) in lines {
            let n = at + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match open.take() {
                Some((name, body)) if raw == "}" => {
                    functions.insert(name, body);
                }
                Some((name, mut body)) => {
                    body.push(line.to_owned());
                    open = Some((name, body));
                }
                None => {
                    if let Some(name) = line.strip_suffix("() {") {
                        if !is_name(name) || functions.contains_key(name) {
                            return Err(format!(
                                "line {n}: the function {name:?} is not a new name"
                            ));
                        }
                        open = Some((name.to_owned(), Vec::new()));
                        continue;
                    }
                    let (name, value) = line.split_once('=').ok_or_else(|| {
                        format!("line {n}: not an assignment or a function: {line:?}")
                    })?;
                    if !is_name(name) {
                        return Err(format!("line {n}: {name:?} is not a variable name"));
                    }
                    if vars.insert(name.to_owned(), value_of(value, n)?).is_some() {
                        return Err(format!("line {n}: the init script sets {name} twice"));
                    }
                }
            }
        }
        if open.is_some() {
            return Err("a function is not closed by a `}` on its own line".to_owned());
        }
        Ok(Initd { vars, functions })
    }
}

/// The init script sets only [`VARIABLES`], each of [`FIXED`] to its value, and defines only
/// [`FUNCTIONS`]. It does not set `notify`: supervise-daemon's wait for it has no end.
fn check_variables(initd: &Initd) -> Result<(), String> {
    if initd.vars.contains_key("notify") {
        return Err(
            "the init script sets notify: supervise-daemon's wait for the notice has no end when the daemon exits before it is ready, which would hold the runlevel; start_post waits for the ready file instead"
                .to_owned(),
        );
    }
    if let Some(name) = initd.vars.keys().find(|k| !VARIABLES.contains(&k.as_str())) {
        return Err(format!(
            "the init script sets {name}, which it does not use: the daemon runs as root with no sandbox, and its configuration is its flags"
        ));
    }
    for (name, wanted) in FIXED {
        match initd.vars.get(name) {
            Some(value) if value == wanted => {}
            other => return Err(format!("{name} is {other:?}, not {wanted:?}")),
        }
    }
    if let Some(name) = initd
        .functions
        .keys()
        .find(|k| !FUNCTIONS.contains(&k.as_str()))
    {
        return Err(format!(
            "the init script defines {name}, which it does not use"
        ));
    }
    Ok(())
}

/// `command_args` is, word for word, the arguments of the unit's `ExecStart=` after its program,
/// with `--notify-systemd` replaced by `--ready-file` and [`READY_FILE`].
fn check_arguments(initd: &Initd, unit_text: &str) -> Result<(), String> {
    let unit = Unit::parse(unit_text)?;
    let exec = unit::command_words(unit.single("Service", "ExecStart")?)?;
    let wanted: Vec<&str> = exec
        .iter()
        .skip(1)
        .flat_map(|word| match word.as_str() {
            "--notify-systemd" => vec!["--ready-file", READY_FILE],
            other => vec![other],
        })
        .collect();
    let args = initd.vars.get("command_args").map_or("", String::as_str);
    let words: Vec<&str> = args.split_whitespace().collect();
    if words != wanted {
        return Err(format!(
            "command_args is {words:?}, not the arguments of the unit's ExecStart=: {wanted:?}"
        ));
    }
    if !words.contains(&"--ready-file") {
        return Err(
            "command_args lacks --ready-file, so the daemon writes no ready file and start_post waits for it with no end"
                .to_owned(),
        );
    }
    Ok(())
}

/// The ready file `command_args` names: the word after `--ready-file`.
fn ready_file(initd: &Initd) -> Result<String, String> {
    let args = initd.vars.get("command_args").map_or("", String::as_str);
    let mut words = args.split_whitespace();
    words
        .find(|word| *word == "--ready-file")
        .and_then(|_| words.next())
        .map(str::to_owned)
        .ok_or_else(|| "command_args names no --ready-file".to_owned())
}

/// A directory `start_pre` makes: its path, mode and owner as `checkpath` is given them.
type Directory = (String, String, String);

/// The directories `start_pre` makes, in order, each `checkpath --directory --mode <mode>
/// --owner <owner> <path>...` line giving one for every path.
fn made_directories(body: &[String]) -> Result<Vec<Directory>, String> {
    let mut out = Vec::new();
    for line in body {
        let words: Vec<&str> = line.split_whitespace().collect();
        let [
            "checkpath",
            "--directory",
            "--mode",
            mode,
            "--owner",
            owner,
            paths @ ..,
        ] = words.as_slice()
        else {
            return Err(format!(
                "start_pre runs `{line}`, not `checkpath --directory --mode <mode> --owner <owner> <path>...`"
            ));
        };
        if paths.is_empty() || paths.iter().any(|path| !path.starts_with('/')) {
            return Err(format!(
                "start_pre: `{line}` names no path or a relative one"
            ));
        }
        for path in paths {
            out.push(((*path).to_owned(), (*mode).to_owned(), (*owner).to_owned()));
        }
    }
    Ok(out)
}

/// The directories of the tmpfiles rules with their modes and owners, and [`PARENTS`], sorted.
fn tmpfiles_directories(tmpfiles_text: &str) -> Result<Vec<Directory>, String> {
    let mut out = Vec::new();
    for line in tmpfiles::parse(tmpfiles_text)? {
        if line.kind != "d" {
            return Err(format!(
                "line {}: the tmpfiles rule is of type {}, not d (a directory)",
                line.line, line.kind
            ));
        }
        out.push((
            line.path,
            line.mode,
            format!("{}:{}", line.user, line.group),
        ));
    }
    for parent in PARENTS {
        out.push((parent.to_owned(), "0755".to_owned(), "root:root".to_owned()));
    }
    out.sort();
    Ok(out)
}

/// `start_pre` makes exactly the directories of the tmpfiles rules and their parents, with the
/// rules' modes and owners, each parent before the directory in it.
fn check_directories(initd: &Initd, tmpfiles_text: &str) -> Result<(), String> {
    let body = initd
        .functions
        .get("start_pre")
        .ok_or("the init script has no start_pre")?;
    let ready = ready_file(initd)?;
    let rm = format!("rm -f {ready}");
    let last = body.last().map_or("", String::as_str);
    let checkpaths = &body[..body.len().saturating_sub(1)];
    if last != rm {
        return Err(format!(
            "start_pre ends with `{last}`, not `{rm}`: the ready file of the last start must be gone before the daemon starts, or start_post would return at once"
        ));
    }
    let made = made_directories(checkpaths)?;
    let mut sorted = made.clone();
    sorted.sort();
    let wanted = tmpfiles_directories(tmpfiles_text)?;
    if sorted != wanted {
        return Err(format!(
            "start_pre makes {sorted:?}, not the directories of the tmpfiles rules and their parents: {wanted:?}"
        ));
    }
    for (at, (path, _, _)) in made.iter().enumerate() {
        let parent = Path::new(path)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if made.iter().position(|(other, _, _)| *other == parent) > Some(at) {
            return Err(format!("start_pre makes {path} before its parent {parent}"));
        }
    }
    Ok(())
}

/// The init script runs after the network, as the unit is ordered after network-online.target.
fn check_depend(initd: &Initd) -> Result<(), String> {
    let body = initd.functions.get("depend");
    if body.is_none_or(|lines| lines.as_slice() != ["after net"]) {
        return Err(format!("depend is {body:?}, not `after net`"));
    }
    Ok(())
}

/// `start_post` is, line for line, the wait for the ready file of `command_args`, which stops
/// with an error said once supervise-daemon has started the daemon a second time.
fn check_start_post(initd: &Initd) -> Result<(), String> {
    let ready = ready_file(initd)?;
    let wanted = [
        format!("while [ ! -e {ready} ]; do"),
        "if [ \"$(service_get_value start_count)\" -gt 1 ]; then".to_owned(),
        "eerror \"hostkeeper ended before it was ready; supervise-daemon starts it again (see /var/log/hostkeeper.log)\"".to_owned(),
        "return 0".to_owned(),
        "fi".to_owned(),
        "sleep 1".to_owned(),
        "done".to_owned(),
    ];
    let body = initd.functions.get("start_post");
    if body.is_none_or(|lines| lines.as_slice() != wanted) {
        return Err(format!(
            "start_post is {body:?}, not the wait for {ready} that gives up once the start count is past 1: {wanted:?}"
        ));
    }
    Ok(())
}

fn check_initd(text: &str, unit_text: &str, tmpfiles_text: &str) -> Result<(), String> {
    let initd = Initd::parse(text)?;
    check_variables(&initd)?;
    check_arguments(&initd, unit_text)?;
    check_depend(&initd)?;
    check_directories(&initd, tmpfiles_text)?;
    check_start_post(&initd)
}

/// Catches an init script that drifts from the systemd unit and the tmpfiles rules it stands
/// in for: a flag, path or order of the daemon's arguments that is not the unit's, no
/// `--ready-file`, a `notify=`, a supervisor, program, notify mode or respawn setting other than the
/// design's, a variable that drops root or confines the daemon, a directory that is not one of
/// the rules' (or their parent) or is made with another mode or owner, a parent made after the
/// directory in it, and a line of a language other than the small part of OpenRC's it is
/// written in.
#[test]
fn init_script_runs_the_units_flags_and_makes_the_tmpfiles_directories() {
    let initd = read_alpine(INITD);
    let unit = read(UNIT);
    let tmpfiles = read(TMPFILES);
    assert_eq!(check_initd(&initd, &unit, &tmpfiles), Ok(()));
    let cases = [
        (
            altered(&initd, "#!/sbin/openrc-run\n", "#!/bin/sh\n"),
            "does not start with `#!/sbin/openrc-run`",
        ),
        (
            altered(
                &initd,
                "\nsupervisor=supervise-daemon\n",
                "\nsupervisor=start-stop-daemon\n",
            ),
            "supervisor is Some(\"start-stop-daemon\")",
        ),
        (
            altered(&initd, "\nsupervisor=supervise-daemon\n", "\n"),
            "supervisor is None",
        ),
        (
            altered(
                &initd,
                "\ncommand=/usr/bin/hostkeeper\n",
                "\ncommand=/var/lib/updater/state/hostkeeper/bin/current\n",
            ),
            "command is Some(\"/var/lib/updater/state/hostkeeper/bin/current\")",
        ),
        (
            altered(
                &initd,
                "\nrespawn_delay=1\n",
                "\nrespawn_delay=1\nnotify=\"socket:ready\"\n",
            ),
            "sets notify: supervise-daemon's wait for the notice has no end",
        ),
        (
            altered(
                &initd,
                "\nrespawn_delay=1\n",
                "\nrespawn_delay=1\nnotify=fd:3\n",
            ),
            "sets notify: supervise-daemon's wait",
        ),
        (
            altered(&initd, "\nrespawn_max=0\n", "\nrespawn_max=3\n"),
            "respawn_max is Some(\"3\")",
        ),
        (
            altered(&initd, "\nrespawn_delay=1\n", "\nrespawn_delay=5\n"),
            "respawn_delay is Some(\"5\")",
        ),
        (
            altered(
                &initd,
                "\nrespawn_max=0\n",
                "\nrespawn_max=0\nrespawn_max=0\n",
            ),
            "sets respawn_max twice",
        ),
        (
            altered(
                &initd,
                "\nrespawn_max=0\n",
                "\nrespawn_max=0\ncommand_user=hostkeeper\n",
            ),
            "sets command_user",
        ),
        (
            altered(
                &initd,
                "\nrespawn_max=0\n",
                "\nrespawn_max=0\nrc_ulimit=-n_1024\n",
            ),
            "sets rc_ulimit",
        ),
        (
            altered(
                &initd,
                "\noutput_log=/var/log/hostkeeper.log\n",
                "\noutput_log=$RC_SVCNAME.log\n",
            ),
            "the character '$' is outside",
        ),
        (
            altered(
                &initd,
                "\nrespawn_delay=",
                "\nexport RUST_LOG=debug\nrespawn_delay=",
            ),
            "\"export RUST_LOG\" is not a variable name",
        ),
        (
            altered(
                &initd,
                "description=\"hostkeeper, the update daemon of this host\"",
                "description=hostkeeper, the update daemon of this host",
            ),
            "a value with a space is not quoted",
        ),
        (
            altered(
                &initd,
                "\ndescription=\"hostkeeper,",
                "\ndescription=\"hostkeeper;",
            ),
            "the character ';' is outside",
        ),
        (
            altered(&initd, "\nrespawn_max=0\n", "\nrespawn_max 0\n"),
            "not an assignment or a function",
        ),
        (
            altered(&initd, "\tafter net\n", "\tneed net\n"),
            "depend is Some([\"need net\"])",
        ),
        (
            format!("{initd}\nstop_post() {{\n\ttrue\n}}\n"),
            "defines stop_post",
        ),
        (
            format!("{initd}\nstop_post() {{\n\ttrue\n}}\nstop_post() {{\n\ttrue\n}}\n"),
            "the function \"stop_post\" is not a new name",
        ),
        (
            format!("{initd}\nStop() {{\n"),
            "the function \"Stop\" is not a new name",
        ),
        (
            altered(&initd, "\t\tsleep 1\n\tdone\n}", "\t\tsleep 1\n\tdone\n"),
            "a function is not closed",
        ),
        (
            altered(
                &initd,
                "\tcheckpath --directory --mode 0700",
                "\trm -rf /var/lib/updater\n\tcheckpath --directory --mode 0700",
            ),
            "start_pre runs `rm -rf /var/lib/updater`",
        ),
        (
            altered(&initd, "--directory --mode 0700", "--directory --mode"),
            "start_pre runs",
        ),
        (
            altered(
                &initd,
                "\tcheckpath --directory --mode 0700 --owner root:root /run/hostkeeper /var/lib/updater/state /var/lib/updater/hooks\n",
                "\tcheckpath --directory --mode 0700 --owner root:root\n",
            ),
            "names no path or a relative one",
        ),
        (
            altered(
                &initd,
                "/var/lib/updater\n\tcheckpath",
                "var/lib/updater\n\tcheckpath",
            ),
            "names no path or a relative one",
        ),
        (
            altered(
                &initd,
                "--mode 0700 --owner root:root /run",
                "--mode 0755 --owner root:root /run",
            ),
            "start_pre makes",
        ),
        (
            altered(
                &initd,
                "--mode 0755 --owner root:root /etc",
                "--mode 0700 --owner root:root /etc",
            ),
            "start_pre makes",
        ),
        (
            altered(
                &initd,
                "--mode 0700 --owner root:root /run",
                "--mode 0700 --owner hostkeeper:hostkeeper /run",
            ),
            "start_pre makes",
        ),
        (
            altered(&initd, " /var/lib/updater/hooks\n", "\n"),
            "start_pre makes",
        ),
        (
            altered(
                &initd,
                " /var/lib/updater/hooks\n",
                " /var/lib/updater/hooks /var/lib/other\n",
            ),
            "start_pre makes",
        ),
        (
            altered(
                &initd,
                " /var/lib/hostkeeper/packages /var/lib/updater\n",
                " /var/lib/hostkeeper/packages\n",
            ),
            "start_pre makes",
        ),
        (
            altered(
                &initd,
                "/etc/hostkeeper /var/lib/hostkeeper /var/lib/hostkeeper/packages /var/lib/updater",
                "/etc/hostkeeper /var/lib/hostkeeper/packages /var/lib/hostkeeper /var/lib/updater",
            ),
            "start_pre makes /var/lib/hostkeeper/packages before its parent /var/lib/hostkeeper",
        ),
        (
            altered(&initd, "start_pre() {", "start_post() {"),
            "the function \"start_post\" is not a new name",
        ),
        (
            altered(&initd, "start_pre() {", "stop() {"),
            "defines stop, which it does not use",
        ),
        (
            altered(&initd, "\trm -f /run/hostkeeper/ready\n}", "}"),
            "start_pre ends with `checkpath",
        ),
        (
            altered(
                &initd,
                "\trm -f /run/hostkeeper/ready\n}",
                "\trm -rf /run/hostkeeper\n}",
            ),
            "start_pre ends with `rm -rf /run/hostkeeper`, not `rm -f /run/hostkeeper/ready`",
        ),
        (
            altered(
                &initd,
                "\trm -f /run/hostkeeper/ready\n}",
                "\trm -f /run/hostkeeper/other\n}",
            ),
            "start_pre ends with `rm -f /run/hostkeeper/other`",
        ),
        (
            altered(
                &initd,
                "\trm -f /run/hostkeeper/ready\n}",
                "\trm -f /run/hostkeeper/ready\n\trm -f /run/hostkeeper/admin.sock\n}",
            ),
            "start_pre ends with `rm -f /run/hostkeeper/admin.sock`",
        ),
        (
            altered(
                &initd,
                "\n\t\tif [ \"$(service_get_value start_count)\" -gt 1 ]; then",
                "\n\t\tif false; then",
            ),
            "start_post is",
        ),
        (altered(&initd, "-gt 1 ]", "-gt 2 ]"), "start_post is"),
        (
            altered(
                &initd,
                "while [ ! -e /run/hostkeeper/ready ]",
                "while [ ! -e /run/hostkeeper/up ]",
            ),
            "start_post is",
        ),
        (altered(&initd, "\t\tsleep 1\n", ""), "start_post is"),
        (altered(&initd, "\t\t\treturn 0\n", ""), "start_post is"),
        (
            altered(&initd, "\nstart_post() {", "\nstop_post() {"),
            "defines stop_post",
        ),
    ];
    refuses(|t: &str| check_initd(t, &unit, &tmpfiles), &cases);
}

/// Catches an init script whose arguments are not the unit's: a path, a flag, the order, or
/// no `--ready-file` in both, `--notify-systemd` kept, or another ready file; and an init script that no longer follows a unit that changed.
#[test]
fn init_script_arguments_are_the_units_word_for_word() {
    let initd = read_alpine(INITD);
    let unit = read(UNIT);
    let tmpfiles = read(TMPFILES);
    let args = "daemon --config /etc/hostkeeper/hostkeeper.toml --state-dir /var/lib/updater/state --packages-dir /var/lib/hostkeeper/packages --ready-file /run/hostkeeper/ready";
    let with_args = |new: &str| altered(&initd, args, new);
    let initd_cases = [
        (
            with_args(&args.replace(
                "--state-dir /var/lib/updater/state",
                "--state-dir /var/lib/hostkeeper/state",
            )),
            "command_args is",
        ),
        (
            with_args(&args.replace(" --ready-file /run/hostkeeper/ready", "")),
            "command_args is",
        ),
        (
            with_args(&args.replace("--ready-file /run/hostkeeper/ready", "--notify-systemd")),
            "command_args is",
        ),
        (
            with_args(&args.replace("--ready-file", "--ready-path")),
            "command_args is",
        ),
        (
            with_args(&args.replace("/run/hostkeeper/ready", "/run/hostkeeper/up")),
            "command_args is",
        ),
        (with_args(&args.replace("daemon ", "")), "command_args is"),
        (
            with_args(&format!("{args} --heartbeat-secs 15")),
            "command_args is",
        ),
        (
            with_args(
                "daemon --state-dir /var/lib/updater/state --config /etc/hostkeeper/hostkeeper.toml --packages-dir /var/lib/hostkeeper/packages --ready-file /run/hostkeeper/ready",
            ),
            "command_args is",
        ),
    ];
    refuses(|t: &str| check_initd(t, &unit, &tmpfiles), &initd_cases);
    let unit_cases = [
        (
            altered(
                &unit,
                "--config /etc/hostkeeper/hostkeeper.toml",
                "--config /etc/hostkeeper/other.toml",
            ),
            "command_args is",
        ),
        (
            altered(&unit, " --packages-dir /var/lib/hostkeeper/packages", ""),
            "command_args is",
        ),
    ];
    refuses(|t: &str| check_initd(&initd, t, &tmpfiles), &unit_cases);
    // Both without the flag: they agree, and the daemon would write no ready file.
    let without = |initd: &str, unit: &str| {
        check_initd(
            &initd.replace(" --ready-file /run/hostkeeper/ready", ""),
            &unit.replace(" --notify-systemd", ""),
            &tmpfiles,
        )
    };
    assert_eq!(
        without(&initd, &unit),
        Err("command_args lacks --ready-file, so the daemon writes no ready file and start_post waits for it with no end".to_owned())
    );
}

/// Catches an init script whose `start_pre` and `start_post` name another ready file than the
/// one `command_args` hands the daemon: the wait would never end, or the stale file of the last
/// start would end it at once.
#[test]
fn init_script_waits_for_the_file_the_daemon_writes() {
    let initd = read_alpine(INITD);
    let unit = read(UNIT);
    let tmpfiles = read(TMPFILES);
    let cases = [
        (
            altered(
                &initd,
                "rm -f /run/hostkeeper/ready",
                "rm -f /run/hostkeeper/up",
            ),
            "start_pre ends with `rm -f /run/hostkeeper/up`, not `rm -f /run/hostkeeper/ready`",
        ),
        (
            altered(
                &initd,
                "while [ ! -e /run/hostkeeper/ready ]",
                "while [ ! -e /run/hostkeeper/up ]",
            ),
            "start_post is",
        ),
        (
            altered(&initd, "start_post() {", "start_post() {\n\tsleep 5"),
            "start_post is",
        ),
    ];
    refuses(|t: &str| check_initd(t, &unit, &tmpfiles), &cases);
}

/// Catches an init script that drifts from the tmpfiles rules: a mode, an owner, a directory
/// added or gone, a rule that is not a directory, or an unreadable rule.
#[test]
fn init_script_directories_follow_the_tmpfiles_rules() {
    let initd = read_alpine(INITD);
    let unit = read(UNIT);
    let tmpfiles = read(TMPFILES);
    let cases = [
        (
            altered(
                &tmpfiles,
                "/var/lib/updater/state       0700",
                "/var/lib/updater/state       0755",
            ),
            "start_pre makes",
        ),
        (
            altered(
                &tmpfiles,
                "/etc/hostkeeper              0755 root root",
                "/etc/hostkeeper              0755 root wheel",
            ),
            "start_pre makes",
        ),
        (
            format!("{tmpfiles}d /var/lib/updater/new 0700 root root -\n"),
            "start_pre makes",
        ),
        (
            altered(
                &tmpfiles,
                "d      /var/lib/updater/hooks       0700 root root  -\n",
                "",
            ),
            "start_pre makes",
        ),
        (
            altered(
                &tmpfiles,
                "d      /etc/hostkeeper ",
                "L      /etc/hostkeeper ",
            ),
            "not d (a directory)",
        ),
        (
            altered(
                &tmpfiles,
                "d      /etc/hostkeeper              0755 root root  -",
                "d      /etc/hostkeeper",
            ),
            "fields",
        ),
    ];
    refuses(|t: &str| check_initd(&initd, &unit, t), &cases);
}
