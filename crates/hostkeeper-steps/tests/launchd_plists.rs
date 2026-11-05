//! The launchd plist of the updater's fallback timer job. The daemon's own plist is
//! `hostkeeper_launchd::plist::daemon_plist`, tested with its golden in that crate.

use std::path::Path;

use hostkeeper_steps::selfupdate::{Layout, launchd_fallback_job};

const HEAD: [&str; 2] = [
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
    "<plist version=\"1.0\">",
];

/// The lines of a plist: the fixed head, `body` inside the outer dict, the closing tags and a
/// final newline.
fn plist(body: &[&str]) -> String {
    let mut lines: Vec<&str> = HEAD.to_vec();
    lines.push("<dict>");
    lines.extend_from_slice(body);
    lines.push("</dict>");
    lines.push("</plist>");
    lines.join("\n") + "\n"
}

/// Catches a plist that differs from run to run or lacks a part of the contract: the fallback
/// job runs the previous binary's verb on an interval, not at load, under the `.fallback` label.
#[test]
fn the_fallback_job_is_deterministic_and_carries_the_fallback_label() {
    let layout = Layout::new(Path::new("/opt/example/updater"));
    let job = launchd_fallback_job(&layout, "com.example.hostkeeper", 60);
    assert_eq!(
        job,
        plist(&[
            "  <key>Label</key>",
            "  <string>com.example.hostkeeper.fallback</string>",
            "  <key>ProgramArguments</key>",
            "  <array>",
            "    <string>/opt/example/updater/previous</string>",
            "    <string>self-update-fallback</string>",
            "    <string>--dir</string>",
            "    <string>/opt/example/updater</string>",
            "  </array>",
            "  <key>RunAtLoad</key>",
            "  <false/>",
            "  <key>StartInterval</key>",
            "  <integer>60</integer>",
        ])
    );
    assert_eq!(
        job,
        launchd_fallback_job(&layout, "com.example.hostkeeper", 60)
    );
}

/// Catches a plist that breaks out of its XML when a directory or a label holds `&`, `<`, `>`,
/// `"` or `'`: each becomes its entity, in the label and in every path, and no raw character
/// is left inside a `<string>` element.
#[test]
fn a_path_or_label_with_xml_metacharacters_is_escaped() {
    let layout = Layout::new(Path::new("/opt/a&b<c>d\"e'f"));
    let dir = "/opt/a&amp;b&lt;c&gt;d&quot;e&apos;f";
    let job = launchd_fallback_job(&layout, "x&y", 7);
    assert_eq!(
        job,
        plist(&[
            "  <key>Label</key>",
            "  <string>x&amp;y.fallback</string>",
            "  <key>ProgramArguments</key>",
            "  <array>",
            &format!("    <string>{dir}/previous</string>"),
            "    <string>self-update-fallback</string>",
            "    <string>--dir</string>",
            &format!("    <string>{dir}</string>"),
            "  </array>",
            "  <key>RunAtLoad</key>",
            "  <false/>",
            "  <key>StartInterval</key>",
            "  <integer>7</integer>",
        ])
    );
}
