//! `docs/operations/move-to-native.md`, the runbook of the move to native packages
//! (packages-native.md section 10.6), against `hkctl` and the coordinator's JSON.
//!
//! - Each query of [`QUERIES`] is one fenced block: its label line, one `hkctl` line that calls
//!   the query's method and writes the answer with `--response-file`, and one `jq` line that
//!   reads that file with the query's output flag and exactly the query's `--arg` names. The
//!   block is found by its label, and anything else in it is refused.
//! - Each query's program, as the runbook writes it, is run with jaq on answers serialized as the
//!   gateway serializes them: the real `Host` messages of the protos, in the canonical proto3 JSON
//!   mapping with defaults left out ([`answer`]). A renamed proto field, a count that misses a host
//!   or counts one twice, a version compared as text, or a query that counts part of a fleet of
//!   more than one page fails here.
//! - Each of stages 5 to 10 has its section, which says when it starts and how it is backed out,
//!   and stage 9's names its two queries.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};

use super::jq::{Ended, Ran, run_on};
use super::{command_of, fenced_lines, repo_root, words};

/// The runbook, from the repository root.
const RUNBOOK: &str = "docs/operations/move-to-native.md";

/// One query of the runbook: its label, the `hkctl` call it makes, the flag `jq` prints with,
/// and the names of its `--arg`s.
struct Spec {
    label: &'static str,
    call: &'static str,
    output: &'static str,
    args: &'static [&'static str],
}

/// The queries the runbook must hold.
const QUERIES: &[Spec] = &[
    Spec {
        label: "# query: older-daemons",
        call: "hosts list",
        output: "-r",
        args: &["floor"],
    },
    Spec {
        label: "# query: archive-hosts",
        call: "hosts list",
        output: "-r",
        args: &[],
    },
    Spec {
        label: "# query: service-not-native",
        call: "hosts list",
        output: "-r",
        args: &["service"],
    },
    Spec {
        label: "# query: not-announcing",
        call: "hosts list",
        output: "-r",
        args: &["installer"],
    },
    Spec {
        label: "# check: announces",
        call: "hosts get",
        output: "-e",
        args: &["installer"],
    },
    Spec {
        label: "# check: busy",
        call: "hosts get",
        output: "-r",
        args: &[],
    },
];

/// The fleet queries: each reads one page of `hosts list`.
const FLEET: [&str; 4] = [
    "# query: older-daemons",
    "# query: archive-hosts",
    "# query: service-not-native",
    "# query: not-announcing",
];

/// What a fleet query stops with when the coordinator has more hosts than one page.
const ONE_PAGE: &str = "more hosts than one page: this query reads one page";

/// The fenced blocks of `doc`, each as its lines with continued lines joined.
fn blocks(doc: &str) -> Vec<Vec<String>> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            match current.take() {
                Some(text) => blocks.push(fenced_lines(&format!("```\n{text}```\n"))),
                None => current = Some(String::new()),
            }
            continue;
        }
        if let Some(text) = current.as_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    blocks
}

/// The value of the flag `flag` among `words`, if any.
fn value_of(words: &[String], flag: &str) -> Option<String> {
    let at = words.iter().position(|w| w == flag)?;
    words.get(at + 1).cloned()
}

/// The program of `spec` in `doc`, or why the block is not whole.
fn program(doc: &str, spec: &Spec) -> Result<String, String> {
    let label = spec.label;
    let mut found = blocks(doc)
        .into_iter()
        .filter(|b| b.first().map(String::as_str) == Some(label));
    let block = found.next().ok_or(format!("{label}: no block"))?;
    if found.next().is_some() {
        return Err(format!("{label}: two blocks"));
    }
    let [_, call, jq] = block.as_slice() else {
        return Err(format!("{label}: not one hkctl line and one jq line"));
    };
    let call_words = words(call)?;
    if call_words.first().map(String::as_str) != Some("hkctl") {
        return Err(format!("{label}: the second line is not hkctl"));
    }
    match command_of(call)? {
        Some(c) if c == spec.call => {}
        other => return Err(format!("{label}: calls {other:?}, not {}", spec.call)),
    }
    if spec.call == "hosts list" && value_of(&call_words, "--limit").as_deref() != Some("256") {
        return Err(format!("{label}: not one page of 256 hosts"));
    }
    let file = value_of(&call_words, "--response-file")
        .ok_or(format!("{label}: hkctl writes no --response-file"))?;
    let jq = words(jq)?;
    let mut rest = jq.iter();
    if rest.next().map(String::as_str) != Some("jq") {
        return Err(format!("{label}: the third line is not jq"));
    }
    let (mut output, mut args, mut operands) = (Vec::new(), Vec::new(), Vec::new());
    while let Some(word) = rest.next() {
        match word.as_str() {
            "-r" | "-e" => output.push(word.as_str()),
            "--arg" => {
                let name = rest.next().ok_or(format!("{label}: --arg with no name"))?;
                rest.next()
                    .ok_or(format!("{label}: --arg {name} with no value"))?;
                args.push(name.as_str());
            }
            flag if flag.starts_with('-') => return Err(format!("{label}: jq {flag}")),
            operand => operands.push(operand),
        }
    }
    if output != [spec.output] {
        return Err(format!("{label}: jq {output:?}, not {}", spec.output));
    }
    if args != spec.args {
        return Err(format!("{label}: --arg {args:?}, not {:?}", spec.args));
    }
    match operands.as_slice() {
        [program, read] if **read == file => Ok((*program).to_owned()),
        [_, read] => Err(format!("{label}: jq reads {read}, hkctl writes {file}")),
        _ => Err(format!(
            "{label}: jq takes {operands:?}, not a program and a file"
        )),
    }
}

/// Every problem of the runbook's queries and stages.
fn problems(doc: &str) -> Vec<String> {
    let mut problems: Vec<String> = QUERIES
        .iter()
        .filter_map(|spec| program(doc, spec).err())
        .collect();
    for stage in 5..=10 {
        let heading = format!("## Stage {stage}:");
        let Some(at) = doc.find(&heading) else {
            problems.push(format!("no section `{heading}`"));
            continue;
        };
        let body = &doc[at + heading.len()..];
        let body = &body[..body.find("\n## ").unwrap_or(body.len())];
        for said in ["**Starts when**", "**Back out**"] {
            if !body.contains(said) {
                problems.push(format!("stage {stage} has no {said}"));
            }
        }
        if stage == 9 {
            for query in ["`archive-hosts`", "`older-daemons`"] {
                if !body.contains(query) {
                    problems.push(format!("stage 9 does not name {query}"));
                }
            }
        }
    }
    problems
}

/// The runbook as it is in the repository.
fn real() -> String {
    std::fs::read_to_string(repo_root().join(RUNBOOK)).unwrap_or_else(|e| panic!("{RUNBOOK}: {e}"))
}

/// The program the runbook writes under `label`.
fn real_program(label: &str) -> String {
    let spec = QUERIES
        .iter()
        .find(|s| s.label == label)
        .unwrap_or_else(|| panic!("{label} is a query"));
    program(&real(), spec).unwrap_or_else(|e| panic!("{e}"))
}

/// `message` as the gateway answers it: the canonical proto3 JSON mapping of the message the
/// descriptors name `name`, with the gateway's options.
fn answer(name: &str, message: &impl Message) -> String {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decode");
    let descriptor = pool.get_message_by_name(name).expect(name);
    let dynamic =
        DynamicMessage::decode(descriptor, message.encode_to_vec().as_slice()).expect("decodes");
    let mut writer = serde_json::Serializer::new(Vec::new());
    dynamic
        .serialize_with_options(&mut writer, &SerializeOptions::new())
        .expect("serializes");
    String::from_utf8(writer.into_inner()).expect("utf-8")
}

/// A host the coordinator lists with nothing but its node id: never heard, nothing kept.
fn host(id: &str) -> op::Host {
    op::Host {
        node_id: id.to_owned(),
        ..op::Host::default()
    }
}

/// `host` whose last heartbeat named the agent `name` at `version`.
fn daemon(mut host: op::Host, name: &str, version: &str) -> op::Host {
    host.heartbeat = Some(agent::HeartbeatRequest {
        agent: Some(agent::AgentInfo {
            name: name.to_owned(),
            version: version.to_owned(),
        }),
        ..agent::HeartbeatRequest::default()
    });
    host
}

/// `host` that said it runs each `(service, version, installer)` of `services`.
fn runs(mut host: op::Host, services: &[(&str, &str, &str)]) -> op::Host {
    host.running = services
        .iter()
        .map(|&(service, version, installer)| op::RunningService {
            service: service.to_owned(),
            version: version.to_owned(),
            installer: installer.to_owned(),
        })
        .collect();
    host.running_unix = Some(1_700_000_000);
    host
}

/// `host` whose daemon listed `installers`.
fn lists(mut host: op::Host, installers: &[&str]) -> op::Host {
    host.installers = installers.iter().map(|&i| i.to_owned()).collect();
    host
}

/// The fleet query `label` of the runbook, run on one page of `hosts` with `vars`.
fn fleet(label: &str, vars: &[(&str, &str)], hosts: Vec<op::Host>, next: &str) -> Ran {
    let page = op::ListHostsResponse {
        hosts,
        next_page_token: next.to_owned(),
    };
    let input = answer("hostkeeper.operator.v1.ListHostsResponse", &page);
    let vars: Vec<(&str, String)> = vars.iter().map(|&(n, v)| (n, v.to_owned())).collect();
    run_on(&real_program(label), &vars, &input)
}

/// The one-host check `label` of the runbook, run on `host` with `vars`.
fn one(label: &str, vars: &[(&str, &str)], host: op::Host) -> Ran {
    let reply = op::GetHostResponse { host: Some(host) };
    let input = answer("hostkeeper.operator.v1.GetHostResponse", &reply);
    let vars: Vec<(&str, String)> = vars.iter().map(|&(n, v)| (n, v.to_owned())).collect();
    run_on(&real_program(label), &vars, &input)
}

/// What a query printed, to compare: its lines, and that it ran to its end.
fn printed(lines: &[&str]) -> Ran {
    (lines.iter().map(|&l| l.to_owned()).collect(), Ended::Done)
}

/// Catches a query of the runbook that is missing, split, calls another method, reads another
/// file than hkctl wrote, or binds other variables; and a stage section that does not say when it
/// starts or how it is backed out.
#[test]
fn the_runbook_holds_each_query_and_stage() {
    assert_eq!(problems(&real()), Vec::<String>::new());
}

/// The check of [`the_runbook_holds_each_query_and_stage`] refuses each defect, with its reason.
#[test]
fn the_runbook_check_refuses_each_defect() {
    let doc = real();
    let edit = |from: &str, to: &str| {
        assert!(doc.contains(from), "{from}");
        problems(&doc.replacen(from, to, 1))
    };
    assert_eq!(
        edit("# query: archive-hosts\n", "# query: archive\n"),
        ["# query: archive-hosts: no block"]
    );
    assert_eq!(
        edit(
            "# check: busy\n",
            "# check: busy\n```\n\n```\n# check: busy\n"
        ),
        ["# check: busy: two blocks"]
    );
    assert_eq!(
        edit(
            "    hosts get --node-id <id> --response-file host.json\njq -r",
            "    hosts get --node-id <id> --response-file host.json\necho\njq -r"
        ),
        ["# check: busy: not one hkctl line and one jq line"]
    );
    assert_eq!(
        edit(
            "hosts.json\n```\n\n`<version>`",
            "other.json\n```\n\n`<version>`"
        ),
        ["# query: older-daemons: jq reads other.json, hkctl writes hosts.json"]
    );
    assert_eq!(
        edit(
            "hosts list --limit 256 --response-file hosts.json\njq -r 'if",
            "hosts list --limit 100 --response-file hosts.json\njq -r 'if"
        ),
        ["# query: archive-hosts: not one page of 256 hosts"]
    );
    assert_eq!(
        edit(
            "hosts get --node-id <id> --response-file host.json\njq -e",
            "hosts exclude --node-id <id> --response-file host.json\njq -e"
        ),
        ["# check: announces: calls Some(\"hosts exclude\"), not hosts get"]
    );
    assert_eq!(
        edit("jq -e --arg installer apt", "jq -r --arg installer apt"),
        ["# check: announces: jq [\"-r\"], not -e"]
    );
    assert_eq!(
        edit(
            "jq -r --arg service <service>",
            "jq -r --arg name <service>"
        ),
        ["# query: service-not-native: --arg [\"name\"], not [\"service\"]"]
    );
    assert_eq!(
        edit("jq -r '[.host.services", "jq -r --slurp '[.host.services"),
        ["# check: busy: jq --slurp"]
    );
    assert_eq!(
        edit(
            "**Back out**: set a release with archive entries",
            "Set a release with archive entries"
        ),
        ["stage 9 has no **Back out**"]
    );
    assert_eq!(
        edit(
            "1. `archive-hosts` prints `0`",
            "1. the first query prints `0`"
        ),
        ["stage 9 does not name `archive-hosts`"]
    );
    assert_eq!(
        edit("## Stage 10:", "## Stage ten:"),
        ["no section `## Stage 10:`"]
    );
}

/// Catches a version compared as text (`0.1.99` after `0.1.200`) or a floor read as a ceiling:
/// a daemon below the floor in any of its three numbers is counted, one at it or above is not.
#[test]
fn older_daemons_counts_a_daemon_below_the_floor_by_number() {
    let hosts = vec![
        daemon(host("h1"), "hostkeeper", "0.1.200"),
        daemon(host("h2"), "hostkeeper", "0.1.201"),
        daemon(host("h3"), "hostkeeper", "0.2.0"),
        daemon(host("h4"), "hostkeeper", "1.0.0"),
        daemon(host("h5"), "hostkeeper", "0.1.199"),
        daemon(host("h6"), "hostkeeper", "0.1.99"),
        daemon(host("h7"), "hostkeeper", "0.0.300"),
    ];
    assert_eq!(
        fleet("# query: older-daemons", &[("floor", "0.1.200")], hosts, ""),
        printed(&[
            "3",
            "h5: daemon 0.1.199",
            "h6: daemon 0.1.99",
            "h7: daemon 0.0.300"
        ])
    );
}

/// Catches a host left out of stage 9's second count because nothing proves its daemon new
/// enough: no heartbeat, a heartbeat with no agent (kbf's agent names none), another agent, or a
/// version that is not three numbers. An enrolled host not heard since the coordinator started is
/// judged by the daemon its enrollment kept.
#[test]
fn older_daemons_counts_a_host_whose_daemon_is_not_known_new_enough() {
    let mut no_agent = host("h2");
    no_agent.heartbeat = Some(agent::HeartbeatRequest::default());
    let hosts = vec![
        host("h1"),
        no_agent,
        daemon(host("h3"), "kbf-updater", "9.9.9"),
        daemon(host("h4"), "hostkeeper", "0.1.200-rc.1"),
        daemon(host("h5"), "hostkeeper", "0.1"),
        daemon(host("h6"), "hostkeeper", "0.1.x"),
        daemon(host("h7"), "hostkeeper", ""),
        daemon(host("h8"), "hostkeeper", "0.1.200"),
    ];
    assert_eq!(
        fleet("# query: older-daemons", &[("floor", "0.1.200")], hosts, ""),
        printed(&[
            "7",
            "h1: no hostkeeper daemon known",
            "h2: no hostkeeper daemon known",
            "h3: no hostkeeper daemon known",
            "h4: daemon 0.1.200-rc.1 is not major.minor.patch",
            "h5: daemon 0.1 is not major.minor.patch",
            "h6: daemon 0.1.x is not major.minor.patch",
            "h7: daemon  is not major.minor.patch",
        ])
    );
}

/// Catches a floor the operator did not fill in, or wrote wrong, read as a version that counts
/// nobody.
#[test]
fn older_daemons_refuses_a_floor_that_is_not_a_version() {
    for floor in ["<version>", "0.1", "v0.1.200", "0.1.200.1", ""] {
        let hosts = vec![daemon(host("h1"), "hostkeeper", "0.0.1")];
        assert_eq!(
            fleet("# query: older-daemons", &[("floor", floor)], hosts, ""),
            (
                vec![],
                Ended::Failed(format!("--arg floor is not major.minor.patch: {floor}"))
            ),
            "{floor:?}"
        );
    }
}

/// Catches a fleet query that counts the first page of a larger fleet as the whole, and one that
/// prints nothing, not `0`, for a fleet with no host.
#[test]
fn every_fleet_query_reads_exactly_one_whole_page() {
    let vars = [
        ("floor", "0.1.200"),
        ("service", "hello"),
        ("installer", "apt"),
    ];
    for label in FLEET {
        let spec = QUERIES.iter().find(|s| s.label == label).expect("a query");
        let vars: Vec<(&str, &str)> = vars
            .iter()
            .copied()
            .filter(|(n, _)| spec.args.contains(n))
            .collect();
        assert_eq!(
            fleet(label, &vars, vec![host("h1")], "h1"),
            (vec![], Ended::Failed(ONE_PAGE.to_owned())),
            "{label}"
        );
        assert_eq!(fleet(label, &vars, vec![], ""), printed(&["0"]), "{label}");
    }
}

/// Catches a host counted once per archive service instead of once, a service from an installer
/// the coordinator does not know left uncounted, and a host nothing is known of taken as native.
#[test]
fn archive_hosts_counts_each_host_once_and_every_host_that_may_run_archives() {
    let hosts = vec![
        runs(
            host("h1"),
            &[
                ("hello", "1.0.0", "hostkeeper-archive"),
                ("hostkeeper", "0.1.200", "apt"),
                ("mdm", "2.0.0", "hostkeeper-archive"),
            ],
        ),
        runs(host("h2"), &[("hello", "1.0.0", "")]),
        host("h3"),
        runs(
            host("h4"),
            &[("hello", "1.0.0", "apt"), ("tierstore", "3.0.0", "apt")],
        ),
        runs(host("h5"), &[("hello", "1.0.0", "macos-pkg")]),
        runs(host("h6"), &[]),
    ];
    assert_eq!(
        fleet("# query: archive-hosts", &[], hosts, ""),
        printed(&[
            "3",
            "h1: hello 1.0.0 from hostkeeper-archive, mdm 2.0.0 from hostkeeper-archive",
            "h2: hello 1.0.0 from an installer not known",
            "h3: nothing known of what it runs",
        ])
    );
}

/// Catches stage 8 started while a host still runs `hello` from archives, and another
/// service's archives counted against `hello`.
#[test]
fn service_not_native_counts_the_hosts_that_run_that_service_other_than_natively() {
    let hosts = vec![
        runs(
            host("h1"),
            &[
                ("hello", "1.0.0", "apt"),
                ("mdm", "2.0.0", "hostkeeper-archive"),
            ],
        ),
        runs(host("h2"), &[("hello", "1.0.0", "hostkeeper-archive")]),
        runs(host("h3"), &[("hello", "1.0.0", "")]),
        host("h4"),
        runs(host("h5"), &[("mdm", "2.0.0", "hostkeeper-archive")]),
        runs(host("h6"), &[("hello", "1.0.0", "macos-pkg")]),
    ];
    assert_eq!(
        fleet(
            "# query: service-not-native",
            &[("service", "hello")],
            hosts.clone(),
            ""
        ),
        printed(&[
            "3",
            "h2: hello 1.0.0 from hostkeeper-archive",
            "h3: hello 1.0.0 from an installer not known",
            "h4: nothing known of what it runs",
        ])
    );
    assert_eq!(
        fleet(
            "# query: service-not-native",
            &[("service", "mdm")],
            hosts,
            ""
        ),
        printed(&[
            "3",
            "h1: mdm 2.0.0 from hostkeeper-archive",
            "h4: nothing known of what it runs",
            "h5: mdm 2.0.0 from hostkeeper-archive",
        ])
    );
}

/// Catches a host whose installers the coordinator has not read taken as announcing, and an
/// installer matched by part of its name.
#[test]
fn not_announcing_counts_each_host_whose_daemon_does_not_list_the_installer() {
    let hosts = vec![
        lists(host("h1"), &["hostkeeper-archive", "apt"]),
        lists(host("h2"), &["hostkeeper-archive"]),
        host("h3"),
        lists(host("h4"), &["hostkeeper-archive", "macos-pkg"]),
        lists(host("h5"), &["apt-get"]),
    ];
    assert_eq!(
        fleet(
            "# query: not-announcing",
            &[("installer", "apt")],
            hosts.clone(),
            ""
        ),
        printed(&[
            "4",
            "h2: announces hostkeeper-archive",
            "h3: no installers read since the coordinator started",
            "h4: announces hostkeeper-archive, macos-pkg",
            "h5: announces apt-get",
        ])
    );
    assert_eq!(
        fleet(
            "# query: not-announcing",
            &[("installer", "macos-pkg")],
            hosts,
            ""
        ),
        printed(&[
            "4",
            "h1: announces hostkeeper-archive, apt",
            "h2: announces hostkeeper-archive",
            "h3: no installers read since the coordinator started",
            "h5: announces apt-get",
        ])
    );
}

/// Catches stage 6's check passing on a host whose daemon does not list `apt`.
#[test]
fn announces_is_true_only_when_the_daemon_lists_the_installer() {
    let apt = [("installer", "apt")];
    let check =
        |installers: &[&str]| one("# check: announces", &apt, lists(host("h1"), installers));
    assert_eq!(check(&["hostkeeper-archive", "apt"]), printed(&["true"]));
    assert_eq!(check(&["hostkeeper-archive"]), printed(&["false"]));
    assert_eq!(check(&[]), printed(&["false"]));
    assert_eq!(check(&["apt-get"]), printed(&["false"]));
}

/// Catches a restart in stage 6 judged safe while the host holds an update slot.
#[test]
fn busy_names_each_service_the_host_holds_an_update_slot_of() {
    let service = |name: &str, holds_slot: bool| op::HostService {
        service: name.to_owned(),
        holds_slot,
        ..op::HostService::default()
    };
    let mut idle = host("h1");
    idle.services = vec![service("hello", false), service("hostkeeper", false)];
    assert_eq!(one("# check: busy", &[], idle), printed(&["0"]));
    let mut busy = host("h1");
    busy.services = vec![
        service("hello", true),
        service("hostkeeper", false),
        service("mdm", true),
    ];
    assert_eq!(
        one("# check: busy", &[], busy),
        printed(&["2", "hello", "mdm"])
    );
    assert_eq!(one("# check: busy", &[], host("h1")), printed(&["0"]));
}
