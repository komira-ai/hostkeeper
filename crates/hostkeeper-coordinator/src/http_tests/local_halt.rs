//! Over HTTP: a host that holds a halt of a service of its own (`local_halt` in its beat) is held
//! back under `halted_here`, or `stale_halt_here` when the generation it holds is above any this
//! coordinator issued, never `failed_here`; the operator's clear issues a generation past it, so
//! the host lifts its halt, and its `halted` refusal is set aside so it is ordered again.

use super::*;
use crate::http::OPERATOR_PATH;

/// The report of a hook that halted the update to `target`.
fn halt_report(target: &str) -> String {
    format!(
        r#","last_update":{{"target":"{target}","outcome":"failed","phase":"halt","reason":"disk_tainted"}}"#
    )
}

/// The refusal a daemon that holds a halt of the service gives the update to 2.0.
const HALTED: &str = r#","last_update":{"target":"2.0","outcome":"failed","reason":"halted"}"#;

/// A daemon that announced `halt-v1`.
const AGENT: &str = r#","agent_features":["halt-v1"]"#;

fn beat(version: &str, extra: &str) -> String {
    format!(r#"{{"platform":"linux-x86_64","version":"{version}","health":"healthy"{extra}}}"#)
}

/// The beat's `local_halt` for a halt held under `generation`.
fn holding(generation: u64) -> String {
    format!(
        r#","local_halt":{{"reason":"disk_tainted","generation":{generation},"age_secs":604800}}"#
    )
}

/// The answer to `host`'s heartbeat `body`, as the JSON the route answers, its `features` left
/// out: the orders go by the typed transport, which the JSON route does not carry.
async fn send(rig: &Rig, host: &str, role: &str, body: &str) -> Value {
    let beat: hostkeeper_wire::wire::Heartbeat = serde_json::from_str(body).expect("a heartbeat");
    let answer = rig
        .coordinator
        .heartbeat_native(crate::marks_support::who(host, role), beat)
        .expect("answered");
    let mut reply = serde_json::to_value(&answer).expect("JSON");
    if let Some(fields) = reply.as_object_mut() {
        fields.remove("features");
    }
    reply
}

async fn operate(rig: &Rig, body: &str) -> (StatusCode, Value) {
    call(
        rig,
        Surface::Control,
        &None,
        request("POST", OPERATOR_PATH, Some(OPERATOR), body),
    )
    .await
}

/// The operator's clear of `kbf`'s halt.
async fn clear(rig: &Rig) -> (StatusCode, Value) {
    operate(
        rig,
        r#"{"action":"clear_halt","service":"kbf","reason":"stale hold"}"#,
    )
    .await
}

/// The rule that holds `host`'s order of `kbf` back, as the status says it.
async fn hold_of(rig: &Rig, host: &str) -> Value {
    let status = status(rig).await;
    status["per_service"]["kbf"]["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["node_id"] == host)
        .map_or(Value::Null, |h| h["hold"].clone())
}

/// The generation of `kbf` the ops store holds.
async fn generation(rig: &Rig) -> Value {
    status(rig).await["ops"]["services"]["kbf"]["halt_generation"].clone()
}

/// A rig past the quiet start whose `kbf` wants 2.0 with no hosts required up, and whose
/// generation of `kbf` is `issued`: as many halts of s1 cleared.
async fn farm(name: &str, issued: u64) -> Rig {
    let rig = Rig::new(name);
    rig.at(1_100);
    rig.set(r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#)
        .await;
    for round in 1..=issued {
        send(
            &rig,
            "s1",
            "storage",
            &beat("1.0", &halt_report(&format!("2.{round}"))),
        )
        .await;
        let (code, entry) = clear(&rig).await;
        assert_eq!(code, StatusCode::OK, "{entry}");
    }
    assert_eq!(
        generation(&rig).await,
        if issued == 0 {
            Value::Null
        } else {
            json!(issued)
        }
    );
    rig
}

/// Proves: a host that holds a halt under generation 15, which a coordinator that has issued 3
/// never sends, is shown `stale_halt_here` (not `failed_here`) and ordered nothing; the
/// operator's clear, with no halt of the coordinator standing, is accepted and audited naming no
/// host, and issues 16, past the held 15, which the host is told; while the host still reports
/// the halt it is `halted_here` (16 is now above it); once it reports the halt lifted, its
/// `halted` refusal no longer holds it, and it is ordered the update to 2.0.
///
/// Catches: a hold no generation of this coordinator can lift: a clear refused `not halted`, or
/// one that counts from 3 to 4 and never reaches 15, and a host left at `failed_here` for good.
#[tokio::test]
async fn a_halt_held_above_every_generation_issued_is_stale_and_the_next_clear_lifts_it() {
    let rig = farm("local-halt-stale", 3).await;
    let held = format!("{AGENT}{}{HALTED}", holding(15));
    let reply = send(&rig, "d1", "daemon", &beat("1.0", &held)).await;
    assert_eq!(reply, json!({"reply": "nothing", "halts": {"kbf": 3}}));
    assert_eq!(hold_of(&rig, "d1").await, "stale_halt_here");

    let (code, entry) = clear(&rig).await;
    assert_eq!(code, StatusCode::OK, "{entry}");
    assert_eq!(entry["action"], "clear_halt");
    assert_eq!(entry["service"], "kbf");
    assert!(entry.get("node_id").is_none(), "{entry}");
    assert_eq!(generation(&rig).await, 16);

    let reply = send(&rig, "d1", "daemon", &beat("1.0", &held)).await;
    assert_eq!(reply, json!({"reply": "nothing", "halts": {"kbf": 16}}));
    assert_eq!(hold_of(&rig, "d1").await, "halted_here");

    let lifted = format!("{AGENT}{HALTED}");
    let reply = send(&rig, "d1", "daemon", &beat("1.0", &lifted)).await;
    assert_eq!(
        reply,
        json!({"reply": "update", "version": "2.0", "halts": {"kbf": 16}})
    );
}

/// Proves: a host that holds a halt under the generation the coordinator issued last (3) is shown
/// `halted_here`, and the clear issues the next generation, 4, as a clear always has.
///
/// Catches: a current halt read as stale, or a clear that jumps past a generation no host holds.
#[tokio::test]
async fn a_halt_held_under_the_generation_issued_last_is_halted_here_and_cleared_by_the_next() {
    let rig = farm("local-halt-current", 3).await;
    let held = format!("{AGENT}{}{HALTED}", holding(3));
    send(&rig, "d1", "daemon", &beat("1.0", &held)).await;
    assert_eq!(hold_of(&rig, "d1").await, "halted_here");
    let (code, _) = clear(&rig).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(generation(&rig).await, 4);
}

/// Proves: a halt of the coordinator standing comes before a host's own (`halted`); its clear
/// names the host that raised it and issues a generation past the one another host holds (16
/// past 15, with none issued before).
///
/// Catches: a clear of a standing halt that counts from its own generation alone, leaving the
/// host that holds 15 halted after it.
#[tokio::test]
async fn a_standing_halt_comes_first_and_its_clear_issues_past_a_hosts_own() {
    let rig = farm("local-halt-standing", 0).await;
    send(&rig, "s1", "storage", &beat("1.0", &halt_report("2.0"))).await;
    let held = format!("{AGENT}{}{HALTED}", holding(15));
    send(&rig, "d1", "daemon", &beat("1.0", &held)).await;
    assert_eq!(hold_of(&rig, "d1").await, "halted");
    let (code, entry) = clear(&rig).await;
    assert_eq!(code, StatusCode::OK, "{entry}");
    assert_eq!(entry["node_id"], "s1");
    assert_eq!(generation(&rig).await, 16);
}

/// Proves: with no halt standing and no host reporting one of its own, a clear is still refused
/// and issues nothing; a host that reports a `halted` refusal but no `local_halt` (a daemon that
/// predates it) is `failed_here` as before.
///
/// Catches: a clear accepted for a halt no one holds, which would count a generation for
/// nothing.
#[tokio::test]
async fn with_no_halt_held_anywhere_a_clear_is_refused() {
    let rig = farm("local-halt-none", 0).await;
    send(
        &rig,
        "d1",
        "daemon",
        &beat("1.0", &format!("{AGENT}{HALTED}")),
    )
    .await;
    assert_eq!(hold_of(&rig, "d1").await, "failed_here");
    let (code, _) = clear(&rig).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(generation(&rig).await, Value::Null);
}

/// Proves: the clear sets aside only a `halted` refusal: a host that held a halt of its own and
/// whose last report is the hook's halt of 2.0 (phase `halt`) is still `failed_here` for 2.0 once
/// it reports the halt lifted, as after any clear, until `Hosts.Retry`.
///
/// Catches: a clear that forgets a failure of the version itself, so the version the hook halted
/// is ordered to the host again.
#[tokio::test]
async fn a_clear_sets_aside_only_a_halted_refusal() {
    let rig = farm("local-halt-phase", 0).await;
    let report = halt_report("2.0");
    let held = format!("{AGENT}{}{report}", holding(15));
    send(&rig, "d1", "daemon", &beat("1.0", &held)).await;
    let (code, _) = clear(&rig).await;
    assert_eq!(code, StatusCode::OK);
    let lifted = format!("{AGENT}{report}");
    let reply = send(&rig, "d1", "daemon", &beat("1.0", &lifted)).await;
    assert_eq!(reply["reply"], "nothing");
    assert_eq!(hold_of(&rig, "d1").await, "failed_here");
}

/// Proves: the clear sets aside the `halted` refusal of the host that held a halt of its own and
/// not that of a host that reported none (a daemon that predates `local_halt`): once the first
/// reports its halt lifted it is no longer `failed_here` (it waits for the stage the second's
/// failure keeps open), and the second still is.
///
/// Catches: a clear that sets aside every `halted` refusal, ordering a host whose halt no
/// generation the clear issued is known to lift.
#[tokio::test]
async fn a_clear_sets_aside_the_refusal_only_of_a_host_that_held_a_halt() {
    let rig = farm("local-halt-mixed", 0).await;
    let refused = format!("{AGENT}{HALTED}");
    send(
        &rig,
        "d1",
        "daemon",
        &beat("1.0", &format!("{AGENT}{}{HALTED}", holding(15))),
    )
    .await;
    send(&rig, "d2", "daemon", &beat("1.0", &refused)).await;
    let (code, _) = clear(&rig).await;
    assert_eq!(code, StatusCode::OK);
    send(&rig, "d1", "daemon", &beat("1.0", &refused)).await;
    send(&rig, "d2", "daemon", &beat("1.0", &refused)).await;
    assert_eq!(hold_of(&rig, "d2").await, "failed_here");
    // d1's refusal no longer holds it; it waits for the stage d2's failure keeps open.
    assert_eq!(hold_of(&rig, "d1").await, "stage_failed");
}
