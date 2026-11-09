//! The shipped plans and the plan check: every shipped plan checks and is described by the
//! results page; a plan that breaks any rule of the check is refused with the rule.

use hostkeeper_measure::plan::{Action, Choose, Health, Kind, OnTimeout, Pick, Plan, Until};

use crate::support;

/// The shipped plans, by id.
pub const SHIPPED: [&str; 6] = [
    "coordinator-restart",
    "drift",
    "enrollment",
    "enrollment-mac",
    "rollout",
    "update-rollback",
];

fn readme() -> String {
    std::fs::read_to_string(support::crate_dir().join("../../docs/report/results/README.md"))
        .expect("the results page")
}

/// The cells of the table row of `README` that starts with the plan's id as code.
fn row<'a>(readme: &'a str, id: &str) -> Option<Vec<&'a str>> {
    let start = format!("| `{id}` |");
    readme
        .lines()
        .find(|l| l.starts_with(&start))
        .map(|l| l.trim_matches('|').split(" | ").map(str::trim).collect())
}

/// The names `README` does not hold as a code span.
fn unnamed<'a>(readme: &str, names: &[&'a str]) -> Vec<&'a str> {
    names
        .iter()
        .copied()
        .filter(|n| !readme.contains(&format!("`{n}`")))
        .collect()
}

#[test]
fn the_plans_directory_holds_exactly_the_shipped_plans_each_named_by_its_id() {
    let mut found: Vec<String> = std::fs::read_dir(support::crate_dir().join("plans"))
        .expect("the plans directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    let mut want: Vec<String> = SHIPPED.iter().map(|id| format!("{id}.toml")).collect();
    want.sort();
    assert_eq!(found, want);
    for id in SHIPPED {
        assert_eq!(support::shipped(id).id, id);
    }
}

#[test]
fn the_results_page_gives_each_plan_its_repetitions_and_names_every_event_and_field() {
    let readme = readme();
    for id in SHIPPED {
        let plan = support::shipped(id);
        let cells = row(&readme, id).unwrap_or_else(|| panic!("no row of {id}"));
        assert_eq!(cells.len(), 4, "{id}: {cells:?}");
        assert_eq!(cells[3], plan.repetitions.to_string(), "{id}");
        let mut events: Vec<&str> = plan
            .steps
            .iter()
            .flat_map(|s| {
                plan.events_of(&s.id)
                    .expect("a step's events")
                    .iter()
                    .copied()
            })
            .collect();
        events.extend(["started", "ended"]);
        assert_eq!(unnamed(&readme, &events), Vec::<&str>::new(), "{id}");
    }
    let fields = [
        "rep",
        "step",
        "event",
        "host",
        "runner_ms",
        "coordinator_unix",
        "detail",
    ];
    assert_eq!(unnamed(&readme, &fields), Vec::<&str>::new());
}

#[test]
fn the_page_checks_report_what_is_missing() {
    let page = "| `a` | x | y | 3 |\nthe `b` field\n";
    assert_eq!(row(page, "a"), Some(vec!["`a`", "x", "y", "3"]));
    assert_eq!(row(page, "b"), None);
    assert_eq!(unnamed(page, &["b", "c", "x"]), vec!["c", "x"]);
}

#[test]
fn the_shipped_plans_measure_what_the_brief_asks() {
    let rollout = support::shipped("rollout");
    assert_eq!(rollout.hosts.pick, Pick::Service);
    assert_eq!(rollout.hosts.service.as_deref(), Some("hostkeeper"));
    assert_eq!(rollout.hosts.exactly, None);
    // The request sets no maxUpdating: the one-in-flight rule as it is.
    let Action::Call { request, .. } = &rollout.steps[0].action else {
        panic!("the rollout starts with a call");
    };
    let request = request.as_ref().expect("a request");
    assert!(request["desired"].get("maxUpdating").is_none());
    let picks: Vec<(&str, Choose)> = rollout
        .measurements
        .iter()
        .filter_map(|m| match &m.kind {
            Kind::Span { pick, .. } => Some((m.id.as_str(), *pick)),
            _ => None,
        })
        .collect();
    assert_eq!(
        picks,
        [
            ("first-host", Choose::First),
            ("last-host", Choose::Last),
            ("host-update", Choose::Each)
        ]
    );
    for id in ["update-rollback", "drift"] {
        let plan = support::shipped(id);
        assert_eq!(plan.hosts.exactly, Some(1), "{id} is on one host");
    }
    let rollback = support::shipped("update-rollback");
    let Action::Call { args, .. } = &rollback.steps[2].action else {
        panic!("the third step of update-rollback is the force");
    };
    assert_eq!(&args[..2], ["releases", "force"]);
    let drift = support::shipped("drift");
    let Action::Watch(w) = &drift.steps[1].action else {
        panic!("the second step of drift is the detection");
    };
    assert_eq!(
        w.until,
        Until::Health {
            service: "hello".to_owned(),
            health: Health::Unhealthy
        }
    );
    assert_eq!(w.on_timeout, OnTimeout::Continue);
    let restart = support::shipped("coordinator-restart");
    assert_eq!(restart.hosts.pick, Pick::Live);
    for id in ["enrollment", "enrollment-mac"] {
        assert_eq!(support::shipped(id).hosts.pick, Pick::None, "{id}");
    }
}

#[test]
fn no_shipped_plan_or_golden_names_a_host_but_as_host_n() {
    // The node ids of the simulated fleets never reach a golden: only host-<n>.
    let golden = support::crate_dir().join("tests/golden");
    for entry in std::fs::read_dir(golden).expect("the goldens") {
        let text = std::fs::read_to_string(entry.expect("an entry").path()).expect("a golden");
        for node in [
            "node-a", "node-b", "node-c", "node-d", "node-new", "node-mac",
        ] {
            assert!(!text.contains(node), "a golden names {node}");
        }
    }
}

/// A plan that checks, which the cases below break one rule at a time.
pub const BASE: &str = r#"
id = "base"
title = "A base plan"
measures = "What is measured."
records = "What is recorded."
repetitions = 2
states = ["It took {span.median} s in {plan}, run {run}."]

[hosts]
says = "The one host of hello."
pick = "service"
service = "hello"
exactly = 1

[[param]]
name = "version"
says = "The version."
per_repetition = true

[[step]]
id = "set"
do = "call"
args = ["releases", "set-desired", "--x", "{version}"]
audit = "set_desired"
request = { service = "hello", desired = { version = "{version}" } }

[[step]]
id = "wait"
do = "watch"
until = { what = "updated", service = "hello", version = "{version}" }
poll_secs = 1
timeout_secs = 10

[[measure]]
id = "span"
kind = "span"
says = "the set to updated"
unit = "s"
from = "set/called"
to = "wait/updated"
pick = "first"
"#;

fn refused(from: &str, to: &str) -> String {
    assert!(BASE.contains(from), "the base plan has no {from:?}");
    match Plan::parse(&BASE.replacen(from, to, 1)) {
        Ok(_) => panic!("a plan with {to:?} for {from:?} checks"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_base_plan_checks() {
    let plan = support::plan(BASE);
    assert_eq!(plan.events_of("nothing"), None);
    assert_eq!(
        plan.events_of("repetition"),
        Some(&["started", "ended"][..])
    );
}

#[test]
fn a_plan_breaking_a_rule_is_refused_by_that_rule() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "id = \"set\"",
            "id = \"Set\"",
            "step \"Set\" is not 1 to 32 bytes of [a-z0-9-] starting with a letter",
        ),
        (
            "audit = \"set_desired\"",
            "audit = \"\"",
            "step \"set\": its audit action is not 1 to 64 bytes of [a-z_]",
        ),
        (
            "audit = \"set_desired\"",
            "audit = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
            "step \"set\": its audit action is not 1 to 64 bytes of [a-z_]",
        ),
        (
            "pick = \"first\"",
            "pick = \"first\"\n[[measure]]\nid = \"span\"\nkind = \"count\"\nsays = \"x\"\nunit = \"x\"\nof = \"set/called\"",
            "measure \"span\" is declared twice",
        ),
        (
            "id = \"base\"",
            "id = \"Base\"",
            "the plan id \"Base\" is not 1 to 32 bytes of [a-z0-9-] starting with a letter",
        ),
        (
            "id = \"base\"",
            "id = \"base\"\nextra = 1",
            "the plan is not TOML of the plan's shape: TOML parse error at line 3, column 1\n  |\n3 | extra = 1\n  | ^^^^^\nunknown field `extra`, expected one of `id`, `title`, `measures`, `hosts`, `repetitions`, `records`, `param`, `step`, `measure`, `states`\n",
        ),
        (
            "title = \"A base plan\"",
            "title = \"\"",
            "the title is empty",
        ),
        (
            "title = \"A base plan\"",
            "title = \"A\\nplan\"",
            "the title holds a tab or a line break: \"A\\nplan\"",
        ),
        (
            "measures = \"What is measured.\"",
            "measures = \"What\\tis\"",
            "measures holds a tab or a line break: \"What\\tis\"",
        ),
        (
            "repetitions = 2",
            "repetitions = 0",
            "the plan has no repetitions",
        ),
        (
            "service = \"hello\"\nexactly = 1",
            "exactly = 1",
            "the hosts: pick service names no service",
        ),
        (
            "pick = \"service\"",
            "pick = \"live\"",
            "the hosts: a service is named for a pick of no service",
        ),
        (
            "pick = \"service\"\nservice = \"hello\"",
            "pick = \"none\"",
            "the hosts: a pick of none has no number of hosts",
        ),
        (
            "exactly = 1",
            "exactly = 0",
            "the hosts: exactly zero hosts measure nothing",
        ),
        (
            "name = \"version\"",
            "name = \"host\"",
            "parameter \"host\" is not 1 to 32 bytes of [a-z0-9-] starting with a letter",
        ),
        (
            "name = \"version\"",
            "name = \"Version\"",
            "parameter \"Version\" is not 1 to 32 bytes of [a-z0-9-] starting with a letter",
        ),
        (
            "per_repetition = true",
            "per_repetition = true\n[[param]]\nname = \"version\"\nsays = \"Again.\"",
            "parameter \"version\" is declared twice",
        ),
        (
            "says = \"The version.\"",
            "says = \" \"",
            "parameter \"version\" is empty",
        ),
        (
            "id = \"set\"",
            "id = \"repetition\"",
            "step \"repetition\" is not 1 to 32 bytes of [a-z0-9-] starting with a letter",
        ),
        (
            "id = \"wait\"",
            "id = \"set\"",
            "step \"set\" is declared twice",
        ),
        (
            "\"{version}\"]",
            "\"{nothing}\"]",
            "step \"set\": {nothing} names no parameter \"nothing\"",
        ),
        (
            "\"{version}\"]",
            "\"{file:x}\"]",
            "step \"set\": {file:x} names no parameter \"x\"",
        ),
        (
            "\"{version}\"]",
            "\"{a.b}\"]",
            "step \"set\": {a.b} is not a slot of a step",
        ),
        (
            "\"{version}\"]",
            "\"{version\"]",
            "step \"set\": \"{version\": a `{` with no `}` after it",
        ),
        (
            "\"{version}\"]",
            "\"{host}\"]\naudit_x = 1",
            "the plan is not TOML of the plan's shape",
        ),
        (
            "args = [\"releases\", \"set-desired\", \"--x\", \"{version}\"]",
            "args = []",
            "step \"set\": a call with no arguments",
        ),
        (
            "audit = \"set_desired\"",
            "audit = \"Set\"",
            "step \"set\": its audit action is not 1 to 64 bytes of [a-z_]",
        ),
        (
            "do = \"call\"",
            "do = \"shout\"",
            "the plan is not TOML of the plan's shape",
        ),
        (
            "poll_secs = 1",
            "poll_secs = 0",
            "step \"wait\": a watch polls at least every second, within its timeout",
        ),
        (
            "timeout_secs = 10",
            "timeout_secs = 0",
            "step \"wait\": a watch polls at least every second, within its timeout",
        ),
        (
            "until = { what = \"updated\"",
            "until = { what = \"sideways\"",
            "the plan is not TOML of the plan's shape",
        ),
        (
            "id = \"span\"",
            "id = \"Span\"",
            "measure \"Span\" is not 1 to 32 bytes of [a-z0-9-] starting with a letter",
        ),
        (
            "says = \"the set to updated\"",
            "says = \"the set\\nto updated\"",
            "measure \"span\" holds a tab or a line break: \"the set\\nto updated\"",
        ),
        (
            "unit = \"s\"",
            "unit = \"two s\"",
            "measure \"span\": its unit is more than one word",
        ),
        (
            "unit = \"s\"",
            "unit = \"\"",
            "measure \"span\"'s unit is empty",
        ),
        (
            "from = \"set/called\"",
            "from = \"set\"",
            "measure \"span\": \"set\" is not step/event",
        ),
        (
            "from = \"set/called\"",
            "from = \"nothing/called\"",
            "measure \"span\": \"nothing/called\": no step \"nothing\"",
        ),
        (
            "from = \"set/called\"",
            "from = \"set/updated\"",
            "measure \"span\": \"set/updated\": step \"set\" records no event \"updated\"",
        ),
        (
            "kind = \"span\"",
            "kind = \"count\"",
            "the plan is not TOML of the plan's shape",
        ),
        (
            "{span.median}",
            "{span.mean}",
            "statement 0: {span.mean} is no measure's statistic",
        ),
        (
            "{span.median}",
            "{other.median}",
            "statement 0: {other.median} is no measure's statistic",
        ),
        (
            "{span.median}",
            "{span}",
            "statement 0: {span} is no measure's statistic",
        ),
        (
            "{span.median}",
            "{span.median",
            "statement 0: \"It took {span.median s in {plan}, run {run}.\": the slot {span.median s in {plan} is not 1 to 64 bytes of [a-z0-9.:-]",
        ),
        (
            "states = [\"It took {span.median} s in {plan}, run {run}.\"]",
            "states = [\"A\\nB\"]",
            "statement 0 holds a tab or a line break: \"A\\nB\"",
        ),
        (
            "states = [\"It took {span.median} s in {plan}, run {run}.\"]",
            "states = []",
            "the statements is empty",
        ),
    ];
    for (from, to, want) in cases {
        let got = refused(from, to);
        assert!(
            got == *want || (want.ends_with("shape") && got.starts_with(want)),
            "{to:?} for {from:?}:\n got  {got:?}\n want {want:?}"
        );
    }
}

#[test]
fn a_plan_with_no_steps_or_no_measures_is_refused() {
    let no_steps = BASE[..BASE.find("[[step]]").expect("steps")].to_owned()
        + &BASE[BASE.find("[[measure]]").expect("measures")..];
    assert_eq!(
        Plan::parse(&no_steps.replace("repetitions = 2", "repetitions = 2\nstep = []"))
            .err()
            .map(|e| e.to_string()),
        Some("the steps is empty".to_owned())
    );
    let no_measures = BASE[..BASE.find("[[measure]]").expect("measures")]
        .replace("repetitions = 2", "repetitions = 2\nmeasure = []");
    assert_eq!(
        Plan::parse(&no_measures).err().map(|e| e.to_string()),
        Some("the measures is empty".to_owned())
    );
}

/// The base plan with its watch replaced by `watch` (a TOML step) and `extra` steps first.
fn with_steps(hosts: &str, steps: &str) -> Result<Plan, String> {
    let head = &BASE[..BASE.find("[hosts]").expect("hosts")];
    let text = format!(
        "{head}[hosts]\nsays = \"Hosts.\"\n{hosts}\n{steps}\n[[measure]]\nid = \"n\"\nkind = \"count\"\nsays = \"events\"\nunit = \"events\"\nof = \"repetition/started\"\n"
    )
    .replace("{span.median}", "{n.max}");
    Plan::parse(&text).map_err(|e| e.to_string())
}

#[test]
fn the_host_of_a_step_and_the_watches_of_hosts_need_a_plan_that_knows_its_hosts() {
    let live = "pick = \"live\"";
    let none = "pick = \"none\"";
    let host_call = "[[step]]\nid = \"c\"\ndo = \"call\"\nargs = [\"hosts\", \"get\", \"--node-id\", \"{host}\"]\n";
    let audit = "[[step]]\nid = \"a\"\ndo = \"watch\"\nuntil = { what = \"audit\", actions = [\"enroll_issued\"] }\npoll_secs = 1\ntimeout_secs = 1\n";
    let beating = "[[step]]\nid = \"b\"\ndo = \"watch\"\nuntil = { what = \"beating\" }\npoll_secs = 1\ntimeout_secs = 1\n";
    let watch = |what: &str| {
        format!(
            "[[step]]\nid = \"w\"\ndo = \"watch\"\nuntil = {{ what = \"{what}\" }}\npoll_secs = 1\ntimeout_secs = 1\n"
        )
    };
    assert_eq!(
        with_steps(live, host_call).err(),
        Some("step \"c\": {host} before the plan knows its host".to_owned())
    );
    assert!(with_steps(live, &format!("{audit}{host_call}")).is_ok());
    assert_eq!(
        with_steps(none, beating).err(),
        Some("step \"b\": a beating watch before an audit watch".to_owned())
    );
    assert!(with_steps(none, &format!("{audit}{beating}")).is_ok());
    for what in ["live", "back"] {
        assert_eq!(
            with_steps(none, &watch(what)).err(),
            Some("step \"w\": a watch of the plan's hosts in a plan with none".to_owned()),
            "{what}"
        );
        assert!(with_steps(live, &watch(what)).is_ok(), "{what}");
    }
    assert!(with_steps(none, &watch("mdm-enrolled")).is_ok());
    let no_action = audit.replace("[\"enroll_issued\"]", "[]");
    let empty_action = audit.replace("[\"enroll_issued\"]", "[\"\"]");
    for broken in [no_action, empty_action] {
        assert_eq!(
            with_steps(none, &broken).err(),
            Some("step \"a\": an audit watch with no action".to_owned())
        );
    }
    let operator = "[[step]]\nid = \"o\"\ndo = \"operator\"\nsays = \"\"\n";
    assert_eq!(
        with_steps(none, operator).err(),
        Some("step \"o\" is empty".to_owned())
    );
    let health = "[[step]]\nid = \"h\"\ndo = \"watch\"\nuntil = { what = \"health\", service = \"{x}\", health = \"healthy\" }\npoll_secs = 1\ntimeout_secs = 1\n";
    assert_eq!(
        with_steps(live, health).err(),
        Some("step \"h\": {x} names no parameter \"x\"".to_owned())
    );
}
