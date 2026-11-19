//! The address of the app's one socket (`ui/socket_address.js`): the page's own host, over TLS
//! when the page is, and a path of a fixed shape.

mod support;

use serde_json::{Value, json};
use support::Ui;

/// What `socketAddress(location, path)` gives for each `[location, path]`: `{"address": ...}`, or
/// `{"refused": <the error's message>}` when it throws.
fn addresses(cases: &[(Value, Value)]) -> Vec<Value> {
    let input: Vec<Value> = cases
        .iter()
        .map(|(location, path)| json!({"location": location, "path": path}))
        .collect();
    let driver = format!(
        r#"
import {{ socketAddress }} from "./socket_address.js";
const cases = {};
export async function main() {{
  return cases.map((c) => {{
    try {{
      return {{ address: socketAddress(c.location, c.path) }};
    }} catch (e) {{
      return {{ refused: e.message }};
    }}
  }});
}}"#,
        json!(input)
    );
    Ui::start()
        .run(&driver)
        .as_array()
        .expect("one answer per case")
        .clone()
}

fn page() -> Value {
    json!({"protocol": "https:", "host": "coordinator.example:8443"})
}

/// A session id: 64 lower-case hex digits.
const SESSION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Proves: the Remote SSH terminal's path and a console session's stream path open on the page's
/// own host and port, over `wss:` from an https page and `ws:` from any other, the path kept as it
/// is.
///
/// Catches: an address that drops the port, names another host, or opens `ws:` from an https page.
#[test]
fn the_ssh_terminal_and_a_console_stream_open_on_the_pages_own_host() {
    let ssh = "/v1/ssh/terminal?host=node-01.lab%2Fa_b~c!*'()";
    let stream = format!("/console/v1/stream/{SESSION}");
    let plain = json!({"protocol": "http:", "host": "127.0.0.1:8080"});
    let got = addresses(&[
        (page(), json!(ssh)),
        (page(), json!(stream)),
        (plain, json!(stream)),
    ]);
    assert_eq!(
        got,
        [
            json!({"address": format!("wss://coordinator.example:8443{ssh}")}),
            json!({"address": format!("wss://coordinator.example:8443{stream}")}),
            json!({"address": format!("ws://127.0.0.1:8080{stream}")}),
        ]
    );
}

/// Proves: every path of another shape is refused before a socket is made, with the path named:
/// another prefix, a session id that is not exactly 64 lower-case hex digits, anything after the
/// id, a host id with a character `encodeURIComponent` escapes, an empty host id, and a value that
/// is not a string.
///
/// Catches: a prefix match that lets `/console/v1/stream/<id>/../x` or a query through, an id
/// check that takes upper-case, 63 or 65 digits, and a host id that carries `&`, `#`, `/` or a
/// space into the terminal's query.
#[test]
fn a_path_of_no_fixed_shape_is_refused() {
    let upper = SESSION.to_ascii_uppercase();
    let short = &SESSION[..63];
    let paths = [
        json!("/v1/hosts"),
        json!("//evil.example/console/v1/stream/".to_owned() + SESSION),
        json!("console/v1/stream/".to_owned() + SESSION),
        json!(format!("/console/v1/stream/{upper}")),
        json!(format!("/console/v1/stream/{short}")),
        json!(format!("/console/v1/stream/{SESSION}0")),
        json!(format!("/console/v1/stream/{SESSION}/")),
        json!(format!("/console/v1/stream/{SESSION}?x=1")),
        json!(format!("/console/v1/stream/{}g", &SESSION[..63])),
        json!("/console/v1/stream/"),
        json!("/v1/ssh/terminal?host="),
        json!("/v1/ssh/terminal?host=a&b=c"),
        json!("/v1/ssh/terminal?host=a#b"),
        json!("/v1/ssh/terminal?host=a/b"),
        json!("/v1/ssh/terminal?host=a b"),
        json!("/v1/ssh/terminal"),
        json!("/v1/ssh/terminal?host=a\n"),
        json!(""),
    ];
    let cases: Vec<(Value, Value)> = paths.iter().map(|p| (page(), p.clone())).collect();
    let got = addresses(&cases);
    for (path, got) in paths.iter().zip(&got) {
        let text = path.as_str().expect("a string");
        assert_eq!(
            got,
            &json!({"refused": format!("not a socket path: {text}")}),
            "{path}"
        );
    }
    let odd = addresses(&[(page(), json!(null)), (page(), json!(["/v1/hosts"]))]);
    assert_eq!(
        odd,
        [
            json!({"refused": "not a socket path: null"}),
            json!({"refused": "not a socket path: /v1/hosts"}),
        ]
    );
}
