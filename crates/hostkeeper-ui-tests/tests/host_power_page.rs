//! The "Power and boot" tab of the Host page, run in the JavaScript engine against the fake
//! gateway of `support`: every button needs a reason, asks first, sends one Console call with the
//! body the proto reads and says what the BMC reported; none is sent for a reason that breaks its
//! rule or a question answered no, and drawing the page calls nothing of the Console service.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

const HOST: &str = r#"{ nodeId: "node-02" }"#;

/// A gateway that knows `node-02`, as the Host page reads it.
fn start() -> Ui {
    let ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "node-02", "role": "web"}}),
    );
    ui
}

/// The calls the page makes when it is drawn, before any button.
fn drawn() -> Vec<(String, Value)> {
    vec![
        (m("Hosts/Get"), json!({"nodeId": "node-02"})),
        (m("Services/List"), json!({})),
        (m("HostGroups/List"), json!({})),
        (m("Ssh/HostKeys"), json!({"hostId": "node-02"})),
        (m("Layout/Get"), json!({})),
    ]
}

/// The Console calls the gateway recorded after the page was drawn.
fn console_calls(ui: &Ui) -> Vec<(String, Value)> {
    bodies(ui).split_off(drawn().len())
}

const REASON: &str = "type(tree, \"power-reason\", \"  disk replaced  \");\n";

#[test]
fn every_power_button_sends_its_action_with_the_trimmed_reason_and_says_what_the_bmc_reported() {
    // Catches: a button that sends another action than its own, a reason sent untrimmed, a send
    // without the operator's yes, the state before the command lost, a cycle of an off host
    // reported as a cycle, and a soft shutdown answered as if the host were already off.
    let ui = start();
    let answers = [
        json!({"state": "POWER_STATE_ON"}),
        json!({"state": "POWER_STATE_OFF", "sent": "POWER_ACTION_ON"}),
        json!({"state": "POWER_STATE_ON", "sent": "POWER_ACTION_SOFT"}),
        json!({"state": "POWER_STATE_ON", "sent": "POWER_ACTION_OFF"}),
        json!({"state": "POWER_STATE_ON", "sent": "POWER_ACTION_CYCLE"}),
        json!({"state": "POWER_STATE_OFF", "sent": "POWER_ACTION_ON"}),
        json!({"state": "POWER_STATE_ON", "sent": "POWER_ACTION_RESET"}),
    ];
    for answer in answers {
        ui.reply(&m("Console/Power"), answer);
    }
    let mut ui = ui;
    let out = ui.run(&page_driver(
        "host",
        HOST,
        "[true, true, true, true, true, true, true]",
        &format!(
            "{REASON}{}",
            ["status", "on", "soft", "off", "cycle", "cycle", "reset"]
                .map(|n| format!("await click(tree, \"power-{n}\");\n"))
                .concat()
        ),
    ));
    let sent = |action: &str| {
        (
            m("Console/Power"),
            json!({"hostId": "node-02", "action": action, "reason": "disk replaced"}),
        )
    };
    let mut expected = drawn();
    expected.extend(
        ["STATUS", "ON", "SOFT", "OFF", "CYCLE", "CYCLE", "RESET"]
            .map(|a| sent(&format!("POWER_ACTION_{a}"))),
    );
    assert_eq!(bodies(&ui), expected);
    let seen = &out["seen"];
    let cuts = "the running system is stopped at once, and a soft shutdown still running is cut.";
    assert_eq!(
        seen["questions"],
        json!([
            "Send status to node-02?",
            "Send on to node-02?",
            "Send soft shutdown to node-02? Soft shutdown asks the host's system to shut itself down; it does so in its own time, and this answer does not say when it is off.",
            format!("Send off to node-02? Off cuts the power: {cuts}"),
            format!("Send cycle to node-02? Cycle cuts the power and turns it on: {cuts}"),
            format!("Send cycle to node-02? Cycle cuts the power and turns it on: {cuts}"),
            "Send reset to node-02? Reset restarts the host at once: the running system is stopped, and a soft shutdown still running is cut.",
        ])
    );
    assert_eq!(
        seen["reports"],
        json!([
            "node-02 is on.",
            "node-02 was off; on sent.",
            "node-02 was on; soft shutdown sent. The host shuts down in its own time.",
            "node-02 was on; off sent.",
            "node-02 was on; cycle sent.",
            "node-02 was off; on sent instead of cycle.",
            "node-02 was on; reset sent.",
        ])
    );
    assert_eq!(seen["refreshes"], 0, "power changes nothing the page shows");
}

#[test]
fn a_no_to_the_question_sends_nothing_and_says_so() {
    // Catches: a power command or a boot device change sent after the operator answered no.
    let mut ui = start();
    let out = ui.run(&page_driver(
        "host",
        HOST,
        "[false, false]",
        &format!(
            "{REASON}await click(tree, \"power-off\");\nawait click(tree, \"set-boot-device\");"
        ),
    ));
    assert_eq!(bodies(&ui), drawn());
    assert_eq!(out["seen"]["reports"], json!(["Not sent.", "Not sent."]));
    assert_eq!(out["seen"]["questions"].as_array().map(Vec::len), Some(2));
}

#[test]
fn a_reason_that_is_missing_or_breaks_its_rule_sends_nothing() {
    // Catches: a Console call with no reason, a blank one, one over 256 bytes (a multi-byte
    // character counts its bytes) or one with a control character; and a question asked before
    // the reason is checked.
    let mut ui = start();
    let out = ui.run(&page_driver(
        "host",
        HOST,
        "[true]",
        r#"
await click(tree, "power-cycle");
type(tree, "power-reason", "   ");
await click(tree, "power-on");
type(tree, "power-reason", "x".repeat(257));
await click(tree, "power-off");
type(tree, "power-reason", "é".repeat(129));
await click(tree, "power-status");
type(tree, "power-reason", "a\tb");
await click(tree, "power-reset");
await click(tree, "set-boot-device");
type(tree, "power-reason", "");
await click(tree, "set-boot-device");
"#,
    ));
    assert_eq!(bodies(&ui), drawn());
    let tab = "A reason has no control character (a tab or a line break, for example).";
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason first.",
            "Give a reason first.",
            "A reason is at most 256 bytes; this one is 257.",
            "A reason is at most 256 bytes; this one is 258.",
            tab,
            tab,
            "Give a reason first.",
        ])
    );
    assert_eq!(out["seen"]["questions"], json!([]));
}

#[test]
fn set_boot_device_sends_the_chosen_flags_and_says_what_the_bmc_reads_back() {
    // Catches: a boot device sent that is not the one chosen (disk is the first choice), a flag
    // lost or sent inverted, the read-back flags replaced by the ones asked for, and an answer
    // with no device (no override) worded as a device.
    let mut ui = start();
    for answer in [
        json!({"device": "BOOT_DEVICE_DISK", "persistent": true, "efi": true}),
        json!({"device": "BOOT_DEVICE_PXE"}),
        json!({"device": "BOOT_DEVICE_BIOS", "efi": true}),
        json!({}),
    ] {
        ui.reply(&m("Console/SetBootDevice"), answer);
    }
    let out = ui.run(&page_driver(
        "host",
        HOST,
        "[true, true, true, true]",
        &format!(
            r#"{REASON}
const first = chosen(tree, "boot-device");
const offered = choices(tree, "boot-device");
tick(tree, "boot-persistent", true);
tick(tree, "boot-efi", true);
await click(tree, "set-boot-device");
choose(tree, "boot-device", "BOOT_DEVICE_PXE");
tick(tree, "boot-persistent", false);
tick(tree, "boot-efi", false);
await click(tree, "set-boot-device");
choose(tree, "boot-device", "BOOT_DEVICE_BIOS");
tick(tree, "boot-efi", true);
await click(tree, "set-boot-device");
await click(tree, "set-boot-device");
return {{ first, offered }};
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!({"first": "BOOT_DEVICE_DISK", "offered": ["Disk", "Network (PXE)", "BIOS setup"]})
    );
    let sent = |device: &str, persistent: bool, efi: bool| {
        (
            m("Console/SetBootDevice"),
            json!({"hostId": "node-02", "device": device, "persistent": persistent,
                   "efi": efi, "reason": "disk replaced"}),
        )
    };
    let mut expected = drawn();
    expected.extend([
        sent("BOOT_DEVICE_DISK", true, true),
        sent("BOOT_DEVICE_PXE", false, false),
        sent("BOOT_DEVICE_BIOS", false, true),
        sent("BOOT_DEVICE_BIOS", false, true),
    ]);
    assert_eq!(bodies(&ui), expected);
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Set the boot device of node-02 to disk, every boot, UEFI?",
            "Set the boot device of node-02 to the network (PXE), next boot only, legacy BIOS?",
            "Set the boot device of node-02 to BIOS setup, next boot only, UEFI?",
            "Set the boot device of node-02 to BIOS setup, next boot only, UEFI?",
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "The BMC of node-02 now boots disk, every boot, UEFI.",
            "The BMC of node-02 now boots the network (PXE), next boot only, legacy BIOS.",
            "The BMC of node-02 now boots BIOS setup, next boot only, UEFI.",
            "The BMC of node-02 holds no boot device override.",
        ])
    );
}

#[test]
fn a_refused_console_call_reports_the_refusal() {
    // Catches: a refusal of the coordinator (no console access, a power action the operator may
    // not send) shown as a success or swallowed.
    let mut ui = start();
    ui.refuse(
        &m("Console/Power"),
        403,
        "PERMISSION_DENIED",
        "power_not_permitted",
    );
    ui.refuse(
        &m("Console/SetBootDevice"),
        403,
        "PERMISSION_DENIED",
        "no_console_access",
    );
    let out = ui.run(&page_driver(
        "host",
        HOST,
        "[true, true]",
        &format!(
            "{REASON}await click(tree, \"power-off\");\nawait click(tree, \"set-boot-device\");"
        ),
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "PERMISSION_DENIED power_not_permitted",
            "PERMISSION_DENIED no_console_access"
        ])
    );
    assert_eq!(console_calls(&ui).len(), 2);
}

#[test]
fn drawing_the_host_page_makes_no_console_call() {
    // Catches: the tab asking the BMC for the power state when the page is drawn (a call per
    // visit, and a BMC session for an operator who only looks).
    let mut ui = start();
    let out = ui.run(&page_driver("host", HOST, "[]", ""));
    assert_eq!(bodies(&ui), drawn());
    let text = out["text"].as_str().expect("text");
    for label in ["Power and boot", "Soft shutdown", "Set boot device"] {
        assert!(text.contains(label), "{label}: {text}");
    }
}
