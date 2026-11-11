//! Services as data: the file that defines them, and the decision across two services.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use hostkeeper_wire::wire::{MAX_SERVICE_NAME, MAX_SERVICES, Reply};

use crate::desired::Desired;
use crate::fleet::{Fleet, ORDER_AGE};
use crate::services::{Services, ServicesError, VersionScheme};
use crate::testutil::{
    beat, decide_in, desired, id, kbf_def, kbf_services, role, scratch, updating, version,
};

const NOW: Duration = Duration::from_secs(1_000);

const TWO: &str = r#"
[[service]]
name = "server"
roles = ["daemon"]

[[service]]
name = "agent"
roles = ["daemon", "storage"]
priority = 3
"#;

fn update(target: &str) -> Reply {
    Reply::Update {
        version: version(target),
    }
}

fn wants(entries: &[(&str, &Desired)]) -> BTreeMap<String, Desired> {
    entries
        .iter()
        .map(|(name, d)| ((*name).to_owned(), (*d).clone()))
        .collect()
}

fn ask(s: &Services, w: &BTreeMap<String, Desired>, f: &Fleet, service: &str, host: &str) -> Reply {
    decide_in(s, w, service, f, &id(host), NOW)
}

fn refusal(text: &str) -> String {
    Services::parse(text).expect_err("refused").to_string()
}

/// Proves: kbf's definition is the recorded file's data: its four roles and nothing else.
///
/// Catches: a service built in to the coordinator: nothing is defined by default or by an
/// empty file, and kbf's definition names all four roles and no other setting.
#[test]
fn no_service_is_built_in_and_kbf_is_data() {
    assert_eq!(Services::default().names().count(), 0);
    assert_eq!(Services::parse("").expect("empty"), Services::default());
    let kbf = kbf_def();
    assert_eq!(kbf.name, "kbf");
    assert_eq!(kbf.version_scheme, VersionScheme::Semver);
    assert_eq!(
        kbf.roles,
        BTreeSet::from(["frontend", "storage", "daemon", "mac-daemon"].map(str::to_owned))
    );
    assert_eq!(kbf_services().get("kbf"), Some(&kbf));
}

/// Catches: a rollout file that is read wrongly: the two services of the file are defined
/// with their roles and priority, and only those (no kbf); and a file that cannot be read is
/// refused.
#[test]
fn a_rollout_file_defines_further_services() {
    let s = Services::parse(TWO).expect("parsed");
    let agent = s.get("agent").expect("defined");
    assert_eq!(
        agent.roles,
        BTreeSet::from(["daemon".to_owned(), "storage".to_owned()])
    );
    assert_eq!(agent.priority, 3);
    assert!(s.get("server").is_some());
    assert!(s.get("kbf").is_none());
    assert_eq!(s.names().collect::<Vec<_>>(), ["agent", "server"]);

    let dir = scratch("rollout-file");
    let path = dir.path().join("services.toml");
    std::fs::write(&path, TWO).expect("written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    assert_eq!(Services::from_file(&path).expect("read"), s);
    let missing = Services::from_file(&dir.path().join("nope.toml")).expect_err("refused");
    assert!(matches!(missing, ServicesError::Read(_)));
    assert!(missing.to_string().contains("nope.toml"));
}

/// Catches: a service name using digits or `-` being refused, as if names were letters only.
#[test]
fn a_service_name_may_hold_digits_and_hyphens() {
    let text = "[[service]]\nname = \"web-2\"\nroles = [\"daemon\"]\n";
    assert!(
        Services::parse(text)
            .expect("parsed")
            .get("web-2")
            .is_some()
    );
}

/// Catches: a rollout file that is accepted although it cannot work: each refusal names what
/// is wrong (not TOML or an unknown key, a bad or repeated name, no roles, a role that is no
/// role name or is listed twice, and a CI service that does not exist).
#[test]
fn a_rollout_file_that_cannot_work_is_refused() {
    let role = "roles = [\"daemon\"]\n";
    let svc = |name: &str, extra: &str, roles: &str| {
        format!("[[service]]\nname = \"{name}\"\n{extra}\n{roles}")
    };
    assert!(matches!(
        Services::parse("service = 3").expect_err("refused"),
        ServicesError::Parse(_)
    ));
    assert!(matches!(
        Services::parse("[[service]]\nname = \"a\"\nbogus = 1\n").expect_err("refused"),
        ServicesError::Parse(_)
    ));
    for bad in ["", "Upper", "has space"] {
        assert!(
            refusal(&svc(bad, "", role)).contains("lowercase"),
            "{bad:?}"
        );
    }
    // No name is reserved: kbf is a service like any other, anywhere in the file.
    let kbf_later = format!("{}{}", svc("a", "", role), svc("kbf", "", role));
    Services::parse(&kbf_later).expect("kbf is no reserved name");
    let twice = format!("{}{}", svc("a", "", role), svc("a", "", role));
    assert!(refusal(&twice).contains("twice"));
    assert!(refusal(&svc("a", "", "")).contains("no roles"));
    assert!(refusal(&svc("a", "", "roles = [\"Cook\"]\n")).contains("Cook is not a role name"));
    assert!(refusal(&svc("a", "", "roles = [\"a--b\"]\n")).contains("a--b is not a role name"));
    // Roles are open: any well-formed name is a role.
    Services::parse(&svc("a", "", "roles = [\"cook\", \"cache\"]\n")).expect("roles are open");
    assert!(
        refusal(&svc("a", "", "roles = [\"daemon\", \"daemon\"]\n"))
            .contains("roles lists daemon twice")
    );
    // Which services a token may set is the allow-list's: the old key is refused by name.
    assert!(
        refusal(&format!("ci_services = [\"kbf\"]\n{}", svc("a", "", role)))
            .contains("ci_services")
    );
    Services::parse(&svc("js-server2", "", role)).expect("letters, digits and - make a name");
}

/// Proves: a key an earlier build read and the rollout file no longer has is refused with its
/// name, never ignored, and the table form of `roles` is refused with the text that says what
/// to write.
///
/// Catches: a removed key silently dropped, which would leave an operator believing a limit
/// still holds.
#[test]
fn each_removed_key_is_refused_by_name() {
    for key in [
        "after",
        "require_min_up",
        "max_updating",
        "min_up",
        "never_last",
    ] {
        let text = format!("[[service]]\nname = \"a\"\nroles = [\"daemon\"]\n{key} = 1\n");
        let why = Services::parse(&text).expect_err("refused");
        assert!(matches!(why, ServicesError::Parse(_)), "{key}");
        assert!(
            why.to_string().contains(&format!("unknown field `{key}`")),
            "{key}: {why}"
        );
    }
    // The table form of roles, with each key an earlier build read in it.
    for key in ["max_updating", "min_up", "never_last", "after"] {
        let text = format!("[[service]]\nname = \"a\"\n[service.roles.daemon]\n{key} = 1\n");
        let why = Services::parse(&text).expect_err("refused");
        assert!(matches!(why, ServicesError::Invalid(_)), "{key}");
        let said = why.to_string();
        assert!(
            said.contains("roles must be a list of role names"),
            "{key}: {said}"
        );
        assert!(said.contains("of type table"), "{key}: {said}");
        assert!(said.contains(key), "{key}: {said}");
    }
    assert_eq!(
        refusal("[[service]]\nname = \"a\"\n[service.roles.daemon]\n"),
        "services: a: roles must be a list of role names, roles = [\"daemon\"], not a value of \
         type table; the [service.roles.<name>] tables, with their max_updating, min_up, \
         never_last and after keys, are no longer read"
    );
}

/// Catches: `install_when_absent = true` silently ignored, which would leave an operator believing
/// absent hosts are updated as before: it is refused with the text that names what replaced it,
/// while `false`, the old default and what an agent that cannot take an install still gets,
/// reads as before.
#[test]
fn install_when_absent_true_is_refused_and_false_still_reads() {
    let text = |value: bool| {
        format!("[[service]]\nname = \"a\"\nroles = [\"daemon\"]\ninstall_when_absent = {value}\n")
    };
    assert_eq!(
        refusal(&text(true)),
        "services: a: install_when_absent = true is no longer read: a host the service is \
         assigned to that does not run it is ordered to install it; take the key out"
    );
    let read = Services::parse(&text(false)).expect("read");
    assert_eq!(read.names().collect::<Vec<_>>(), ["a"]);
}

/// Proves: `version_scheme` is optional, `semver` is its default and its only value, and any
/// other value is refused naming the key.
///
/// Catches: a scheme the coordinator cannot order versions by being accepted, so that a
/// release would be compared by the wrong rule.
#[test]
fn version_scheme_defaults_to_semver_and_any_other_value_is_refused() {
    let role = "roles = [\"daemon\"]\n";
    let svc = |extra: &str| format!("[[service]]\nname = \"a\"\n{extra}\n{role}");
    let named = Services::parse(&svc("version_scheme = \"semver\"")).expect("semver");
    let absent = Services::parse(&svc("")).expect("no scheme");
    assert_eq!(named, absent);
    assert_eq!(
        absent.get("a").expect("defined").version_scheme,
        VersionScheme::Semver
    );
    assert_eq!(VersionScheme::default(), VersionScheme::Semver);
    assert_eq!(
        refusal(&svc("version_scheme = \"calver\"")),
        "services: a: version_scheme is \"calver\", and \"semver\" is the only scheme"
    );
    assert_eq!(
        refusal(&svc("version_scheme = \"\"")),
        "services: a: version_scheme is \"\", and \"semver\" is the only scheme"
    );
    let why = Services::parse(&svc("version_scheme = 2")).expect_err("refused");
    assert!(matches!(why, ServicesError::Parse(_)));
    assert!(why.to_string().contains("version_scheme"), "{why}");
}

/// Proves: `roles` is a list of role names: any other value is refused with the type it was,
/// an empty or missing list is a service with no roles, and the roles come back in the order
/// of the desired-state format first and by name after.
///
/// Catches: a string, a mixed array or a table read as roles, and a role order that follows
/// the file and not the format.
#[test]
fn roles_are_a_list_of_role_names() {
    let svc = |roles: &str| format!("[[service]]\nname = \"a\"\n{roles}");
    for (roles, kind) in [
        ("roles = \"daemon\"\n", "string"),
        ("roles = [\"daemon\", 3]\n", "array"),
        ("roles = { daemon = 1 }\n", "table"),
    ] {
        let why = Services::parse(&svc(roles)).expect_err("refused");
        assert!(matches!(why, ServicesError::Invalid(_)), "{roles}");
        let said = why.to_string();
        assert!(
            said.contains("roles must be a list of role names"),
            "{roles}: {said}"
        );
        assert!(said.contains(&format!("of type {kind}")), "{roles}: {said}");
    }
    assert!(refusal(&svc("roles = []\n")).contains("has no roles"));
    assert!(refusal(&svc("")).contains("has no roles"));
    let names = |roles: &str| {
        let services = Services::parse(&svc(roles)).expect("parsed");
        let def = services.get("a").expect("defined");
        def.role_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names("roles = [\"storage\", \"daemon\"]\n"),
        ["storage", "daemon"]
    );
    assert_eq!(
        names("roles = [\"zeta\", \"alpha\", \"daemon\", \"frontend\"]\n"),
        ["frontend", "daemon", "alpha", "zeta"]
    );
}

/// Catches: `after` read again: the second service is held for the first. The agent host is
/// told to update while a server host is behind, and with no desired state for the server; it
/// is told nothing only while a server host is updating (the one live update), and a silent
/// server host does not hold it.
#[test]
fn a_service_waits_for_no_other_service() {
    let s = Services::parse(TWO).expect("parsed");
    let want = desired("2.0", 0, 0);
    let w = wants(&[("server", &want), ("agent", &want)]);
    let fleet_of = |server: hostkeeper_wire::wire::Heartbeat, seen: u64| {
        let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
        f.record_in(
            "server",
            id("s1"),
            role("daemon"),
            server,
            Duration::from_secs(seen),
        );
        f.record_in("agent", id("a1"), role("daemon"), beat("1.0"), NOW);
        f
    };
    let behind = fleet_of(beat("1.0"), 1_000);
    assert_eq!(ask(&s, &w, &behind, "agent", "a1"), update("2.0"));
    assert_eq!(ask(&s, &w, &behind, "server", "s1"), update("2.0"));
    let busy = fleet_of(updating(beat("1.0"), "2.0"), 1_000);
    assert_eq!(ask(&s, &w, &busy, "agent", "a1"), Reply::Nothing);
    let done = fleet_of(beat("2.0"), 1_000);
    assert_eq!(ask(&s, &w, &done, "agent", "a1"), update("2.0"));
    let gone = fleet_of(beat("1.0"), 900);
    assert_eq!(ask(&s, &w, &gone, "agent", "a1"), update("2.0"));
    let agent_only = wants(&[("agent", &want)]);
    assert_eq!(ask(&s, &agent_only, &behind, "agent", "a1"), update("2.0"));
}

/// Catches: a host running two updates at once: a host whose other service is updating, or
/// has an order outstanding for it, is told nothing; an order that has aged out, or one for
/// the service asked about, does not block; and an order for one service is not ended by
/// the host's heartbeat of the other.
#[test]
fn a_host_updates_one_service_at_a_time() {
    let text = "[[service]]\nname = \"one\"\nroles = [\"daemon\"]\n\
                [[service]]\nname = \"two\"\nroles = [\"daemon\"]\n";
    let s = Services::parse(text).expect("parsed");
    let want = desired("2.0", 0, 0);
    let w = wants(&[("one", &want), ("two", &want)]);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_in("one", id("h"), role("daemon"), beat("1.0"), NOW);
    f.record_in("two", id("h"), role("daemon"), beat("1.0"), NOW);
    assert!(!f.is_busy_besides("two", &id("h"), NOW));
    assert_eq!(ask(&s, &w, &f, "two", "h"), update("2.0"));

    f.order("one", id("h"), version("2.0"), NOW);
    assert_eq!(
        ask(&s, &w, &f, "two", "h"),
        Reply::Nothing,
        "ordered for one"
    );
    assert_eq!(
        ask(&s, &w, &f, "one", "h"),
        update("2.0"),
        "its own order repeats"
    );
    f.record_in("two", id("h"), role("daemon"), beat("1.0"), NOW);
    assert_eq!(
        ask(&s, &w, &f, "two", "h"),
        Reply::Nothing,
        "two's beat leaves one's order"
    );
    let later = NOW + ORDER_AGE;
    assert_eq!(
        decide_in(&s, &w, "two", &f, &id("h"), later),
        update("2.0"),
        "aged out"
    );

    f.record_in(
        "one",
        id("h"),
        role("daemon"),
        updating(beat("1.0"), "2.0"),
        NOW,
    );
    assert_eq!(
        ask(&s, &w, &f, "two", "h"),
        Reply::Nothing,
        "one is updating"
    );
    assert_eq!(
        ask(&s, &w, &f, "one", "h"),
        Reply::Nothing,
        "and not told again"
    );
    f.record_in("one", id("h"), role("daemon"), beat("2.0"), NOW);
    assert_eq!(ask(&s, &w, &f, "two", "h"), update("2.0"));
}

/// Catches: a lone host refused (no minimum is read), a role the service lacks ordered, and a
/// service with no definition or no desired state ordered: a host is ordered only for a
/// service that runs on its role.
#[test]
fn a_host_is_ordered_only_for_a_service_that_runs_on_its_role() {
    let text = "[[service]]\nname = \"loose\"\nroles = [\"daemon\"]\n";
    let s = Services::parse(text).expect("parsed");
    let want = desired("2.0", 0, 0);
    let w = wants(&[("loose", &want)]);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_in("loose", id("d1"), role("daemon"), beat("1.0"), NOW);
    f.record_in("loose", id("m1"), role("mac-daemon"), beat("1.0"), NOW);
    assert_eq!(ask(&s, &w, &f, "loose", "d1"), update("2.0"));
    assert_eq!(
        ask(&s, &w, &f, "loose", "m1"),
        Reply::Nothing,
        "no mac-daemon role"
    );
    assert_eq!(ask(&s, &w, &f, "nope", "d1"), Reply::Nothing);
    assert_eq!(ask(&s, &BTreeMap::new(), &f, "loose", "d1"), Reply::Nothing);
}

/// Catches: the rollout file's name check drifting from the wire's: a name of exactly
/// `MAX_SERVICE_NAME` bytes is defined, one byte more is refused and the refusal says why.
#[test]
fn a_service_name_has_the_wires_length_cap() {
    let role = "roles = [\"daemon\"]\n";
    let svc = |name: &str| format!("[[service]]\nname = \"{name}\"\n{role}");
    Services::parse(&svc(&"a".repeat(MAX_SERVICE_NAME))).expect("at the cap");
    let why = refusal(&svc(&"a".repeat(MAX_SERVICE_NAME + 1)));
    assert!(why.contains("64 bytes"), "{why}");
}

/// Catches: no cap on the services a file defines, or one off by one: exactly
/// `MAX_SERVICES` are defined, one more is refused.
#[test]
fn a_rollout_file_defines_at_most_the_most_services() {
    let file = |n: usize| {
        (0..n)
            .map(|i| format!("[[service]]\nname = \"svc-{i}\"\nroles = [\"daemon\"]\n"))
            .collect::<Vec<_>>()
            .concat()
    };
    Services::parse(&file(MAX_SERVICES)).expect("at the cap");
    assert!(refusal(&file(MAX_SERVICES + 1)).contains("more than 16 services"));
}
