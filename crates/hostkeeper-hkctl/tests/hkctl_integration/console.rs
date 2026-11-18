//! The `Console` calls, as the real script sends them: the path each posts to and the body curl
//! reads, an action or boot device the proto does not name refused before anything is sent, and
//! a call through the real gateway answered with the coordinator's refusal.

use crate::gateway;
use crate::rig::{Rig, Script, args};

/// `words` after the global flags of a call to the coordinator at `port`.
fn call(rig: &Rig, port: u16, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(port);
    all.extend(args(words));
    all
}

/// The target and the body of the only `curl` call the rig recorded.
fn posted(rig: &Rig) -> (String, String) {
    let curls = rig.curls();
    assert_eq!(curls.len(), 1, "{curls:#?}");
    let url = curls[0]
        .argv
        .iter()
        .skip_while(|a| *a != "--url")
        .nth(1)
        .expect("a url")
        .clone();
    let body = curls[0]
        .read_after("--data-binary", 0)
        .expect("the body is read from a pipe");
    (url, body)
}

/// Proves: `console open`, `power`, `set-boot-device`, `list` and `close` each post to their own
/// method of `Console` a body that holds exactly their flags, under the fields' JSON names, the
/// action and the boot device by their enum names and `--persistent`/`--efi` as booleans, and
/// each exits 0 with the coordinator's answer on stdout.
///
/// Catches: a console command dispatched to another method, a flag that does not reach its
/// field (the host id, the reason), an enum value sent by its flag spelling, and a boolean sent
/// as a string.
#[test]
fn each_console_command_posts_its_flags_to_its_method() {
    for (words, method, body) in [
        (
            &[
                "console",
                "open",
                "--host-id",
                "node-02",
                "--reason",
                "boot order",
            ][..],
            "Console/Open",
            r#"{"hostId":"node-02","reason":"boot order"}"#,
        ),
        (
            &[
                "console",
                "power",
                "--host-id",
                "node-02",
                "--action",
                "cycle",
                "--reason",
                "hung",
            ][..],
            "Console/Power",
            r#"{"hostId":"node-02","action":"POWER_ACTION_CYCLE","reason":"hung"}"#,
        ),
        (
            &[
                "console",
                "set-boot-device",
                "--host-id",
                "node-02",
                "--device",
                "disk",
                "--persistent",
                "true",
                "--efi",
                "false",
                "--reason",
                "boot order",
            ][..],
            "Console/SetBootDevice",
            r#"{"hostId":"node-02","device":"BOOT_DEVICE_DISK","persistent":true,"efi":false,"reason":"boot order"}"#,
        ),
        (&["console", "list"][..], "Console/List", "{}"),
        (
            &["console", "close", "--host-id", "node-02"][..],
            "Console/Close",
            r#"{"hostId":"node-02"}"#,
        ),
    ] {
        let rig = Rig::new("console-post");
        rig.reply(200, "{\"sessions\":[]}");
        let ran = rig.run(Script::Real, &call(&rig, 1, words), b"", &[]);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, "{\"sessions\":[]}\n", ""),
            "{method}"
        );
        let (url, sent) = posted(&rig);
        assert_eq!(
            url,
            format!("https://127.0.0.1:1/hostkeeper.operator.v1.{method}")
        );
        assert_eq!(sent, body, "{method}");
    }
}

/// Proves: a power action or a boot device the proto does not name (`reboot`, `cdrom`), and the
/// unspecified zero, are usage errors that exit 2 with exactly the flag and its values, and nothing is
/// sent to the coordinator.
///
/// Catches: a power command sent with an action the coordinator would have to guess at, or a
/// typo reaching the BMC path as another command.
#[test]
fn an_action_or_device_the_proto_does_not_name_is_refused_before_anything_is_sent() {
    for (words, stderr) in [
        (
            &[
                "console",
                "power",
                "--host-id",
                "node-02",
                "--action",
                "reboot",
                "--reason",
                "x",
            ][..],
            "hkctl: --action takes one of: status, on, off, soft, cycle, reset\n",
        ),
        (
            &[
                "console",
                "power",
                "--host-id",
                "node-02",
                "--action",
                "unspecified",
                "--reason",
                "x",
            ][..],
            "hkctl: --action takes one of: status, on, off, soft, cycle, reset\n",
        ),
        (
            &[
                "console",
                "set-boot-device",
                "--host-id",
                "node-02",
                "--device",
                "cdrom",
                "--reason",
                "x",
            ][..],
            "hkctl: --device takes one of: pxe, disk, bios\n",
        ),
    ] {
        let rig = Rig::new("console-usage");
        rig.reply(200, "{}");
        let ran = rig.run(Script::Real, &call(&rig, 1, words), b"", &[]);
        assert_eq!(ran.code, 2, "{words:?}: {}", ran.stderr);
        assert_eq!(ran.stderr, stderr, "{words:?}");
        assert!(rig.curls().is_empty(), "{words:?}");
    }
}

/// Proves: `console open` through the real gateway reaches the coordinator's `Console/Open`
/// with the operator's bearer, and the coordinator's refusal (no console operators file is read,
/// so nobody has console access) ends `hkctl` with exit 3 and exactly the gateway's code and the
/// refusal's token on stderr, nothing on stdout.
///
/// Catches: a console command the real router does not serve (`UNIMPLEMENTED`), a refusal that
/// exits 0, and one whose token is lost.
#[test]
fn console_open_through_the_real_gateway_is_refused_no_console_access() {
    let rig = Rig::new("console-gateway");
    let gateway = gateway::start(&rig.path(""));
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            &[
                "console",
                "open",
                "--host-id",
                "node-02",
                "--reason",
                "boot order",
            ],
        ),
        b"",
        &[],
    );
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].target(), "/hostkeeper.operator.v1.Console/Open");
    assert_eq!(
        String::from_utf8_lossy(&seen[0].body),
        r#"{"hostId":"node-02","reason":"boot order"}"#
    );
    assert_eq!(seen[0].status, 403);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            3,
            "",
            "hkctl: the coordinator answered PERMISSION_DENIED\nrefused: no_console_access\n"
        )
    );
}
