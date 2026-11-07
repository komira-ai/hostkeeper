use std::path::{Path, PathBuf};

use clap::Parser;

use super::{Error, Flags, Mode, run};
use crate::testdir::TestDir;
use crate::{Cli, Verb};

/// An empty directory of its own for one test, removed when it drops.
fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-netinstall-unit"),
        name,
    )
}

/// The path of one input among the flags.
type FileOf = fn(&mut Flags) -> &mut PathBuf;

/// The three inputs of one served install, written into `dir`, and the flags that name them.
fn inputs(dir: &Path) -> Flags {
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write an input");
        path
    };
    Flags {
        mac: "52:54:00:4E:10:01".parse().expect("a MAC"),
        host_id: "net-1".to_owned(),
        mode: Mode::Proxy,
        pxe_log: write(
            "hostkeeper-pxe.log",
            "hostkeeper-pxe serve: 1000 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture \
             7, PXEClient, on the DHCP port: proxy offer sent, boot file hostkeeper/shimx64.efi \
             (linux-x86_64)\n\
             hostkeeper-pxe serve: 1040 seed seed/52:54:00:4e:10:01/meta-data: fetched by \
             52:54:00:4e:10:01, host net-1: its install started, and it leaves the answer set \
             for the rest of the run\n",
        ),
        pending: write(
            "pending.json",
            r#"{"requests": [{"recordHostId": "net-1", "firstSeenUnix": "1900"}]}"#,
        ),
        host: write(
            "host.json",
            r#"{"host": {"nodeId": "net-1", "enrolledUnix": "1960", "firstHeartbeatUnix": "1975",
                "firstServingUnix": "2010"}}"#,
        ),
    }
}

/// Catches an input read from another flag's file, or one left out of the report: each of the
/// six events comes from the file that records it.
#[test]
fn the_report_reads_each_event_from_its_file() {
    let dir = scratch("served");
    let report = run(&inputs(&dir)).expect("the report");
    assert_eq!(
        report[4..10],
        [
            "| power-on | 1000 | hostkeeper-pxe: the first DHCPDISCOVER from the MAC |",
            "| install started | 1040 | hostkeeper-pxe: the seed fetch |",
            "| pending | 1900 | the coordinator: the request's `first_seen_unix` |",
            "| approved | 1960 | the coordinator: the host's `enrolled_unix` |",
            "| first heartbeat | 1975 | the coordinator: the host's `first_heartbeat_unix` |",
            "| serving | 2010 | the coordinator: the host's `first_serving_unix` |",
        ],
        "{report:#?}"
    );
}

/// Catches a file that cannot be read passed over as an event not reached, or named by
/// another flag: each refusal names its flag and its path.
#[test]
fn a_file_that_cannot_be_read_is_refused_by_its_flag() {
    let dir = scratch("unread");
    let missing = dir.join("missing");
    let cases: [(&str, FileOf); 3] = [
        ("--pxe-log", |f| &mut f.pxe_log),
        ("--pending", |f| &mut f.pending),
        ("--host", |f| &mut f.host),
    ];
    for (flag, field) in cases {
        let mut flags = inputs(&dir);
        *field(&mut flags) = missing.clone();
        let Err(Error::Io { what, why }) = run(&flags) else {
            panic!("{flag}: a missing file is refused");
        };
        assert_eq!(what, format!("{flag} {}", missing.display()));
        assert!(why.contains("No such file"), "{why}");
    }
}

/// Catches a refusal of an input's text not reaching the caller: a pending file of another
/// shape, a host file of another host.
#[test]
fn a_file_that_does_not_say_what_its_flag_names_is_refused() {
    let dir = scratch("refused");
    let edits: [(&str, &str, FileOf); 2] = [
        ("[]", "--pending: not a JSON object", |f| &mut f.pending),
        (
            r#"{"host": {"nodeId": "net-2"}}"#,
            "--host: the host is net-2",
            |f| &mut f.host,
        ),
    ];
    for (text, why, file) in edits {
        let mut flags = inputs(&dir);
        std::fs::write(file(&mut flags), text).expect("rewrite an input");
        let Err(Error::Refused(refused)) = run(&flags) else {
            panic!("{why}: refused");
        };
        assert!(refused.starts_with(why), "{refused}");
    }
}

/// The runbook that tells the operator to run the report on the first real machine.
const RUNBOOK: &str = "../../docs/operations/network-install.md";

/// The fenced lines of `doc` that begin `cargo xtask netinstall-report`, each line ending in
/// `\` joined to the next, parsed with this crate's own command line; at least one, or why not.
fn runbook_problems(doc: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let (mut fenced, mut pending) = (false, String::new());
    for line in doc.lines().map(str::trim) {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if fenced {
            match line.strip_suffix('\\') {
                Some(head) => pending.push_str(&format!("{} ", head.trim_end())),
                None => lines.push(std::mem::take(&mut pending) + line),
            }
        }
    }
    let reports: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("cargo xtask netinstall-report "))
        .collect();
    if reports.is_empty() {
        return vec!["no `cargo xtask netinstall-report` line".to_owned()];
    }
    let mut problems = Vec::new();
    for line in reports {
        // `cargo` is the runner; `xtask` stands as the program's name.
        let words = line.split_whitespace().skip(1);
        match Cli::try_parse_from(words) {
            Ok(Cli {
                verb: Verb::NetinstallReport(_),
            }) => {}
            Ok(_) => unreachable!("only the lines of netinstall-report are parsed"),
            Err(e) => problems.push(format!("{line}: {}", e.kind())),
        }
    }
    problems
}

/// Catches the runbook's line of the report going stale: a renamed or missing flag, a value
/// `--mode` does not take, or the line removed.
#[test]
fn the_runbooks_report_line_parses() {
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(RUNBOOK))
        .expect("the runbook");
    assert_eq!(runbook_problems(&doc), Vec::<String>::new());
}

/// Catches the runbook check passing a defect: a misspelled flag, a missing required flag, a
/// mode it does not take, and no line at all.
#[test]
fn the_runbook_check_refuses_each_defect() {
    let line = "cargo xtask netinstall-report --mac 00:00:5e:00:53:01 --host-id web-17 \\\n\
                --mode proxy --pxe-log serve.log --pending pending.json --host host.json";
    let fenced = |line: &str| format!("text\n```\n{line}\n```\n");
    assert_eq!(runbook_problems(&fenced(line)), Vec::<String>::new());
    for (bad, kind) in [
        (line.replace("--pending", "--pendng"), "unexpected argument"),
        (
            line.replace(" --host host.json", ""),
            "required arguments were not provided",
        ),
        (line.replace("proxy", "dhcp"), "isn't valid"),
    ] {
        let problems = runbook_problems(&fenced(&bad));
        assert_eq!(problems.len(), 1, "{bad}");
        assert!(problems[0].contains(kind), "{problems:?}");
    }
    assert_eq!(
        runbook_problems("no fence\n```\nhostkeeper ctl hosts list\n```\n"),
        ["no `cargo xtask netinstall-report` line"]
    );
}
