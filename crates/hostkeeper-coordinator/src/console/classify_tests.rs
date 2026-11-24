//! The classes a probe finds in a serial console's bytes, from pieces of bytes as they came.

use hostkeeper_proto::hostkeeper::operator::v1::ConsoleProbeClass as Class;

use super::{MAX_EVIDENCE, MAX_LINE, Reading, Seen};

/// What a reading of `pieces`, one after another, saw.
fn seen(pieces: &[&[u8]]) -> Seen {
    let mut reading = Reading::new();
    for piece in pieces {
        reading.take(piece);
    }
    reading.finish()
}

/// The classes and evidence of what a reading of `pieces` saw.
fn found(pieces: &[&[u8]]) -> Vec<(Class, String)> {
    seen(pieces)
        .findings
        .into_iter()
        .map(|finding| (finding.class(), finding.evidence))
        .collect()
}

/// One expected finding.
fn one(class: Class, evidence: &str) -> (Class, String) {
    (class, evidence.to_owned())
}

/// Proves: no byte is `silent` with no evidence, and a reading counts the bytes it took.
///
/// Catches: a probe that calls a host that said nothing anything else.
#[test]
fn no_byte_is_silent() {
    let nothing = seen(&[]);
    assert_eq!(nothing.bytes, 0);
    assert!(!nothing.from_boot);
    assert_eq!(found(&[]), [one(Class::Silent, "")]);
    assert_eq!(seen(&[b"ab", b"c"]).bytes, 3);
}

/// Proves: a login prompt is found on its line without a line feed, on one that ends with
/// `login:` once trimmed, after a carriage return alone, and over pieces that cut it anywhere; the
/// lines before it are not a finding of their own; `Login incorrect` or a word that only holds
/// `login` is not a prompt.
///
/// Catches: a prompt missed because it has no line feed, or one claimed from a line that is not.
#[test]
fn a_login_prompt_is_a_line_that_ends_with_login() {
    let prompt = one(Class::LoginPrompt, "node-02 login:");
    assert_eq!(found(&[b"node-02 login: "]), std::slice::from_ref(&prompt));
    assert_eq!(
        found(&[b"node-02 login:\r\n"]),
        std::slice::from_ref(&prompt)
    );
    assert_eq!(
        found(&[b"\rnode-02 login:   "]),
        std::slice::from_ref(&prompt)
    );
    assert_eq!(
        found(&[b"Ubuntu 24.04 LTS\r\n\r\nnode", b"-02 lo", b"gin: "]),
        [prompt]
    );
    assert_eq!(
        found(&[b"Login incorrect\r\nlogins: none\r\n"]),
        [one(Class::OtherOutput, "Login incorrect")]
    );
}

/// Proves: a kernel line is `[`, spaces, digits, `.`, digits and `]` at its start; its first is
/// the evidence; lines like it but not it (a word in brackets, a stamp with no fraction, letters
/// in the seconds, no closing bracket, `[  OK  ]`) are other text; a kernel line whose seconds are
/// not all zeros is no boot read whole, so the text before it is not claimed as the firmware's.
///
/// Catches: systemd's or a shell's lines taken for the kernel's, and firmware claimed from a read
/// that began in the middle of a run.
#[test]
fn a_kernel_line_is_its_stamp_and_text_before_a_later_one_is_not_firmware() {
    let line = b"Press <DEL> to run setup\r\n[ 4211.000001] eth0: link up\r\n";
    assert_eq!(
        found(&[line]),
        [one(Class::KernelOutput, "[ 4211.000001] eth0: link up")]
    );
    assert!(!seen(&[line]).from_boot);
    for not_kernel in [
        "[  OK  ] Started ssh.service",
        "[5] item",
        "[12.] item",
        "[.5] item",
        "[1.x] item",
        "[1x.5] item",
        "[ 1.5 item",
        "[] item",
    ] {
        assert_eq!(
            found(&[not_kernel.as_bytes()]),
            [one(Class::OtherOutput, not_kernel)],
            "{not_kernel}"
        );
    }
}

/// Proves: with the kernel's first line (its seconds all zeros), the read is `from_boot`; the
/// first line of text before it is `firmware_output`, found before the kernel; with no text before
/// it there is no firmware finding (BIOS redirection off) though the read is still from the boot;
/// text before an earlier kernel line (a reboot read from the shutdown on) is not the firmware's,
/// but the text between the shutdown's last kernel line and the new first one is; a second first
/// line is not looked at again.
///
/// Catches: a probe that misses the firmware in a boot read whole, or calls the shutdown's own
/// lines the firmware's.
#[test]
fn the_firmware_is_the_text_before_the_kernels_first_line() {
    let boot = seen(&[b"\x1b[2J\x1b[1;1HPress <DEL> to run setup\r\n\x1b[0m\r\n[    0.000000] Linux version 6.8.0\r\n"]);
    assert!(boot.from_boot);
    let classes: Vec<_> = boot
        .findings
        .iter()
        .map(|finding| (finding.class(), finding.evidence.as_str()))
        .collect();
    assert_eq!(
        classes,
        [
            (Class::FirmwareOutput, "Press <DEL> to run setup"),
            (Class::KernelOutput, "[    0.000000] Linux version 6.8.0"),
        ]
    );

    let redirection_off = seen(&[b"\x1b[2J\r\n[    0.000000] Linux version 6.8.0\r\n"]);
    assert!(redirection_off.from_boot);
    assert_eq!(
        found(&[b"\x1b[2J\r\n[    0.000000] Linux version 6.8.0\r\n"]),
        [one(
            Class::KernelOutput,
            "[    0.000000] Linux version 6.8.0"
        )]
    );

    let reboot: &[&[u8]] = &[
        b"Stopping services\r\n[ 4211.000001] reboot: Restarting system\r\n",
        b"Intel(R) Boot Agent GE v1.5.88\r\n",
        b"[    0.000000] Linux version 6.8.0\r\nnode-02 login: ",
        b"\r\n[    0.000000] again\r\nlater text\r\n",
    ];
    assert_eq!(
        found(reboot),
        [
            one(
                Class::KernelOutput,
                "[ 4211.000001] reboot: Restarting system"
            ),
            one(Class::FirmwareOutput, "Intel(R) Boot Agent GE v1.5.88"),
            one(Class::LoginPrompt, "node-02 login:"),
        ]
    );
}

/// Proves: bytes that are none of the classes are `other_output` with their first line of text;
/// bytes with no text at all (NULs, control characters, escape sequences, blank lines) are
/// `other_output` with no evidence, not `silent`; firmware text with no kernel line is not claimed
/// as the firmware's.
///
/// Catches: a host at 9600 baud (NUL bytes on the console) called silent, and firmware claimed
/// from a read that may not have begun at the boot.
#[test]
fn bytes_of_no_class_are_other_output() {
    assert_eq!(
        found(&[b"root@node-02:~# "]),
        [one(Class::OtherOutput, "root@node-02:~#")]
    );
    assert_eq!(
        found(&[b"\x00\x00\x07\x7f\x1b[0m\x1b7\r\n\r\n"]),
        [one(Class::OtherOutput, "")]
    );
    assert_eq!(
        found(&[b"Press <DEL> to run setup\r\n"]),
        [one(Class::OtherOutput, "Press <DEL> to run setup")]
    );
}

/// Proves: escape sequences are taken out across pieces (a sequence cut between two reads, an
/// escape that starts another before the first ended, a two-byte escape), a tab is a space, a
/// byte that is not UTF-8 is a replacement character, a line keeps only its first 1024 bytes, and
/// evidence is cut to 160 bytes on a character's boundary.
///
/// Catches: a prompt hidden by the colour sequences a getty or a firmware sends, and evidence
/// that is a whole screen or cut inside a character.
#[test]
fn escapes_are_taken_out_and_lines_are_bounded() {
    assert_eq!(
        found(&[b"\x1b[1;3", b"2mnode-02\x1b", b"[0m\tlogin:"]),
        [one(Class::LoginPrompt, "node-02 login:")]
    );
    assert_eq!(
        found(&[b"\x1b\x1b[31mred\x1bcfull\xff"]),
        [one(Class::OtherOutput, "redfull\u{fffd}")]
    );
    let long = format!("{} login:", "x".repeat(MAX_LINE));
    let cut = found(&[long.as_bytes()]);
    assert_eq!(cut, [one(Class::OtherOutput, &"x".repeat(MAX_EVIDENCE))]);
    let wide = "é".repeat(MAX_EVIDENCE);
    let evidence = &found(&[wide.as_bytes()])[0].1;
    assert_eq!(evidence, &"é".repeat(MAX_EVIDENCE / 2));
    let odd = format!("a{wide}");
    let evidence = &found(&[odd.as_bytes()])[0].1;
    assert_eq!(evidence, &format!("a{}", "é".repeat(MAX_EVIDENCE / 2 - 1)));
}
