//! The `agent` of a heartbeat: absent from an agent that predates it, present from one that
//! names itself, and dropped on its own when it is bad.

use serde_json::{Value, json};

use crate::version::Version;
use crate::wire::{
    Agent, AgentName, Heartbeat, LEGACY_AGENT_NAME, MAX_SERVICE_NAME, ServiceBeat, read_heartbeat,
};

fn v(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

fn wire(agent: Option<Value>) -> Vec<u8> {
    let mut wire = json!({"platform": "linux-arm64", "version": "1.0", "health": "healthy"});
    if let Some(agent) = agent {
        wire["agent"] = agent;
    }
    serde_json::to_vec(&wire).expect("json")
}

/// Catches: a heartbeat from an agent that predates the field counted as nobody: it counts as
/// the legacy agent, has no version, and is written back without an `agent` key.
#[test]
fn a_heartbeat_without_agent_counts_as_the_legacy_agent() {
    let beat: Heartbeat = serde_json::from_slice(&wire(None)).expect("strict");
    assert_eq!(beat.agent, None);
    assert_eq!(beat.agent_name(), "kbf-updater");
    assert_eq!(beat.agent_name(), LEGACY_AGENT_NAME);
    assert_eq!(beat.agent_version(), None);
    let written = serde_json::to_value(&beat).expect("json");
    assert!(written.get("agent").is_none(), "{written}");
}

/// Catches: an agent that is named but not read, or not written back as it was read.
#[test]
fn a_named_agent_is_read_and_written_back() {
    let agent = json!({"name": "hostkeeper-updater", "version": "0.3.1"});
    let beat: Heartbeat = serde_json::from_slice(&wire(Some(agent.clone()))).expect("strict");
    assert_eq!(beat.agent_name(), "hostkeeper-updater");
    assert_eq!(beat.agent_version(), Some(&v("0.3.1")));
    assert_eq!(
        beat.agent,
        Some(Agent {
            name: AgentName::parse("hostkeeper-updater").expect("a name"),
            version: v("0.3.1"),
        })
    );
    assert_eq!(serde_json::to_value(&beat).expect("json")["agent"], agent);
    let read = read_heartbeat(&wire(Some(agent)), None).expect("read");
    assert_eq!(read.beat, beat);
    assert_eq!(read.extras_dropped, None);
    assert!(read.extras_dropped_facts.is_empty());
}

/// Catches: a bad `agent` costing the host its heartbeat: every kind of bad value is dropped
/// on its own, named `agent`, and the heartbeat is read as one from the legacy agent. The
/// strict read refuses it.
#[test]
fn a_bad_agent_is_dropped_as_the_fact_agent() {
    for bad in [
        json!({"name": "Bad Name", "version": "1.0"}),
        json!({"name": "", "version": "1.0"}),
        json!({"name": "agent", "version": "not a version"}),
        json!({"name": "agent"}),
        json!({"name": "agent", "version": "1.0", "extra": 1}),
        json!("agent"),
        json!(7),
    ] {
        let bytes = wire(Some(bad.clone()));
        assert!(
            serde_json::from_slice::<Heartbeat>(&bytes).is_err(),
            "{bad}"
        );
        let read = read_heartbeat(&bytes, None).expect("the host is kept");
        assert_eq!(read.beat.agent, None, "{bad}");
        assert_eq!(read.beat.agent_name(), LEGACY_AGENT_NAME);
        assert_eq!(read.extras_dropped_facts, ["agent"], "{bad}");
        assert!(
            read.extras_dropped
                .as_deref()
                .is_some_and(|why| why.starts_with("agent: ")),
            "{bad}"
        );
        assert_eq!(read.services_dropped, None);
    }
}

/// Catches: a null `agent` counted as a bad one.
#[test]
fn a_null_agent_is_no_agent() {
    let read = read_heartbeat(&wire(Some(Value::Null)), None).expect("read");
    assert_eq!(read.beat.agent, None);
    assert!(read.extras_dropped_facts.is_empty());
}

/// Catches: the agent name grammar drifting from the service name rule: 1 to 64 bytes of
/// lowercase letters, digits and `-`, and an error that says so.
#[test]
fn an_agent_name_follows_the_service_name_rule() {
    let longest = "a".repeat(MAX_SERVICE_NAME);
    for ok in ["a", "kbf-updater", "x1-2", longest.as_str()] {
        let name = AgentName::parse(ok).expect(ok);
        assert_eq!(name.as_str(), ok);
        assert_eq!(String::from(name), ok);
        assert!(AgentName::try_from(ok.to_owned()).is_ok());
    }
    let too_long = "a".repeat(MAX_SERVICE_NAME + 1);
    for bad in ["", "Agent", "a b", "a_b", "a/b", too_long.as_str()] {
        assert_eq!(AgentName::parse(bad), None, "{bad:?}");
        assert_eq!(
            AgentName::try_from(bad.to_owned()),
            Err("not an agent name: 1 to 64 bytes of a-z, 0-9 and -"),
            "{bad:?}"
        );
    }
}

/// Catches: a service entry's state turned into a heartbeat that claims an agent, or a
/// platform other than the one given.
#[test]
fn a_service_beat_as_a_heartbeat_names_no_agent() {
    let entry: ServiceBeat = serde_json::from_value(
        json!({"name": "db", "version": "2.0", "health": "unhealthy", "updating": "3.0"}),
    )
    .expect("an entry");
    let beat = entry.as_heartbeat(crate::names::Platform::DarwinArm64);
    assert_eq!(beat.agent, None);
    assert_eq!(beat.agent_name(), LEGACY_AGENT_NAME);
    assert_eq!(beat.platform, crate::names::Platform::DarwinArm64);
    assert_eq!(beat.version, v("2.0"));
    assert_eq!(beat.updating, Some(v("3.0")));
    assert_eq!(beat.services, None);
}
