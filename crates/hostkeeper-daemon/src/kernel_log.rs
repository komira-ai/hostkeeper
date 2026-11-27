//! The kernel's log lines turned into hardware records (docs/design/host-health-alerts.md section
//! 3.1, the source `journal`): pure functions over lines, with no file, program or clock of
//! their own. [`crate::kernel_sources`] reads the lines (`/dev/kmsg` for this boot, the journal
//! for the boot before) and hands them here.
//!
//! **Reading.** [`parse_kmsg`] reads `/dev/kmsg` records (`<prio>,<seq>,<usec>,<flags>;<text>`,
//! its `\xNN` escapes undone, its continuation lines passed over); [`parse_journal`] reads the
//! lines `journalctl --output=json` prints, one object a line, a line that does not parse (the
//! last one of an output cut at its cap) passed over, in the order the kernel logged them.
//!
//! **Classifying** ([`classify`]). Each line is judged by its own words, never by its syslog
//! priority:
//!
//! - machine checks: `mce: [Hardware Error]: CPU <n>: Machine Check...: Bank <b>: <status>` is
//!   `cpu`, critical `cpu_machine_check` when the status's UC bit (61) is set, else `info`
//!   `cpu_corrected`; `mce: [Hardware Error]: Machine check events logged` is `info`
//!   `cpu_corrected`; `mce: Uncorrected hardware memory error` is `memory`, critical
//!   `memory_uncorrectable`; `Kernel panic - not syncing:` naming a machine check is critical
//!   `cpu_machine_check`. The other `mce:` lines (TSC, ADDR, MISC, PROCESSOR) are the same event's
//!   details and give nothing.
//! - EDAC: `EDAC ...: <n> UE ... on <label> (...)` is `memory` by the DIMM label, critical
//!   `memory_uncorrectable`; `<n> CE` is `info` `memory_corrected`, standing for `n` errors.
//! - AER: `AER: <kind> error received: <device>`, and the newer kernels' `AER: <kind> error
//!   message received from <device>`, is `pcie` by the device (else the reporting port's
//!   address): `Corrected` or `Correctable` `info` `pcie_corrected`, `Uncorrected (Fatal)` or
//!   `Uncorrectable (Fatal)` critical `pcie_fatal`, `(Non-Fatal)` critical `pcie_uncorrected`.
//! - APEI's `[Hardware Error]:` lines, `{<n>}[Hardware Error]: ...` from a GHES source, are read
//!   as one record per error: it begins at `Hardware error from APEI` or at an `event severity:`
//!   line, takes its component from `section_type:` (memory, PCIe, processor, else `board`) and
//!   its subject from `DIMM location:` or `device_id:`. `fatal` and `recoverable` are critical,
//!   `corrected` and anything else `info`; an error with no `event severity:` line gives nothing.
//!   The lines with no `{<n>}` are BERT's, the errors the firmware kept of the boot before: their
//!   records are `of_previous_boot`. But AMD's machine check decoder also prints lines with no
//!   `{<n>}`: `[Hardware Error]: Corrected error, ...` is `cpu`, `info` `cpu_corrected`, and
//!   `[Hardware Error]: Uncorrected, ...` critical `cpu_machine_check`; its other lines give
//!   nothing.
//! - I/O errors: `I/O error, dev <disk>,` is `disk`, warning `io_error`, for a physical disk only
//!   (no `loop`, `ram`, `zram`, `dm-`, `md`, `nbd` or `sr` device).
//! - Every other line gives nothing.
//!
//! **Coalescing.** The records of one call that agree in code, subject, severity and boot are one
//! record: the first line's id and time, the last line's text as `raw`, and a message that says
//! how many times (`..., 37 times`). So a storm of corrected errors or of I/O errors of one disk is
//! one record per read, never hundreds.
//!
//! **Ids.** A record's `event_id` is `journal:<boot>:<usec>:<digest>` of its first line: the boot
//! ([`boot_key`]), the kernel's microsecond stamp and 8 hex digits of the SHA-256 of the line's
//! text, so the same line read from `/dev/kmsg` in its boot and from the journal after it is the
//! same record.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The source token of every record of the kernel's log.
pub const SOURCE: &str = "journal";

/// The bit of a machine check status that says the error was not corrected.
pub const MCE_STATUS_UC: u64 = 1 << 61;

/// The block device names that are not physical disks: their I/O errors give no record.
pub const NOT_PHYSICAL: [&str; 7] = ["loop", "ram", "zram", "dm-", "md", "nbd", "sr"];

/// One line of the kernel's log, from `/dev/kmsg` or from the journal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KernelLine {
    /// The boot it was logged in, as the source names it (with or without dashes).
    pub boot_id: String,
    /// When it was logged, microseconds since that boot began: the kernel's own stamp.
    pub usec: u64,
    /// When it was logged, Unix seconds; 0 when not known.
    pub time_unix: u64,
    /// The text, as the kernel printed it.
    pub text: String,
}

/// One record of `/dev/kmsg`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KmsgRecord {
    /// The kernel's sequence number of the record, from 0 at each boot.
    pub seq: u64,
    /// The kernel's stamp, microseconds since the boot began.
    pub usec: u64,
    /// The text, its escapes undone.
    pub text: String,
}

/// The records of `bytes`, one or more whole `/dev/kmsg` records: each line
/// `<prio>,<seq>,<usec>,<flags>[,...];<text>`, whose continuation lines (a leading space) are
/// passed over, as is a line that does not parse.
#[must_use]
pub fn parse_kmsg(bytes: &[u8]) -> Vec<KmsgRecord> {
    let text = String::from_utf8_lossy(bytes);
    text.split('\n')
        .filter(|line| !line.is_empty() && !line.starts_with(' '))
        .filter_map(parse_kmsg_line)
        .collect()
}

fn parse_kmsg_line(line: &str) -> Option<KmsgRecord> {
    let (prefix, text) = line.split_once(';')?;
    let mut fields = prefix.split(',');
    let _priority: u64 = fields.next()?.parse().ok()?;
    let seq = fields.next()?.parse().ok()?;
    let usec = fields.next()?.parse().ok()?;
    Some(KmsgRecord {
        seq,
        usec,
        text: unescape(text),
    })
}

/// `text` with the kernel's `\xNN` escapes undone; an escape that is not two hex digits is kept.
#[must_use]
pub fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = bytes
            .get(i..i + 4)
            .filter(|part| part[0] == b'\\' && part[1] == b'x')
            .filter(|part| part[2].is_ascii_hexdigit() && part[3].is_ascii_hexdigit())
            .and_then(|part| std::str::from_utf8(&part[2..]).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The lines of `journalctl --output=json` output: each object's `MESSAGE` (a string, or an
/// array of bytes), `_BOOT_ID`, `_SOURCE_MONOTONIC_TIMESTAMP` (else `__MONOTONIC_TIMESTAMP`) and
/// `__REALTIME_TIMESTAMP`, sorted by the kernel's stamp. An object with no message or no stamp,
/// and a line that is not one object, are passed over.
#[must_use]
pub fn parse_journal(stdout: &[u8]) -> Vec<KernelLine> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines: Vec<KernelLine> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|entry| journal_line(&entry))
        .collect();
    lines.sort_by_key(|line| line.usec);
    lines
}

fn journal_line(entry: &Value) -> Option<KernelLine> {
    let text = match entry.get("MESSAGE")? {
        Value::String(text) => text.clone(),
        Value::Array(bytes) => {
            let bytes: Vec<u8> = bytes
                .iter()
                .filter_map(Value::as_u64)
                .filter_map(|b| u8::try_from(b).ok())
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => return None,
    };
    let number = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .and_then(|digits| digits.parse::<u64>().ok())
    };
    let usec = number("_SOURCE_MONOTONIC_TIMESTAMP").or_else(|| number("__MONOTONIC_TIMESTAMP"))?;
    Some(KernelLine {
        boot_id: entry
            .get("_BOOT_ID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        usec,
        time_unix: number("__REALTIME_TIMESTAMP").map_or(0, |usec| usec / 1_000_000),
        text,
    })
}

/// The boot `boot_id` names, as records and marks key it: its hex digits, lowercase, with no
/// dashes, the first 16; `unknown` when it has none.
#[must_use]
pub fn boot_key(boot_id: &str) -> String {
    let key: String = boot_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .take(16)
        .collect();
    if key.is_empty() {
        "unknown".to_owned()
    } else {
        key
    }
}

/// Whether `a` and `b` name the same boot, with or without dashes, in either case; an empty one
/// names none.
#[must_use]
pub fn same_boot(a: &str, b: &str) -> bool {
    let digits = |id: &str| -> String {
        id.chars()
            .filter(|c| *c != '-')
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    !a.is_empty() && digits(a) == digits(b)
}

/// 8 hex digits of the SHA-256 of `text`.
#[must_use]
pub fn digest8(text: &str) -> String {
    hex::encode(&Sha256::digest(text.as_bytes())[..4])
}

/// The id of a record whose first line is `line`.
#[must_use]
pub fn line_id(line: &KernelLine) -> String {
    format!(
        "{SOURCE}:{}:{}:{}",
        boot_key(&line.boot_id),
        line.usec,
        digest8(&line.text)
    )
}

/// What one line, or one APEI error of several lines, says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Finding {
    component: pb::HardwareComponent,
    subject: String,
    severity: pb::HardwareSeverity,
    code: &'static str,
    /// The message of one occurrence.
    message: String,
    /// How many errors it stands for.
    count: u64,
    of_previous_boot: bool,
    raw: String,
    /// The index of its first line.
    first: usize,
}

impl Finding {
    fn new(
        component: pb::HardwareComponent,
        subject: &str,
        severity: pb::HardwareSeverity,
        code: &'static str,
        message: String,
    ) -> Finding {
        Finding {
            component,
            subject: subject.to_owned(),
            severity,
            code,
            message,
            count: 1,
            of_previous_boot: false,
            raw: String::new(),
            first: 0,
        }
    }
}

/// The records of `lines`, in the order of their first lines (the module's doc). Every record is
/// `of_previous_boot` when `of_previous_boot` is set (lines of the boot before, from the journal),
/// and so are BERT's whatever it is.
#[must_use]
pub fn classify(lines: &[KernelLine], of_previous_boot: bool) -> Vec<pb::HardwareEvent> {
    let mut findings = Vec::new();
    let mut groups = Groups::default();
    for (index, line) in lines.iter().enumerate() {
        let text = line.text.trim_end();
        if let Some((key, body)) = apei_line(text) {
            if let Some(mut finding) = amd_decoded(key, body) {
                finding.first = index;
                finding.raw = text.to_owned();
                findings.push(finding);
            } else if let Some(done) = groups.line(key, body, text, index) {
                findings.push(done);
            }
            continue;
        }
        if let Some(mut finding) = classify_line(text) {
            finding.first = index;
            finding.raw = text.to_owned();
            findings.push(finding);
        }
    }
    findings.extend(groups.finish());
    findings.sort_by_key(|f| f.first);
    for finding in &mut findings {
        finding.of_previous_boot |= of_previous_boot;
    }
    coalesce(findings)
        .into_iter()
        .map(|f| event(&lines[f.first], f))
        .collect()
}

/// The findings that agree in code, subject, severity and boot, folded into the first of them.
fn coalesce(findings: Vec<Finding>) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let mut at: BTreeMap<(&'static str, String, i32, bool), usize> = BTreeMap::new();
    for finding in findings {
        let key = (
            finding.code,
            finding.subject.clone(),
            i32::from(finding.severity),
            finding.of_previous_boot,
        );
        match at.get(&key) {
            Some(&index) => {
                let kept = &mut out[index];
                kept.count = kept.count.saturating_add(finding.count);
                kept.raw = finding.raw;
            }
            None => {
                at.insert(key, out.len());
                out.push(finding);
            }
        }
    }
    out
}

fn event(first: &KernelLine, finding: Finding) -> pb::HardwareEvent {
    let message = if finding.count > 1 {
        format!("{}, {} times", finding.message, finding.count)
    } else {
        finding.message
    };
    pb::HardwareEvent {
        event_id: line_id(first),
        time_unix: first.time_unix,
        of_previous_boot: finding.of_previous_boot,
        component: finding.component.into(),
        subject: finding.subject,
        severity: finding.severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: SOURCE.to_owned(),
        code: finding.code.to_owned(),
        message,
        raw: finding.raw,
        about: String::new(),
    }
}

/// What one line says on its own: a machine check, EDAC, AER or an I/O error; none for any
/// other line.
fn classify_line(text: &str) -> Option<Finding> {
    machine_check(text)
        .or_else(|| edac(text))
        .or_else(|| aer(text))
        .or_else(|| io_error(text))
}

fn machine_check(text: &str) -> Option<Finding> {
    use pb::HardwareComponent::{Cpu, Memory};
    use pb::HardwareSeverity::{Critical, Info};
    if let Some(body) = text.strip_prefix("mce: [Hardware Error]: ") {
        if body.starts_with("Machine check events logged") {
            return Some(Finding::new(
                Cpu,
                "CPU",
                Info,
                "cpu_corrected",
                "Corrected machine check events logged".to_owned(),
            ));
        }
        let rest = body.strip_prefix("CPU ")?;
        if !rest.contains("Machine Check") {
            return None;
        }
        let cpu = rest.split(':').next().unwrap_or_default().trim();
        let (bank, status) = bank_status(rest);
        let uncorrected = match status {
            Some(status) => status & MCE_STATUS_UC != 0,
            None => rest.contains("Exception"),
        };
        let subject = format!("CPU {cpu}");
        let bank = bank.map(|b| format!(", bank {b}")).unwrap_or_default();
        return Some(if uncorrected {
            Finding::new(
                Cpu,
                &subject,
                Critical,
                "cpu_machine_check",
                format!("Uncorrected machine check on {subject}{bank}"),
            )
        } else {
            Finding::new(
                Cpu,
                &subject,
                Info,
                "cpu_corrected",
                format!("Corrected machine check on {subject}{bank}"),
            )
        });
    }
    if text.starts_with("mce: Uncorrected hardware memory error") {
        return Some(Finding::new(
            Memory,
            "memory",
            Critical,
            "memory_uncorrectable",
            "Uncorrected hardware memory error (machine check)".to_owned(),
        ));
    }
    if text.starts_with("Kernel panic - not syncing:") && text.contains("achine check") {
        return Some(Finding::new(
            Cpu,
            "CPU",
            Critical,
            "cpu_machine_check",
            "Kernel panic on a fatal machine check".to_owned(),
        ));
    }
    None
}

/// The bank number and the status of `... Bank <b>: <hex>`, each none when it does not parse.
fn bank_status(text: &str) -> (Option<u64>, Option<u64>) {
    let Some((_, after)) = text.split_once("Bank ") else {
        return (None, None);
    };
    let (bank, rest) = after.split_once(':').unwrap_or((after, ""));
    let status = rest
        .split_whitespace()
        .next()
        .map(|hex| hex.trim_start_matches("0x"))
        .and_then(|hex| u64::from_str_radix(hex, 16).ok());
    (bank.trim().parse().ok(), status)
}

fn edac(text: &str) -> Option<Finding> {
    use pb::HardwareComponent::Memory;
    use pb::HardwareSeverity::{Critical, Info};
    if !text.starts_with("EDAC ") {
        return None;
    }
    let (kind, at) = [" UE ", " CE "]
        .iter()
        .filter_map(|kind| text.find(kind).map(|at| (*kind, at)))
        .min_by_key(|(_, at)| *at)?;
    let count = text[..at]
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse::<u64>().ok())?
        .max(1);
    let after = &text[at + kind.len()..];
    let label = after
        .split_once(" on ")
        .map(|(_, label)| label.split(" (").next().unwrap_or_default().trim())
        .filter(|label| !label.is_empty())
        .unwrap_or("memory");
    let mut finding = if kind == " UE " {
        Finding::new(
            Memory,
            label,
            Critical,
            "memory_uncorrectable",
            format!("Uncorrectable memory error on {label} (EDAC)"),
        )
    } else {
        Finding::new(
            Memory,
            label,
            Info,
            "memory_corrected",
            format!("Corrected memory error on {label} (EDAC)"),
        )
    };
    finding.count = count;
    Some(finding)
}

fn aer(text: &str) -> Option<Finding> {
    use pb::HardwareComponent::Pcie;
    use pb::HardwareSeverity::{Critical, Info};
    let (before, after) = text.split_once("AER: ")?;
    let (kind, received) = after
        .split_once(" error message received")
        .or_else(|| after.split_once(" error received"))?;
    let named = received
        .split_whitespace()
        .map(|word| word.trim_matches(':'))
        .find(|device| is_pci_address(device));
    let port = before
        .split_whitespace()
        .next_back()
        .map(|device| device.trim_end_matches(':'))
        .filter(|device| is_pci_address(device));
    let subject = named.or(port).unwrap_or("pcie");
    if kind.contains("Uncorrect") {
        Some(if kind.contains("Non-Fatal") {
            Finding::new(
                Pcie,
                subject,
                Critical,
                "pcie_uncorrected",
                format!("Uncorrected PCIe error on {subject} (AER)"),
            )
        } else {
            Finding::new(
                Pcie,
                subject,
                Critical,
                "pcie_fatal",
                format!("Fatal PCIe error on {subject} (AER)"),
            )
        })
    } else if kind.contains("Correct") {
        Some(Finding::new(
            Pcie,
            subject,
            Info,
            "pcie_corrected",
            format!("Corrected PCIe error on {subject} (AER)"),
        ))
    } else {
        None
    }
}

/// Whether `text` is a PCI address of the form `0000:3b:00.0`.
#[must_use]
pub fn is_pci_address(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 12
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b':',
            10 => *b == b'.',
            _ => b.is_ascii_hexdigit(),
        })
}

fn io_error(text: &str) -> Option<Finding> {
    let (_, after) = text.split_once("I/O error, dev ")?;
    let disk = after
        .split(|c: char| c.is_whitespace() | (c == ','))
        .next()
        .filter(|disk| is_physical(disk))?;
    Some(Finding::new(
        pb::HardwareComponent::Disk,
        disk,
        pb::HardwareSeverity::Warning,
        "io_error",
        format!("I/O error on disk {disk}"),
    ))
}

/// Whether the block device `name` may be a physical disk: not empty and none of
/// [`NOT_PHYSICAL`].
#[must_use]
pub fn is_physical(name: &str) -> bool {
    !name.is_empty() && !NOT_PHYSICAL.iter().any(|prefix| name.starts_with(prefix))
}

/// An APEI line's source key (the `<n>` of `{<n>}`, empty for BERT's) and its body after
/// `[Hardware Error]: `; none for any other line, and for the machine check's own
/// `mce: [Hardware Error]:` lines.
fn apei_line(text: &str) -> Option<(&str, &str)> {
    const MARK: &str = "[Hardware Error]: ";
    let at = text.find(MARK)?;
    let prefix = &text[..at];
    if prefix.contains("mce:") {
        return None;
    }
    let key = prefix
        .rfind('{')
        .and_then(|open| prefix[open + 1..].strip_suffix('}'))
        .unwrap_or("");
    Some((key, text[at + MARK.len()..].trim()))
}

/// One APEI error being read.
#[derive(Default)]
struct Group {
    first: usize,
    severity: Option<String>,
    section: Option<String>,
    location: Option<String>,
    device: Option<String>,
    raws: Vec<String>,
}

/// The APEI errors open, one per source key.
#[derive(Default)]
struct Groups {
    open: BTreeMap<String, Group>,
}

impl Groups {
    /// Adds the line `text` (body `body`, of source `key`, at `index`) to its error; the error
    /// before of the same source when this line begins another.
    fn line(&mut self, key: &str, body: &str, text: &str, index: usize) -> Option<Finding> {
        let begins = body.starts_with("Hardware error from APEI")
            || (body.starts_with("event severity:")
                && self
                    .open
                    .get(key)
                    .is_some_and(|group| group.severity.is_some()));
        let done = if begins {
            self.open
                .remove(key)
                .and_then(|group| apei_finding(key, group))
        } else {
            None
        };
        let group = self.open.entry(key.to_owned()).or_insert_with(|| Group {
            first: index,
            ..Group::default()
        });
        let value = |name: &str| body.strip_prefix(name).map(|v| v.trim().to_owned());
        if let Some(severity) = value("event severity:") {
            group.severity = Some(severity.to_ascii_lowercase());
            group.raws.push(text.to_owned());
        } else if let Some(section) = value("section_type:") {
            group.section.get_or_insert(section);
            group.raws.push(text.to_owned());
        } else if let Some(location) = value("DIMM location:") {
            group.location.get_or_insert(location);
            group.raws.push(text.to_owned());
        } else if let Some(device) = value("device_id:").filter(|d| is_pci_address(d)) {
            group.device.get_or_insert(device);
            group.raws.push(text.to_owned());
        }
        done
    }

    /// The errors still open, at the end of the lines.
    fn finish(self) -> Vec<Finding> {
        self.open
            .into_iter()
            .filter_map(|(key, group)| apei_finding(&key, group))
            .collect()
    }
}

/// The record of one APEI error; none when it has no `event severity:` line, which every error
/// of the firmware's error format has.
fn apei_finding(key: &str, group: Group) -> Option<Finding> {
    use pb::HardwareComponent::{Board, Cpu, Memory, Pcie};
    use pb::HardwareSeverity::{Critical, Info};
    let severity = group.severity?;
    let critical = severity == "fatal" || severity == "recoverable";
    let section = group.section.unwrap_or_default();
    let lower = section.to_ascii_lowercase();
    let via = if key.is_empty() { "BERT" } else { "APEI" };
    let (component, subject) = if lower.contains("memory") {
        (
            Memory,
            group.location.unwrap_or_else(|| "memory".to_owned()),
        )
    } else if lower.contains("pcie") {
        (Pcie, group.device.unwrap_or_else(|| "pcie".to_owned()))
    } else if lower.contains("processor") {
        (Cpu, "CPU".to_owned())
    } else {
        (Board, "board".to_owned())
    };
    let (code, what) = match (component, critical, severity.as_str()) {
        (Memory, true, _) => ("memory_uncorrectable", "Uncorrectable memory error"),
        (Memory, false, _) => ("memory_corrected", "Corrected memory error"),
        (Pcie, true, "fatal") => ("pcie_fatal", "Fatal PCIe error"),
        (Pcie, true, _) => ("pcie_uncorrected", "Uncorrected PCIe error"),
        (Pcie, false, _) => ("pcie_corrected", "Corrected PCIe error"),
        (Cpu, true, _) => ("cpu_machine_check", "Uncorrected processor error"),
        (Cpu, false, _) => ("cpu_corrected", "Corrected processor error"),
        (_, true, _) => ("hardware_error", "Uncorrected hardware error"),
        (_, false, _) => ("hardware_corrected", "Corrected hardware error"),
    };
    let message = match component {
        Board if !section.is_empty() => format!("{what} ({section}) ({via})"),
        Board => format!("{what} ({via})"),
        _ => format!("{what} on {subject} ({via})"),
    };
    let mut finding = Finding::new(
        component,
        &subject,
        if critical { Critical } else { Info },
        code,
        message,
    );
    finding.first = group.first;
    finding.of_previous_boot = key.is_empty();
    finding.raw = group.raws.join(" | ");
    Some(finding)
}

/// What a line of AMD's machine check decoder says (`[Hardware Error]: Corrected error, no action
/// required.`, `[Hardware Error]: Uncorrected, ...`), which has no `{<n>}` and is not the
/// firmware's error format; none for any other line.
fn amd_decoded(key: &str, body: &str) -> Option<Finding> {
    use pb::HardwareComponent::Cpu;
    use pb::HardwareSeverity::{Critical, Info};
    if !key.is_empty() {
        return None;
    }
    if body.starts_with("Corrected error") {
        Some(Finding::new(
            Cpu,
            "CPU",
            Info,
            "cpu_corrected",
            "Corrected machine check error (MCE decoder)".to_owned(),
        ))
    } else if body.starts_with("Uncorrected") {
        Some(Finding::new(
            Cpu,
            "CPU",
            Critical,
            "cpu_machine_check",
            "Uncorrected machine check error (MCE decoder)".to_owned(),
        ))
    } else {
        None
    }
}

#[cfg(test)]
#[path = "kernel_log_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "kernel_log_aer_tests.rs"]
mod aer_tests;
