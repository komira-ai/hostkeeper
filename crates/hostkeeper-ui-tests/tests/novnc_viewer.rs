//! The Desktop page's viewer as the browser runs it: `ui/screen.js`'s `screenViewer` over the
//! vendored noVNC `RFB` itself (`ui/vendor/novnc/`, byte for byte as pinned), in the JavaScript
//! engine with a fake browser (`tests/support/novnc_browser.js`: elements, canvases with real
//! pixels, timers the test runs). The test plays the Mac's side of an RFB 3.8 session over an open
//! socket and reads what the viewer answers and what it draws. Nothing waits on a clock.

mod support;

use serde_json::{Value, json};
use support::Ui;

/// The fake browser noVNC runs in, imported ahead of the library.
const BROWSER: &str = include_str!("support/novnc_browser.js");

/// Every module of the vendored noVNC.
fn novnc_modules() -> Vec<&'static str> {
    hostkeeper_ui_tests::UI_FILES
        .iter()
        .filter(|f| f.path.starts_with("vendor/novnc/") && f.is_script())
        .map(|f| f.path)
        .collect()
}

/// Runs `driver` in an engine with the app, noVNC and the fake browser, on a thread of its own
/// with a stack deep enough for the engine to parse noVNC's largest modules.
fn run_with_novnc(driver: &'static str) -> Value {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let mut ui = Ui::start_with(&novnc_modules(), &[("novnc_browser.js", BROWSER)]);
            ui.run(driver)
        })
        .expect("a thread for the engine")
        .join()
        .expect("the engine's thread ends")
}

/// The Mac's side of an RFB 3.8 session with no authentication, a 4 by 2 screen and one Raw
/// rectangle of eight colours, played over an open socket that `screenViewer(RFB)` was given.
const SESSION: &str = r#"
import { runTimers } from "../test/novnc_browser.js";
import RFB from "./vendor/novnc/core/rfb.js";
import { screenViewer } from "./screen.js";
export async function main() {
  let step = "start";
  try {
    return await session((at) => { step = at; });
  } catch (error) {
    return { failed: `${step}: ${error}`, stack: String(error && error.stack) };
  }
}
async function session(at) {
  const sent = [];
  const socket = {
    send(data) { sent.push(...Array.from(new Uint8Array(data.buffer || data, data.byteOffset || 0, data.byteLength))); },
    close() { socket.closes += 1; },
    closes: 0, binaryType: "blob", onerror: null, onmessage: null, onopen: null, onclose: null,
    protocol: "hostkeeper.console.v1", readyState: 1,
  };
  const told = [];
  at("building the viewer");
  const target = document.createElement("div");
  const viewer = screenViewer(RFB)(target, { socket }, {
    connect: () => told.push(["connect"]),
    credentials: (types) => told.push(["credentials", types]),
    securityFailure: (reason) => told.push(["securityFailure", reason]),
    disconnect: (clean) => told.push(["disconnect", clean]),
  });
  runTimers();
  const take = () => sent.splice(0, sent.length);
  const say = (bytes) => {
    socket.onmessage({ data: new Uint8Array(bytes).buffer });
    runTimers();
    return take();
  };
  const ascii = (text) => Array.from(text, (c) => c.charCodeAt(0));
  const u16 = (n) => [(n >> 8) & 255, n & 255];
  const u32 = (n) => [(n >>> 24) & 255, (n >> 16) & 255, (n >> 8) & 255, n & 255];
  const before = take();
  at("the version");
  const version = say(ascii("RFB 003.008\n"));
  at("the security types");
  const security = say([1, 1]);
  at("the security result");
  const clientInit = say(u32(0));
  at("ServerInit");
  const width = 4;
  const height = 2;
  const init = say([
    ...u16(width), ...u16(height),
    32, 24, 0, 1, ...u16(255), ...u16(255), ...u16(255), 16, 8, 0, 0, 0, 0,
    ...u32(3), ...ascii("mac"),
  ]);
  const colours = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255],
                   [10, 20, 30], [40, 50, 60], [70, 80, 90], [100, 110, 120]];
  const raw = colours.flatMap(([r, g, b]) => [r, g, b, 0]);
  at("the frame");
  const afterFrame = say([0, 0, ...u16(1), ...u16(0), ...u16(0), ...u16(width), ...u16(height), ...u32(0), ...raw]);
  const screen = target.children[0];
  const canvas = screen.children[0];
  const drawn = canvas.pixels ? Array.from(canvas.pixels["data"]) : null;
  at("the disconnect");
  viewer.disconnect();
  runTimers();
  return {
    before, version, security, clientInit, init, afterFrame, told,
    size: [canvas["width"], canvas["height"]], drawn, closes: socket.closes,
    binaryType: socket.binaryType,
  };
}"#;

/// The message types of what the client sent after `ServerInit`, read in order: SetPixelFormat
/// (0, 20 bytes), SetEncodings (2, 4 bytes and 4 per encoding) and FramebufferUpdateRequest (3,
/// 10 bytes); `None` at a byte that starts no message of those.
fn messages(bytes: &[u8]) -> Option<Vec<(u8, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let len = match bytes[at] {
            0 => 20,
            2 => {
                let count = usize::from(u16::from_be_bytes([
                    *bytes.get(at + 2)?,
                    *bytes.get(at + 3)?,
                ]));
                4 + 4 * count
            }
            3 => 10,
            _ => return None,
        };
        out.push((bytes[at], bytes.get(at..at + len)?.to_vec()));
        at += len;
    }
    Some(out)
}

fn bytes(value: &Value) -> Vec<u8> {
    value
        .as_array()
        .expect("bytes")
        .iter()
        .map(|b| u8::try_from(b.as_u64().expect("a byte")).expect("a byte"))
        .collect()
}

/// Proves: the viewer the Desktop page builds (`screenViewer` over the vendored `RFB`), given an
/// open socket and no address, speaks RFB 3.8 to the Mac from the Mac's first byte: it sends
/// nothing before the server's version, answers `RFB 003.008`, picks the one security type
/// offered (None), asks for a shared session, then sends its pixel format (32 bits, depth 24,
/// true colour), its encodings (Raw among them) and a full framebuffer update request for the
/// screen the server described; tells the page `connect`; draws a Raw rectangle on its canvas
/// pixel for pixel; asks for the next, incremental, update; and on `disconnect` closes the socket
/// and tells the page so.
///
/// Catches: a viewer that never attaches to the open socket it is handed (no answer to the
/// version), a library that needs an address, a handshake that stalls before `connect`, and a
/// frame that never reaches the canvas.
#[test]
fn the_vendored_viewer_answers_a_macs_handshake_and_draws_its_first_frame() {
    let out = run_with_novnc(SESSION);
    assert_eq!(
        out["failed"],
        Value::Null,
        "the session ran to its end: {}",
        out["stack"]
    );
    assert_eq!(out["before"], json!([]), "nothing before the server speaks");
    assert_eq!(bytes(&out["version"]), b"RFB 003.008\n");
    assert_eq!(out["security"], json!([1]));
    assert_eq!(out["clientInit"], json!([1]), "a shared session");
    assert_eq!(out["binaryType"], json!("arraybuffer"));

    let init = messages(&bytes(&out["init"])).expect("the client's messages after ServerInit");
    let kinds: Vec<u8> = init.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(kinds, [0, 2, 3]);
    assert_eq!(
        init[0].1,
        [
            0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0
        ],
        "SetPixelFormat: 32 bits, depth 24, little-endian true colour, red at 0, green at 8, blue at 16"
    );
    let encodings: Vec<i32> = init[1].1[4..]
        .chunks(4)
        .map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert!(encodings.contains(&0), "Raw is offered: {encodings:?}");
    assert_eq!(
        init[2].1,
        [3, 0, 0, 0, 0, 0, 0, 4, 0, 2],
        "a full update of 4 by 2"
    );

    assert_eq!(
        messages(&bytes(&out["afterFrame"])).map(|m| m.into_iter().map(|(_, b)| b).collect()),
        Some(vec![vec![3, 1, 0, 0, 0, 0, 0, 4, 0, 2]]),
        "the next update is incremental"
    );
    assert_eq!(out["size"], json!([4, 2]));
    let colours: [[u8; 3]; 8] = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 255],
        [10, 20, 30],
        [40, 50, 60],
        [70, 80, 90],
        [100, 110, 120],
    ];
    let want: Vec<u8> = colours
        .iter()
        .flat_map(|[r, g, b]| [*r, *g, *b, 255])
        .collect();
    assert_eq!(bytes(&out["drawn"]), want, "the frame, pixel for pixel");
    assert_eq!(out["told"], json!([["connect"], ["disconnect", true]]));
    assert_eq!(out["closes"], json!(1));
}
